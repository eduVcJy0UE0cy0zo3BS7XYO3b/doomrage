mod tools;

use nrepl::Client;
use serde_json::{json, Value};
use std::path::PathBuf;

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    ).init();

    let args: Vec<String> = std::env::args().collect();
    let mut project_dir = None;
    let mut prompt = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project_dir = Some(PathBuf::from(args.get(i + 1).expect("--project requires dir")));
                i += 2;
            }
            arg if !arg.starts_with('-') && prompt.is_none() => {
                prompt = Some(arg.to_string());
                i += 1;
            }
            _ => { i += 1; }
        }
    }

    let prompt = prompt.unwrap_or_else(|| {
        eprintln!("Usage: canvas-agent [--project <dir>] \"<prompt>\"");
        std::process::exit(1);
    });

    // Load .env from project dir (if exists)
    let env_file = project_dir.as_ref()
        .map(|d| d.join(".env"))
        .or_else(|| std::env::current_dir().ok().map(|d| d.join(".env")));
    if let Some(ref path) = env_file {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') { continue; }
                    if let Some((key, val)) = line.split_once('=') {
                        let key = key.trim();
                        let val = val.trim();
                        // Don't override existing env vars
                        if !val.is_empty() && std::env::var(key).is_err() {
                            std::env::set_var(key, val);
                        }
                    }
                }
            }
        }
    }

    let api_key = std::env::var("LLM_API_KEY").unwrap_or_else(|_| {
        eprintln!("Set LLM_API_KEY environment variable");
        std::process::exit(1);
    });

    // Find nREPL port
    let port = find_nrepl_port(project_dir.as_deref()).unwrap_or_else(|| {
        eprintln!("Cannot find .nrepl-port. Is peer running?");
        std::process::exit(1);
    });

    eprintln!("Connecting to nREPL on port {}...", port);
    let mut client = Client::connect(&format!("127.0.0.1:{}", port)).unwrap_or_else(|e| {
        eprintln!("Failed to connect: {}", e);
        std::process::exit(1);
    });
    let session = client.clone_session().unwrap_or_else(|e| {
        eprintln!("Failed to create session: {}", e);
        std::process::exit(1);
    });
    eprintln!("Connected. Running agent...\n");

    // Tool-use loop
    let tools = tools::tools_schema();
    let mut messages: Vec<Value> = vec![
        json!({"role": "user", "content": prompt})
    ];

    loop {
        let response = call_claude(&api_key, &messages, &tools);
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                eprintln!("API error: {}", e);
                break;
            }
        };

        let stop_reason = response["stop_reason"].as_str().unwrap_or("end_turn");
        let content = response["content"].as_array().cloned().unwrap_or_default();

        // Collect assistant message
        messages.push(json!({"role": "assistant", "content": content}));

        if stop_reason == "end_turn" {
            // Print final text
            for block in &content {
                if block["type"] == "text" {
                    println!("{}", block["text"].as_str().unwrap_or(""));
                }
            }
            break;
        }

        if stop_reason == "tool_use" {
            let mut tool_results = Vec::new();
            for block in &content {
                if block["type"] == "tool_use" {
                    let tool_id = block["id"].as_str().unwrap_or("");
                    let tool_name = block["name"].as_str().unwrap_or("");
                    let input = &block["input"];

                    eprintln!("[tool] {} {}", tool_name, serde_json::to_string(input).unwrap_or_default());
                    let result = tools::execute_tool(&mut client, &session, tool_name, input);
                    eprintln!("[result] {}", &result[..result.len().min(200)]);

                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": result,
                    }));
                }
            }
            messages.push(json!({"role": "user", "content": tool_results}));
        }
    }
}

fn find_nrepl_port(project_dir: Option<&std::path::Path>) -> Option<u16> {
    let candidates: Vec<PathBuf> = vec![
        project_dir.map(|d| d.join(".canvas").join(".nrepl-port")),
        std::env::current_dir().ok().map(|d| d.join(".canvas").join(".nrepl-port")),
        dirs::home_dir().map(|h| h.join(".canvas").join(".nrepl-port")),
    ].into_iter().flatten().collect();

    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(port) = content.trim().parse::<u16>() {
                return Some(port);
            }
        }
    }
    None
}

