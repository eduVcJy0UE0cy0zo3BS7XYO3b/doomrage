use crate::bridge::SharedSessionManager;
use crate::ocapn::session::SessionManager;
use crate::ocapn::types::OCapNMessage;
use crate::types::Value;
use libp2p::{
    futures::StreamExt,
    gossipsub, mdns, noise,
    request_response::{self, ProtocolSupport},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm,
};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

// --- Public types ---

#[derive(Debug, Clone)]
pub enum NetCommand {
    Publish {
        channel: String,
        values: HashMap<String, Value>,
    },
    OCapNSend {
        peer_id: String,
        message: OCapNMessage,
    },
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    PeerDiscovered(String),
    PeerLost(String),
    ValuesReceived {
        peer: String,
        channel: String,
        values: HashMap<String, Value>,
    },
    OCapNReceived {
        peer: String,
        message: OCapNMessage,
    },
    OCapNCallResult {
        request_id: u64,
        value: crate::ocapn::syrup::SyrupValue,
    },
    LocalPeerId(String),
}

/// Handle held by the main (eframe) thread.
pub struct NetHandle {
    cmd_tx: tokio_mpsc::UnboundedSender<NetCommand>,
    event_rx: mpsc::Receiver<NetEvent>,
}

impl NetHandle {
    pub fn send(&self, cmd: NetCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Non-blocking drain of pending events.
    pub fn poll(&self) -> Vec<NetEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }
}

// --- Wire message ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WireMessage {
    channel: String,
    values: HashMap<String, Value>,
    seq: u64,
}

// --- libp2p behaviour ---

#[derive(NetworkBehaviour)]
struct NodeBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
    ocapn_rr: request_response::cbor::Behaviour<OCapNReq, OCapNResp>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OCapNReq(pub Vec<u8>);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OCapNResp(pub Vec<u8>);

// --- Entry point: spawn network thread ---

pub fn spawn_network(ctx: egui::Context, session_manager: SharedSessionManager) -> NetHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("libp2p-net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime for network");
            rt.block_on(run_swarm(cmd_rx, event_tx, ctx, session_manager));
        })
        .expect("Failed to spawn network thread");

    NetHandle { cmd_tx, event_rx }
}

