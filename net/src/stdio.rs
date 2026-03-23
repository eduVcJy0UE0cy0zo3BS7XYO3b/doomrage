use crate::protocol::{StdioIn, StdioOut};
use crate::transport::{NetCommand, NetEvent, NetHandle};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Run the stdio bridge: reads JSON Lines from stdin, writes events to stdout.
pub async fn run_stdio(net_handle: NetHandle) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            match serde_json::from_str::<StdioIn>(trimmed) {
                                Ok(msg) => match msg {
                                    StdioIn::Publish { channel, values } => {
                                        net_handle.send(NetCommand::Publish { channel, values });
                                    }
                                    StdioIn::ConnectRelay { addr } => {
                                        net_handle.send(NetCommand::ConnectRelay { addr });
                                    }
                                    StdioIn::DialPeer { addr } => {
                                        net_handle.send(NetCommand::DialPeer { addr });
                                    }
                                },
                                Err(e) => {
                                    log::warn!("Invalid stdin message: {}", e);
                                }
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        log::error!("stdin read error: {}", e);
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                for event in net_handle.poll() {
                    let out = match event {
                        NetEvent::LocalPeerId(id) => Some(StdioOut::PeerId(id)),
                        NetEvent::PeerDiscovered(id) => Some(StdioOut::PeerDiscovered(id)),
                        NetEvent::PeerLost(id) => Some(StdioOut::PeerLost(id)),
                        NetEvent::ValuesReceived { peer, channel, values } => {
                            Some(StdioOut::ValuesReceived { peer, channel, values })
                        }
                        _ => None, // OCapN events not exposed via stdio
                    };
                    if let Some(out) = out {
                        if let Ok(json) = serde_json::to_string(&out) {
                            let _ = stdout.write_all(json.as_bytes()).await;
                            let _ = stdout.write_all(b"\n").await;
                            let _ = stdout.flush().await;
                        }
                    }
                }
            }
        }
    }
}
