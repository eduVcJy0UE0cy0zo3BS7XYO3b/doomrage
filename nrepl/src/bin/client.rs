use nrepl::Client;
use std::io::{self, BufRead, Write};

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| {
        // Try cwd/.canvas/.nrepl-port first, then ~/.canvas/.nrepl-port
        let candidates = vec![
            std::env::current_dir().ok().map(|d| d.join(".canvas").join(".nrepl-port")),
            dirs::home_dir().map(|h| h.join(".canvas").join(".nrepl-port")),
        ];
        for candidate in candidates.into_iter().flatten() {
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                return format!("127.0.0.1:{}", content.trim());
            }
        }
        "127.0.0.1:7888".to_string()
    });

    eprintln!("Connecting to nREPL at {}...", addr);
    let mut client = match Client::connect(&addr) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect: {}", e);
            std::process::exit(1);
        }
    };

    let session = match client.clone_session() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to create session: {}", e);
            std::process::exit(1);
        }
    };
    eprintln!("Session: {}", session);
    eprintln!("Type Scheme expressions. Ctrl+D to quit.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    loop {
        print!("nrepl> ");
        stdout.flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }

        let code = line.trim();
        if code.is_empty() { continue; }

        match client.eval(&session, code) {
            Ok(responses) => {
                for resp in &responses {
                    if let Some(out) = resp.get_str("out") {
                        print!("{}", out);
                    }
                    if let Some(err) = resp.get_str("err") {
                        eprint!("{}", err);
                    }
                    if let Some(val) = resp.get_str("value") {
                        println!("{}", val);
                    }
                    if let Some(ex) = resp.get_str("ex") {
                        eprintln!("Error: {}", ex);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }
    eprintln!("\nBye.");
}
