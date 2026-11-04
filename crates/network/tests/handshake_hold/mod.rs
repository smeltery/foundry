//! Loopback UDP relay that stalls one QUIC handshake while the server side is
//! in progress, so tests can hold it across an accept window or a wakeup.
use iroh::EndpointAddr;
use std::net::SocketAddr;
use tokio::{net::UdpSocket, sync::watch, task::JoinHandle};

/// The code listener and daemon router both accept in one-second windows.
pub const ACCEPT_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

pub struct HandshakeHold {
    address: SocketAddr,
    stalled: watch::Receiver<bool>,
    release: watch::Sender<bool>,
    relay: JoinHandle<()>,
}

impl HandshakeHold {
    /// Forward client datagrams until the server first answers, then hold them
    /// until `release`. A QUIC server sends nothing for an incoming connection
    /// until its owner accepts it, so the first answer proves the owner has the
    /// connection and its handshake is waiting on the held client flight.
    pub async fn start(server: &EndpointAddr) -> Self {
        let target = *server.ip_addrs().next().expect("direct server address");
        let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        back.connect(target).await.unwrap();
        let address = front.local_addr().unwrap();
        let (answered, stalled) = watch::channel(false);
        let (release, mut released) = watch::channel(false);
        let relay = tokio::spawn(async move {
            let (mut client, mut held) = (None, Vec::new());
            let (mut up, mut down) = (vec![0; 65_536], vec![0; 65_536]);
            loop {
                tokio::select! {
                    received = front.recv_from(&mut up) => {
                        let Ok((length, from)) = received else { continue };
                        client = Some(from);
                        if *answered.borrow() && !*released.borrow() {
                            held.push(up[..length].to_vec());
                        } else {
                            let _ = back.send(&up[..length]).await;
                        }
                    }
                    received = back.recv(&mut down) => {
                        let Ok(length) = received else { continue };
                        answered.send_replace(true);
                        if let Some(client) = client {
                            let _ = front.send_to(&down[..length], client).await;
                        }
                    }
                    changed = released.changed(), if !*released.borrow() => {
                        if changed.is_err() { return; }
                        for datagram in held.drain(..) {
                            let _ = back.send(&datagram).await;
                        }
                    }
                }
            }
        });
        Self {
            address,
            stalled,
            release,
            relay,
        }
    }

    /// The server's identity reached through this relay only.
    pub fn address(&self, server: &EndpointAddr) -> EndpointAddr {
        EndpointAddr::new(server.id).with_ip_addr(self.address)
    }

    /// Resolve once the server owner is mid-handshake on the held connection.
    pub async fn stalled(&self) {
        self.stalled.clone().wait_for(|s| *s).await.unwrap();
    }

    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

impl Drop for HandshakeHold {
    fn drop(&mut self) {
        self.relay.abort();
    }
}
