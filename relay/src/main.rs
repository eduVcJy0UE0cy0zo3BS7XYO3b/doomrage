use clap::Parser;
use libp2p::{
    futures::StreamExt,
    gossipsub, identify, noise, relay, rendezvous,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr,
};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "relay", about = "libp2p relay server for wasm-canvas")]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "4001")]
    port: u16,
}

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    rendezvous: rendezvous::server::Behaviour,
    gossipsub: gossipsub::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let relay = relay::Behaviour::new(
                key.public().to_peer_id(),
                relay::Config::default(),
            );
            let identify = identify::Behaviour::new(
                identify::Config::new("/wasm-canvas/1.0.0".to_string(), key.public()),
            );
            let rendezvous = rendezvous::server::Behaviour::new(
                rendezvous::server::Config::default(),
            );
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
            Ok(RelayBehaviour { relay, identify, rendezvous, gossipsub })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(300)))
        .build();

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", args.port).parse()?;
    swarm.listen_on(listen_addr)?;

    // Subscribe to gossipsub topic so we relay messages between peers
    let topic = gossipsub::IdentTopic::new("wasm-canvas/values");
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    let local_peer_id = *swarm.local_peer_id();
    log::info!("Relay server starting...");
    log::info!("PeerId: {}", local_peer_id);

    // Detect external IP
    let external_ip = match reqwest::get("https://api.ipify.org").await {
        Ok(resp) => resp.text().await.ok(),
        Err(_) => None,
    };

    let mut listening = false;

    loop {
        let event = swarm.select_next_some().await;
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                if !listening {
                    println!();
                    println!("  Relay is running!");
                    println!();
                    if let Some(ref ip) = external_ip {
                        println!("  Connect with:");
                        println!("    /ip4/{}/tcp/{}/p2p/{}", ip.trim(), args.port, local_peer_id);
                        println!();
                    } else {
                        println!("  Local address:");
                        println!("    {}/p2p/{}", address, local_peer_id);
                        println!("  (could not detect external IP)");
                        println!();
                    }
                    listening = true;
                }
                log::info!("Listening on {}/p2p/{}", address, local_peer_id);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) => {
                log::info!("Reservation accepted from {}", src_peer_id);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
                relay::Event::CircuitReqAccepted { src_peer_id, dst_peer_id, .. },
            )) => {
                log::info!("Circuit {} -> {}", src_peer_id, dst_peer_id);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Rendezvous(
                rendezvous::server::Event::PeerRegistered { peer, registration },
            )) => {
                log::info!("Rendezvous: {} registered in namespace '{}'",
                    peer, registration.namespace);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Rendezvous(
                rendezvous::server::Event::DiscoverServed { enquirer, registrations },
            )) => {
                log::info!("Rendezvous: served {} registrations to {}",
                    registrations.len(), enquirer);
            }
            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                let addr = match &endpoint {
                    libp2p::core::ConnectedPoint::Dialer { address, .. } => address.to_string(),
                    libp2p::core::ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr.to_string(),
                };
                log::info!("+ {} from {}", peer_id, addr);
                // Add peer to gossipsub mesh
                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                log::info!("- {}", peer_id);
            }
            _ => {}
        }
    }
}