/// Call LLM via OpenAI-compatible API (works with litellm proxy).
/// Sends OpenAI format, parses OpenAI response, converts to our internal format.
fn call_claude(api_key: &str, messages: &[Value], tools: &Value) -> Result<Value, String> {
    let base_url = std::env::var("LLM_API_BASE").unwrap_or_else(|_| {
        eprintln!("Set LLM_API_BASE (e.g. http://localhost:4000)");
        std::process::exit(1);
    });
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| {
        eprintln!("Set LLM_MODEL (e.g. claude-sonnet)");
        std::process::exit(1);
    });

    // Convert Anthropic-style messages to OpenAI format
    let oai_messages = convert_messages_to_openai(messages);
    let oai_tools = convert_tools_to_openai(tools);

    let client = reqwest::blocking::Client::new();
    let body = json!({
        "model": model,
        "max_tokens": 4096,
        "messages": oai_messages,
        "tools": oai_tools,
    });

    let resp = client
        .post(format!("{}/v1/chat/completions", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("API {} : {}", status, &text[..text.len().min(500)]));
    }

    let oai_resp: Value = serde_json::from_str(&text)
        .map_err(|e| format!("JSON error: {} — body: {}", e, &text[..text.len().min(200)]))?;

    // Convert OpenAI response back to Anthropic-like format for our agent loop
    Ok(convert_response_from_openai(&oai_resp))
}

fn convert_messages_to_openai(messages: &[Value]) -> Vec<Value> {
    let mut result = vec![json!({"role": "system", "content": SYSTEM_PROMPT})];
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = &msg["content"];

        if role == "user" {
            if let Some(text) = content.as_str() {
                result.push(json!({"role": "user", "content": text}));
            } else if let Some(arr) = content.as_array() {
                // Tool results
                for item in arr {
                    if item["type"] == "tool_result" {
                        result.push(json!({
                            "role": "tool",
                            "tool_call_id": item["tool_use_id"],
                            "content": item["content"],
                        }));
                    }
                }
            }
        } else if role == "assistant" {
            if let Some(arr) = content.as_array() {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for item in arr {
                    match item["type"].as_str() {
                        Some("text") => {
                            text_parts.push(item["text"].as_str().unwrap_or("").to_string());
                        }
                        Some("tool_use") => {
                            tool_calls.push(json!({
                                "id": item["id"],
                                "type": "function",
                                "function": {
                                    "name": item["name"],
                                    "arguments": serde_json::to_string(&item["input"]).unwrap_or_default(),
                                }
                            }));
                        }
                        _ => {}
                    }
                }
                let mut msg = json!({"role": "assistant"});
                if !text_parts.is_empty() {
                    msg["content"] = json!(text_parts.join("\n"));
                }
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = json!(tool_calls);
                }
                result.push(msg);
            }
        }
    }
    result
}

fn convert_tools_to_openai(tools: &Value) -> Vec<Value> {
    tools.as_array().map(|arr| {
        arr.iter().map(|t| json!({
            "type": "function",
            "function": {
                "name": t["name"],
                "description": t["description"],
                "parameters": t["input_schema"],
            }
        })).collect()
    }).unwrap_or_default()
}

fn convert_response_from_openai(resp: &Value) -> Value {
    let choice = &resp["choices"][0];
    let message = &choice["message"];
    let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

    let mut content = Vec::new();

    if let Some(text) = message["content"].as_str() {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }

    if let Some(tool_calls) = message["tool_calls"].as_array() {
        for tc in tool_calls {
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tc["id"],
                "name": tc["function"]["name"],
                "input": args,
            }));
        }
    }

    let stop_reason = match finish_reason {
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    };

    json!({
        "stop_reason": stop_reason,
        "content": content,
    })
}
