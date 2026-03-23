//! Standalone P2P network node with TCP JSON Lines server.
//!
//! Usage:
//!   wasm-canvas-net [OPTIONS] [RELAY_ADDR]
//!
//! Options:
//!   --listen ADDR    TCP address to listen for clients (default: 127.0.0.1:9010)
//!   --stdio          Use stdin/stdout instead of TCP server
//!
//! Clients connect via TCP and exchange JSON Lines.
//! See `protocol::ClientMsg` and `protocol::ServerMsg` for message format.

use wasm_canvas_net::ocapn::session::SessionManager;
use wasm_canvas_net::transport::{self, NoRepaint, SharedSessionManager};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    ).init();

    let args: Vec<String> = std::env::args().collect();

    let mut listen_addr = "127.0.0.1:9010".to_string();
    let mut stdio_mode = false;
    let mut relay_addr = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                if i < args.len() {
                    listen_addr = args[i].clone();
                }
            }
            "--stdio" => {
                stdio_mode = true;
            }
            arg if !arg.starts_with('-') => {
                relay_addr = Some(arg.to_string());
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
            }
        }
        i += 1;
    }

    let session_manager: SharedSessionManager = Arc::new(Mutex::new(SessionManager::new()));
    let signal: Arc<dyn transport::RepaintSignal> = Arc::new(NoRepaint);
    let net_handle = transport::spawn_network(signal, session_manager);

    if let Some(ref addr) = relay_addr {
        log::info!("Connecting to relay: {}", addr);
        net_handle.send(transport::NetCommand::ConnectRelay { addr: addr.clone() });
    }

    if stdio_mode {
        wasm_canvas_net::stdio::run_stdio(net_handle).await;
    } else {
        let addr: std::net::SocketAddr = listen_addr.parse()
            .expect("Invalid listen address");
        wasm_canvas_net::server::run_server(addr, net_handle).await;
    }
}
