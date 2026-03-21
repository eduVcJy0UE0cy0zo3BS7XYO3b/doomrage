use crate::types::Value;
use libp2p::{
    futures::StreamExt,
    gossipsub, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, Swarm,
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
}

// --- Entry point: spawn network thread ---

pub fn spawn_network(ctx: egui::Context) -> NetHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("libp2p-net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime for network");
            rt.block_on(run_swarm(cmd_rx, event_tx, ctx));
        })
        .expect("Failed to spawn network thread");

    NetHandle { cmd_tx, event_rx }
}

async fn run_swarm(
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<NetCommand>,
    event_tx: mpsc::Sender<NetEvent>,
    ctx: egui::Context,
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

            Ok(NodeBehaviour { gossipsub, mdns })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}
