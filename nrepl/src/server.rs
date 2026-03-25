use crate::bencode;
use crate::session::{Evaluator, SessionManager, handle_message};
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::sync::Arc;
use std::thread;

/// A running nREPL server.
pub struct Server {
    addr: SocketAddr,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// Start an nREPL server on the given address (e.g. "127.0.0.1:0" for random port).
    pub fn start(addr: &str, evaluator: Arc<dyn Evaluator>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        listener.set_nonblocking(true)?;

        let sessions = Arc::new(SessionManager::new());

        let handle = thread::spawn(move || {
            loop {
                if shutdown_clone.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, peer)) => {
                        log::info!("nREPL client connected: {}", peer);
                        let sessions = sessions.clone();
                        let evaluator = evaluator.clone();
                        thread::spawn(move || {
                            handle_client(stream, &sessions, evaluator.as_ref());
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(e) => {
                        log::error!("nREPL accept error: {}", e);
                        break;
                    }
                }
            }
        });

        log::info!("nREPL server listening on {}", local_addr);

        Ok(Server {
            addr: local_addr,
            shutdown,
            handle: Some(handle),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Write .nrepl-port file at the given directory.
    pub fn write_port_file(&self, dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        let path = dir.join(".nrepl-port");
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, self.port().to_string())?;
        Ok(path)
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // Connect to unblock the accept loop
            let _ = TcpStream::connect(self.addr);
            let _ = handle.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle_client(stream: TcpStream, sessions: &SessionManager, evaluator: &dyn Evaluator) {
    let peer = stream.peer_addr().ok();
    stream.set_nonblocking(false).ok();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    loop {
        let msg = match bencode::decode(&mut reader) {
            Ok(msg) => msg,
            Err(bencode::DecodeError::UnexpectedEof) => break,
            Err(e) => {
                log::warn!("nREPL decode error from {:?}: {}", peer, e);
                break;
            }
        };

        let responses = handle_message(&msg, sessions, evaluator);

        for resp in &responses {
            let data = bencode::encode_to_vec(resp);
            if writer.write_all(&data).is_err() {
                return;
            }
        }
        if writer.flush().is_err() {
            return;
        }
    }

    log::info!("nREPL client disconnected: {:?}", peer);
}
