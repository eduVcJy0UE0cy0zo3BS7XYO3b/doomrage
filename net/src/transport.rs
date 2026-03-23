use crate::ocapn::session::SessionManager;
use crate::ocapn::types::OCapNMessage;
use crate::protocol::Value;
use crate::protocol::WireMessage;
use libp2p::{
    dcutr, futures::StreamExt,
    gossipsub, identify, mdns, noise, relay, rendezvous,
    request_response::{self, ProtocolSupport},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm,
};
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

// --- RepaintSignal ---

/// Abstraction so network can signal the UI without depending on egui.
pub trait RepaintSignal: Send + Sync + 'static {
    fn request_repaint(&self);
}

/// No-op signal for headless mode.
#[derive(Clone)]
pub struct NoRepaint;

impl RepaintSignal for NoRepaint {
    fn request_repaint(&self) {}
}

// --- Shared types ---

pub type SharedSessionManager = Arc<Mutex<SessionManager>>;

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
    ConnectRelay {
        addr: String,
    },
    DialPeer {
        addr: String,
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

/// Handle held by the main thread.
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

// --- libp2p behaviour ---

#[derive(NetworkBehaviour)]
struct NodeBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
    ocapn_rr: request_response::cbor::Behaviour<OCapNReq, OCapNResp>,
    relay_client: relay::client::Behaviour,
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
    rendezvous: rendezvous::client::Behaviour,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OCapNReq(pub Vec<u8>);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OCapNResp(pub Vec<u8>);

// --- Entry point ---

pub fn spawn_network(ctx: Arc<dyn RepaintSignal>, session_manager: SharedSessionManager) -> NetHandle {
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
    ctx: Arc<dyn RepaintSignal>,
    session_manager: SharedSessionManager,
) {
    let mut swarm = match build_swarm() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build libp2p swarm: {}", e);
            return;
        }
    };

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
    let mut latest_seq: HashMap<(String, String), u64> = HashMap::new();
    // Pending relay circuit listeners: relay PeerId → relay Multiaddr
    let mut pending_relay_circuits: HashMap<PeerId, Multiaddr> = HashMap::new();
    // Rendezvous: relay peers to register/discover with
    let mut rendezvous_peers: HashMap<PeerId, Multiaddr> = HashMap::new();
    let rendezvous_ns = rendezvous::Namespace::from_static("wasm-canvas");

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    NetCommand::Publish { channel, values } => {
                        seq += 1;
                        let msg = WireMessage { channel, values, seq };
                        if let Ok(data) = serde_json::to_vec(&msg) {
                            match swarm.behaviour_mut().gossipsub.publish(topic.clone(), data) {
                                Err(gossipsub::PublishError::InsufficientPeers) => {}
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
                    NetCommand::ConnectRelay { addr } => {
                        match addr.parse::<Multiaddr>() {
                            Ok(maddr) => {
                                log::info!("Connecting to relay: {}", maddr);
                                // Extract relay PeerId from multiaddr
                                let relay_peer = maddr.iter().find_map(|p| {
                                    if let libp2p::multiaddr::Protocol::P2p(pid) = p {
                                        Some(pid)
                                    } else {
                                        None
                                    }
                                });
                                if let Some(relay_pid) = relay_peer {
                                    rendezvous_peers.insert(relay_pid, maddr.clone());
                                    // Check if already connected (e.g. via mDNS)
                                    if swarm.is_connected(&relay_pid) {
                                        log::info!("Already connected to relay, setting up circuit");
                                        let circuit_addr = maddr.clone()
                                            .with(libp2p::multiaddr::Protocol::P2pCircuit)
                                            .with(libp2p::multiaddr::Protocol::P2p(local_peer_id));
                                        let relay_listen = maddr.with(libp2p::multiaddr::Protocol::P2pCircuit);
                                        let _ = swarm.listen_on(relay_listen);
                                        swarm.add_external_address(circuit_addr);
                                        // Register + discover
                                        if let Err(e) = swarm.behaviour_mut().rendezvous.register(
                                            rendezvous_ns.clone(), relay_pid, None,
                                        ) {
                                            log::warn!("Rendezvous register failed: {}", e);
                                        }
                                        swarm.behaviour_mut().rendezvous.discover(
                                            Some(rendezvous_ns.clone()), None, None, relay_pid,
                                        );
                                    } else if let Err(e) = swarm.dial(maddr.clone()) {
                                        log::error!("Failed to dial relay: {}", e);
                                    } else {
                                        pending_relay_circuits.insert(relay_pid, maddr);
                                    }
                                } else if let Err(e) = swarm.dial(maddr.clone()) {
                                    log::error!("Failed to dial relay: {}", e);
                                }
                            }
                            Err(e) => log::error!("Invalid relay address '{}': {}", addr, e),
                        }
                    }
                    NetCommand::DialPeer { addr } => {
                        match addr.parse::<Multiaddr>() {
                            Ok(maddr) => {
                                log::info!("Dialing peer: {}", maddr);
                                if let Err(e) = swarm.dial(maddr) {
                                    log::error!("Failed to dial peer: {}", e);
                                }
                            }
                            Err(e) => log::error!("Invalid peer address '{}': {}", addr, e),
                        }
                    }
                }
            }
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(ev)) => {
                        handle_mdns(&mut swarm, &event_tx, &*ctx, ev);
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { propagation_source, message, .. }
                    )) => {
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
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(
                        identify::Event::Received { peer_id, .. },
                    )) => {
                        if peer_id == local_peer_id { continue; }
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                        let _ = event_tx.send(NetEvent::PeerDiscovered(peer_id.to_string()));
                        ctx.request_repaint();
                        log::info!("Identified peer via relay: {}", peer_id);
                        // Register + discover via rendezvous when we identify the relay
                        if rendezvous_peers.contains_key(&peer_id) {
                            log::info!("Registering with rendezvous on {}", peer_id);
                            if let Err(e) = swarm.behaviour_mut().rendezvous.register(
                                rendezvous_ns.clone(), peer_id, None,
                            ) {
                                log::warn!("Rendezvous register failed: {}", e);
                            }
                        }
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Dcutr(
                        dcutr::Event { remote_peer_id, result },
                    )) => {
                        match result {
                            Ok(_) => log::info!("DCUtR hole-punch success with {}", remote_peer_id),
                            Err(e) => log::warn!("DCUtR hole-punch failed with {}: {}", remote_peer_id, e),
                        }
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        log::info!("Connection established with {}", peer_id);
                        // If this is a relay we were waiting for, start circuit listener
                        if let Some(relay_maddr) = pending_relay_circuits.remove(&peer_id) {
                            let circuit_addr = relay_maddr.clone()
                                .with(libp2p::multiaddr::Protocol::P2pCircuit)
                                .with(libp2p::multiaddr::Protocol::P2p(local_peer_id));
                            log::info!("Relay connected, listening on circuit");
                            let relay_listen = relay_maddr.with(libp2p::multiaddr::Protocol::P2pCircuit);
                            let _ = swarm.listen_on(relay_listen);
                            // Add circuit address as external so rendezvous can register
                            swarm.add_external_address(circuit_addr);
                        }
                        // Discover via rendezvous immediately
                        if rendezvous_peers.contains_key(&peer_id) {
                            swarm.behaviour_mut().rendezvous.discover(
                                Some(rendezvous_ns.clone()), None, None, peer_id,
                            );
                        }
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Rendezvous(
                        rendezvous::client::Event::Registered { rendezvous_node, .. },
                    )) => {
                        log::info!("Registered with rendezvous on {}", rendezvous_node);
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Rendezvous(
                        rendezvous::client::Event::Discovered { registrations, rendezvous_node, .. },
                    )) => {
                        for registration in registrations {
                            let peer = registration.record.peer_id();
                            if peer == local_peer_id { continue; }
                            log::info!("Rendezvous discovered peer: {}", peer);
                            // Dial via relay circuit
                            if let Some(relay_maddr) = rendezvous_peers.get(&rendezvous_node) {
                                let circuit_addr = relay_maddr.clone()
                                    .with(libp2p::multiaddr::Protocol::P2pCircuit)
                                    .with(libp2p::multiaddr::Protocol::P2p(peer));
                                log::info!("Dialing discovered peer via circuit: {}", circuit_addr);
                                if let Err(e) = swarm.dial(circuit_addr) {
                                    log::warn!("Failed to dial discovered peer {}: {}", peer, e);
                                }
                            }
                        }
                    }
                    SwarmEvent::Behaviour(NodeBehaviourEvent::Rendezvous(
                        rendezvous::client::Event::RegisterFailed { error, .. },
                    )) => {
                        log::warn!("Rendezvous register failed: {:?}", error);
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
    ctx: &dyn RepaintSignal,
    event: mdns::Event,
) {
    match event {
        mdns::Event::Discovered(peers) => {
            for (peer_id, _addr) in peers {
                log::info!("mDNS discovered: {}", peer_id);
                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                let _ = event_tx.send(NetEvent::PeerDiscovered(peer_id.to_string()));
                ctx.request_repaint();
            }
        }
        mdns::Event::Expired(peers) => {
            for (peer_id, _addr) in peers {
                log::info!("mDNS expired: {}", peer_id);
                swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                let _ = event_tx.send(NetEvent::PeerLost(peer_id.to_string()));
                ctx.request_repaint();
            }
        }
    }
}

