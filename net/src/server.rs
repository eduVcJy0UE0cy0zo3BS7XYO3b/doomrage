use crate::protocol::{ClientMsg, ServerMsg};
use crate::transport::{NetCommand, NetEvent, NetHandle};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

/// Run TCP server that bridges JSON Lines clients to the P2P network.
pub async fn run_server(addr: SocketAddr, net_handle: NetHandle) {
    let listener = TcpListener::bind(addr).await.expect("Failed to bind TCP listener");
    log::info!("Listening for clients on {}", addr);

    // Broadcast channel: network events → all clients
    let (event_tx, _) = broadcast::channel::<ServerMsg>(256);

    // Channel for client commands → main loop
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<NetCommand>();

    loop {
        tokio::select! {
            // Accept new clients
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        log::info!("Client connected: {}", peer_addr);
                        let event_rx = event_tx.subscribe();
                        let cmd_tx = cmd_tx.clone();
                        tokio::spawn(async move {
                            handle_client(stream, event_rx, cmd_tx, peer_addr).await;
                            log::info!("Client disconnected: {}", peer_addr);
                        });
                    }
                    Err(e) => log::warn!("Accept error: {}", e),
                }
            }
            // Forward client commands to network
            Some(cmd) = cmd_rx.recv() => {
                net_handle.send(cmd);
            }
            // Poll network events → broadcast to clients
            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                for event in net_handle.poll() {
                    let msg = match event {
                        NetEvent::LocalPeerId(id) => Some(ServerMsg::PeerId(id)),
                        NetEvent::PeerDiscovered(id) => Some(ServerMsg::PeerDiscovered(id)),
                        NetEvent::PeerLost(id) => Some(ServerMsg::PeerLost(id)),
                        NetEvent::ValuesReceived { peer, channel, values } => {
                            Some(ServerMsg::ValuesReceived { peer, channel, values })
                        }
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        let _ = event_tx.send(msg);
                    }
                }
            }
        }
    }
}

async fn handle_client(
    stream: tokio::net::TcpStream,
    mut event_rx: broadcast::Receiver<ServerMsg>,
    cmd_tx: mpsc::UnboundedSender<NetCommand>,
    peer_addr: SocketAddr,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            match serde_json::from_str::<ClientMsg>(trimmed) {
                                Ok(msg) => match msg {
                                    ClientMsg::Publish { channel, values } => {
                                        let _ = cmd_tx.send(NetCommand::Publish { channel, values });
                                    }
                                    ClientMsg::ConnectRelay { addr } => {
                                        let _ = cmd_tx.send(NetCommand::ConnectRelay { addr });
                                    }
                                    ClientMsg::DialPeer { addr } => {
                                        let _ = cmd_tx.send(NetCommand::DialPeer { addr });
                                    }
                                },
                                Err(e) => {
                                    log::warn!("Invalid message from {}: {}", peer_addr, e);
                                }
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        log::warn!("Read error from {}: {}", peer_addr, e);
                        break;
                    }
                }
            }
            Ok(msg) = event_rx.recv() => {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if writer.write_all(json.as_bytes()).await.is_err() { break; }
                    if writer.write_all(b"\n").await.is_err() { break; }
                    if writer.flush().await.is_err() { break; }
                }
            }
        }
    }
}