async fn run_swarm(
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<NetCommand>,
    event_tx: mpsc::Sender<NetEvent>,
    ctx: egui::Context,
    session_manager: SharedSessionManager,
) {
    // Build swarm
    let mut swarm = match build_swarm() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build libp2p swarm: {}", e);
            return;
        }
    };

    // Listen on all interfaces
    let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse().unwrap();
    if let Err(e) = swarm.listen_on(listen_addr) {
        log::error!("Failed to listen: {}", e);
        return;
    }

    let topic = gossipsub::IdentTopic::new("wasm-canvas/values");
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&topic) {
        log::error!("Failed to subscribe to gossipsub topic: {}", e);
        return;
    }

    let local_peer_id = *swarm.local_peer_id();
    log::info!("Network started. PeerId: {}", local_peer_id);
    let _ = event_tx.send(NetEvent::LocalPeerId(local_peer_id.to_string()));
    ctx.request_repaint();

    let mut seq: u64 = 0;
    // Track latest seq per (peer, channel) to discard stale
    let mut latest_seq: HashMap<(String, String), u64> = HashMap::new();

    loop {
        tokio::select! {
            // Process commands from main thread
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    NetCommand::Publish { channel, values } => {
                        seq += 1;
                        let msg = WireMessage { channel, values, seq };
                        if let Ok(data) = serde_json::to_vec(&msg) {
                            match swarm.behaviour_mut().gossipsub.publish(topic.clone(), data) {
                                Err(gossipsub::PublishError::InsufficientPeers) => {
                                    // Expected when no remote peers connected yet
                                }
                                Err(e) => log::warn!("Gossipsub publish error: {}", e),
                                Ok(_) => {}
                            }
                        }
                    }
                    NetCommand::OCapNSend { peer_id, message } => {
                        if let Ok(pid) = peer_id.parse::<PeerId>() {
                            let data = message.encode();
                            swarm.behaviour_mut().ocapn_rr.send_request(&pid, OCapNReq(data));
                        } else {
                            log::warn!("Invalid PeerId for OCapN send: {}", peer_id);
                        }
                    }
                }
            }
            // Process swarm events
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(ev)) => {
                        handle_mdns(&mut swarm, &event_tx, &ctx, ev);
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { propagation_source, message, .. }
                    )) => {
                        // Ignore our own messages
                        if propagation_source == local_peer_id {
                            continue;
                        }
                        if let Ok(wire) = serde_json::from_slice::<WireMessage>(&message.data) {
                            let key = (propagation_source.to_string(), wire.channel.clone());
                            let prev = latest_seq.get(&key).copied().unwrap_or(0);
                            if wire.seq > prev {
                                latest_seq.insert(key, wire.seq);
                                let _ = event_tx.send(NetEvent::ValuesReceived {
                                    peer: propagation_source.to_string(),
                                    channel: wire.channel,
                                    values: wire.values,
                                });
                                ctx.request_repaint();
                            }
                        }
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::OcapnRr(
                        request_response::Event::Message { peer, message, .. }
                    )) => {
                        match message {
                            request_response::Message::Request { request, channel, .. } => {
                                if let Ok(msg) = OCapNMessage::decode(&request.0) {
                                    match &msg {
                                        OCapNMessage::OpDeliver { to_desc: _, args, request_id } => {
                                            // Handle OpDeliver: deliver and respond with result
                                            let response_data = if args.len() >= 2 {
                                                if let (
                                                    crate::ocapn::syrup::SyrupValue::Symbol(method),
                                                    swiss_val,
                                                ) = (&args[0], &args[1]) {
                                                    if method == "deliver-to" {
                                                        if let Some(swiss) = crate::ocapn::types::SwissNum::from_syrup(swiss_val) {
                                                            let remaining = &args[2..];
                                                            let mgr = session_manager.lock().unwrap();
                                                            match mgr.deliver_by_swiss(&swiss, remaining) {
                                                                Ok(result) => {
                                                                    let value = result.unwrap_or(crate::ocapn::syrup::SyrupValue::Bool(false));
                                                                    let resp_msg = OCapNMessage::OpDeliverResult {
                                                                        request_id: *request_id,
                                                                        value,
                                                                    };
                                                                    resp_msg.encode()
                                                                }
                                                                Err(_) => vec![],
                                                            }
                                                        } else { vec![] }
                                                    } else { vec![] }
                                                } else { vec![] }
                                            } else { vec![] };
                                            let _ = swarm.behaviour_mut().ocapn_rr
                                                .send_response(channel, OCapNResp(response_data));
                                            // Also forward as event so main thread can log it
                                            let _ = event_tx.send(NetEvent::OCapNReceived {
                                                peer: peer.to_string(),
                                                message: msg,
                                            });
                                            ctx.request_repaint();
                                        }
                                        _ => {
                                            let _ = event_tx.send(NetEvent::OCapNReceived {
                                                peer: peer.to_string(),
                                                message: msg,
                                            });
                                            ctx.request_repaint();
                                            // Send empty ack response
                                            let _ = swarm.behaviour_mut().ocapn_rr
                                                .send_response(channel, OCapNResp(vec![]));
                                        }
                                    }
                                } else {
                                    let _ = swarm.behaviour_mut().ocapn_rr
                                        .send_response(channel, OCapNResp(vec![]));
                                }
                            }
                            request_response::Message::Response { response, .. } => {
                                // Handle OpDeliverResult responses
                                if !response.0.is_empty() {
                                    if let Ok(msg) = OCapNMessage::decode(&response.0) {
                                        if let OCapNMessage::OpDeliverResult { request_id, value } = msg {
                                            let _ = event_tx.send(NetEvent::OCapNCallResult {
                                                request_id,
                                                value,
                                            });
                                            ctx.request_repaint();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    SwarmEvent::NewListenAddr { address, .. } => {
                        log::info!("Listening on {}/p2p/{}", address, local_peer_id);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn handle_mdns(
    swarm: &mut Swarm<NodeBehaviour>,
    event_tx: &mpsc::Sender<NetEvent>,
    ctx: &egui::Context,
    event: mdns::Event,
) {
    match event {
        mdns::Event::Discovered(peers) => {
            for (peer_id, _addr) in peers {
                log::info!("mDNS discovered: {}", peer_id);
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .add_explicit_peer(&peer_id);
                let _ = event_tx.send(NetEvent::PeerDiscovered(peer_id.to_string()));
                ctx.request_repaint();
            }
        }
        mdns::Event::Expired(peers) => {
            for (peer_id, _addr) in peers {
                log::info!("mDNS expired: {}", peer_id);
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .remove_explicit_peer(&peer_id);
                let _ = event_tx.send(NetEvent::PeerLost(peer_id.to_string()));
                ctx.request_repaint();
            }
        }
    }
}

fn build_swarm() -> Result<Swarm<NodeBehaviour>, Box<dyn std::error::Error>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(1))
                .validation_mode(gossipsub::ValidationMode::Permissive)
                .max_transmit_size(256 * 1024) // 256 KB
                .build()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns = mdns::tokio::Behaviour::new(
                mdns::Config::default(),
                key.public().to_peer_id(),
            )?;

            let ocapn_rr = request_response::cbor::Behaviour::new(
                [(StreamProtocol::new("/ocapn/1"), ProtocolSupport::Full)],
                request_response::Config::default(),
            );

            Ok(NodeBehaviour { gossipsub, mdns, ocapn_rr })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}
