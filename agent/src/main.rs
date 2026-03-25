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

fn call_claude(api_key: &str, messages: &[Value], tools: &Value) -> Result<Value, String> {
    let base_url = std::env::var("LLM_API_BASE").unwrap_or_else(|_| {
        eprintln!("Set LLM_API_BASE (e.g. http://localhost:4000)");
        std::process::exit(1);
    });
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| {
        eprintln!("Set LLM_MODEL (e.g. anthropic/claude-sonnet-4-20250514)");
        std::process::exit(1);
    });

    let client = reqwest::blocking::Client::new();
    let body = json!({
        "model": model,
        "max_tokens": 4096,
        "system": SYSTEM_PROMPT,
        "tools": tools,
        "messages": messages,
    });

    let resp = client
        .post(format!("{}/v1/messages", base_url))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("API {} : {}", status, &text[..text.len().min(500)]));
    }

    serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))
}