pub fn build_swarm() -> Result<Swarm<NodeBehaviour>, Box<dyn std::error::Error>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay_client| {
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(1))
                .validation_mode(gossipsub::ValidationMode::Permissive)
                .max_transmit_size(256 * 1024)
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

            let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());

            let identify = identify::Behaviour::new(
                identify::Config::new("/wasm-canvas/1.0.0".to_string(), key.public()),
            );

            let rendezvous = rendezvous::client::Behaviour::new(key.clone());

            Ok(NodeBehaviour { gossipsub, mdns, ocapn_rr, relay_client, dcutr, identify, rendezvous })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    Ok(swarm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::futures::StreamExt;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestPair {
        swarm_a: Swarm<NodeBehaviour>,
        swarm_b: Swarm<NodeBehaviour>,
        topic: gossipsub::IdentTopic,
        peer_a: PeerId,
        peer_b: PeerId,
    }

    async fn setup_pair() -> TestPair {
        let mut swarm_a = build_swarm().expect("swarm A");
        let mut swarm_b = build_swarm().expect("swarm B");

        let topic = gossipsub::IdentTopic::new("wasm-canvas/values");
        swarm_a.behaviour_mut().gossipsub.subscribe(&topic).unwrap();
        swarm_b.behaviour_mut().gossipsub.subscribe(&topic).unwrap();

        swarm_a.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        swarm_b.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();

        let peer_a = *swarm_a.local_peer_id();
        let peer_b = *swarm_b.local_peer_id();

        let mut a_found_b = false;
        let mut b_found_a = false;
        let mut mesh_ready = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        while !mesh_ready && tokio::time::Instant::now() < deadline {
            tokio::select! {
                event = swarm_a.select_next_some() => {
                    if let SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) = event {
                        for (pid, _) in peers {
                            swarm_a.behaviour_mut().gossipsub.add_explicit_peer(&pid);
                            if pid == peer_b { a_found_b = true; }
                        }
                    }
                }
                event = swarm_b.select_next_some() => {
                    if let SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) = event {
                        for (pid, _) in peers {
                            swarm_b.behaviour_mut().gossipsub.add_explicit_peer(&pid);
                            if pid == peer_a { b_found_a = true; }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if a_found_b && b_found_a {
                        let probe = serde_json::to_vec(&WireMessage {
                            channel: "__probe".into(), values: HashMap::new(), seq: 0,
                        }).unwrap();
                        if swarm_a.behaviour_mut().gossipsub.publish(topic.clone(), probe).is_ok() {
                            mesh_ready = true;
                        }
                    }
                }
            }
        }
        assert!(mesh_ready, "Gossipsub mesh did not form within timeout");

        for _ in 0..10 {
            tokio::select! {
                _ = swarm_a.select_next_some() => {}
                _ = swarm_b.select_next_some() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }

        TestPair { swarm_a, swarm_b, topic, peer_a, peer_b }
    }

    fn val_f64(v: &Value) -> f64 {
        match v { Value::F64(f) => *f, _ => panic!("expected F64") }
    }

    #[tokio::test]
    async fn test_two_peers_exchange_values() {
        let _lock = SERIAL.lock().unwrap();
        let _ = env_logger::try_init();
        let mut p = setup_pair().await;

        let wire = WireMessage {
            channel: "controls".to_string(),
            values: HashMap::from([
                ("gain".to_string(), Value::F64(42.0)),
                ("freq".to_string(), Value::F64(7.5)),
            ]),
            seq: 1,
        };
        p.swarm_a.behaviour_mut().gossipsub
            .publish(p.topic.clone(), serde_json::to_vec(&wire).unwrap()).unwrap();

        let mut received: Option<WireMessage> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while received.is_none() && tokio::time::Instant::now() < deadline {
            tokio::select! {
                event = p.swarm_b.select_next_some() => {
                    if let SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. }
                    )) = event {
                        if let Ok(w) = serde_json::from_slice::<WireMessage>(&message.data) {
                            if w.channel != "__probe" { received = Some(w); }
                        }
                    }
                }
                _ = p.swarm_a.select_next_some() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }

        let msg = received.expect("B did not receive values from A");
        assert_eq!(msg.channel, "controls");
        assert_eq!(val_f64(msg.values.get("gain").unwrap()), 42.0);
    }
}
