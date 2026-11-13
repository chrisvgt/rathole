//! The two halves of tunnelling UDP through a stream, shared by both ends.
//!
//! * [`serve_udp_socket`] sits at the end that has the visitors: it owns the
//!   listening socket and frames every datagram, together with the visitor's
//!   address, into the stream. The server does this for forward services, the
//!   client for reverse services.
//! * [`forward_udp`] sits at the end that has the service: it gives every
//!   visitor address a connected socket to the service and sends the answers
//!   back through the stream. The client does this for forward services, the
//!   server for reverse services.

use crate::constants::{UDP_BUFFER_SIZE, UDP_SENDQ_SIZE, UDP_TIMEOUT};
use crate::helper::udp_connect;
use crate::protocol::UdpTraffic;
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tracing::{debug, error, instrument, trace, warn};

/// Moves datagrams between the listening socket `l` and `conn` until one of
/// them fails. The stream is never closed by this function
pub async fn serve_udp_socket<S>(l: &UdpSocket, conn: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = [0u8; UDP_BUFFER_SIZE];
    loop {
        tokio::select! {
            // Forward inbound traffic through the tunnel
            val = l.recv_from(&mut buf) => {
                let (n, from) = val?;
                UdpTraffic::write_slice(conn, from, &buf[..n]).await?;
                conn.flush().await?;
            },

            // Forward the answers from the tunnel to the visitor
            hdr_len = conn.read_u8() => {
                let t = UdpTraffic::read(conn, hdr_len?).await?;
                l.send_to(&t.data, t.from).await?;
            }
        }
    }
}

/// Aborts the task when dropped
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// Things get a little tricker when it gets to UDP because it's connection-less.
// A UdpPortMap must be maintained for recent seen incoming address, giving them
// each a local port, which is associated with a socket. So just the sender
// to the socket will work fine for the map's value.
type UdpPortMap = Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Bytes>>>>;

/// Forwards the datagrams that arrive through `conn` to `local_addr`, one
/// socket per visitor address, and sends the answers back
pub async fn forward_udp<S>(
    conn: S,
    local_addr: &str,
    prefer_ipv6: bool,
    max_forwarders: usize,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    debug!("New data channel starts forwarding");

    let port_map: UdpPortMap = Arc::new(RwLock::new(HashMap::new()));

    // The channel stores UdpTraffic that needs to be sent back through the tunnel
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<UdpTraffic>(UDP_SENDQ_SIZE);

    // FIXME: https://github.com/tokio-rs/tls/issues/40
    // Maybe this is our concern
    let (mut rd, mut wr) = io::split(conn);

    // Keep sending items from the outbound channel through the tunnel. The
    // forwarders hold the sender for up to `UDP_TIMEOUT`, so this task would
    // keep the tunnel open after the reading side is gone. Stop it with this
    // function
    let _writer = AbortOnDrop(tokio::spawn(async move {
        while let Some(t) = outbound_rx.recv().await {
            trace!("outbound {:?}", t);
            let res = async {
                t.write(&mut wr).await?;
                // Encrypting transports may keep the data until the next flush
                wr.flush().await?;
                anyhow::Ok(())
            }
            .await
            .with_context(|| "Failed to forward UDP traffic through the tunnel");
            if let Err(e) = res {
                debug!("{:?}", e);
                break;
            }
        }
    }));

    // Whether the forwarder limit was reported since it was last reached
    let mut limit_reported = false;

    loop {
        // Read a packet from the tunnel
        let hdr_len = rd.read_u8().await?;
        let packet = UdpTraffic::read(&mut rd, hdr_len)
            .await
            .with_context(|| "Failed to read UDPTraffic from the tunnel")?;
        let m = port_map.read().await;

        if m.get(&packet.from).is_none() {
            // This packet is from a address we don't see for a while,
            // which is not in the UdpPortMap.
            // So set up a mapping (and a forwarder) for it

            // Every visitor address costs a local socket, and visitor addresses
            // can be spoofed. Drop packets from new visitors once the limit is reached
            if m.len() >= max_forwarders {
                if !limit_reported {
                    warn!(
                        "Too many UDP visitors ({}). Dropping packets from new visitors",
                        max_forwarders
                    );
                    limit_reported = true;
                }
                continue;
            }

            // Drop the reader lock
            drop(m);

            // Connect before grabbing the writer lock, so the forwarders
            // are not blocked while resolving `local_addr`
            let s = udp_connect(local_addr, prefer_ipv6).await;

            // Grab the writer lock
            // This is the only thread that will try to grab the writer lock
            // So no need to worry about some other thread has already set up
            // the mapping between the gap of dropping the reader lock and
            // grabbing the writer lock
            let mut m = port_map.write().await;

            match s {
                Ok(s) => {
                    limit_reported = false;
                    let (inbound_tx, inbound_rx) = mpsc::channel(UDP_SENDQ_SIZE);
                    m.insert(packet.from, inbound_tx);
                    tokio::spawn(run_udp_forwarder(
                        s,
                        inbound_rx,
                        outbound_tx.clone(),
                        packet.from,
                        port_map.clone(),
                    ));
                }
                Err(e) => {
                    error!("{:#}", e);
                }
            }
        }

        // Now there should be a udp forwarder that can receive the packet
        let m = port_map.read().await;
        if let Some(tx) = m.get(&packet.from) {
            let _ = tx.send(packet.data).await;
        }
    }
}

// Run a UdpSocket for the visitor `from`
#[instrument(skip_all, fields(from))]
async fn run_udp_forwarder(
    s: UdpSocket,
    mut inbound_rx: mpsc::Receiver<Bytes>,
    outbount_tx: mpsc::Sender<UdpTraffic>,
    from: SocketAddr,
    port_map: UdpPortMap,
) -> Result<()> {
    debug!("Forwarder created");
    let mut buf = BytesMut::new();
    buf.resize(UDP_BUFFER_SIZE, 0);

    loop {
        tokio::select! {
            // Receive from the tunnel
            data = inbound_rx.recv() => {
                if let Some(data) = data {
                    s.send(&data).await?;
                } else {
                    break;
                }
            },

            // Receive from the service
            val = s.recv(&mut buf) => {
                let len = match val {
                    Ok(v) => v,
                    Err(_) => break
                };

                let t = UdpTraffic{
                    from,
                    data: Bytes::copy_from_slice(&buf[..len])
                };

                outbount_tx.send(t).await?;
            },

            // No traffic for the duration of UDP_TIMEOUT, clean up the state
            _ = time::sleep(Duration::from_secs(UDP_TIMEOUT)) => {
                break;
            }
        }
    }

    let mut port_map = port_map.write().await;
    port_map.remove(&from);

    debug!("Forwarder dropped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn test_udp_forwarder_limit() -> Result<()> {
        // The local service
        let service = UdpSocket::bind("127.0.0.1:0").await?;
        let local_addr = service.local_addr()?.to_string();

        // The tunnel. The other end writes to `server`
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let conn = TcpStream::connect(l.local_addr()?).await?;
        let (mut server, _) = l.accept().await?;
        tokio::spawn(async move {
            let _ = forward_udp(conn, &local_addr, false, 2).await;
        });

        // Three visitors, only two of them get a local socket
        for port in 1..=3u16 {
            let from: SocketAddr = format!("192.0.2.1:{port}").parse()?;
            UdpTraffic::write_slice(&mut server, from, b"x").await?;
        }

        let mut sockets = HashSet::new();
        let mut buf = [0u8; 16];
        while let Ok(v) =
            time::timeout(Duration::from_millis(500), service.recv_from(&mut buf)).await
        {
            sockets.insert(v?.1);
        }
        assert_eq!(sockets.len(), 2);
        Ok(())
    }

    // Both halves together: a visitor reaches the service and gets the answer
    #[tokio::test]
    async fn test_udp_round_trip() -> Result<()> {
        // The service echoes
        let service = UdpSocket::bind("127.0.0.1:0").await?;
        let local_addr = service.local_addr()?.to_string();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            while let Ok((n, from)) = service.recv_from(&mut buf).await {
                let _ = service.send_to(&buf[..n], from).await;
            }
        });

        let l = TcpListener::bind("127.0.0.1:0").await?;
        let a = TcpStream::connect(l.local_addr()?).await?;
        let (b, _) = l.accept().await?;
        tokio::spawn(async move {
            let _ = forward_udp(a, &local_addr, false, 8).await;
        });

        let listen = UdpSocket::bind("127.0.0.1:0").await?;
        let listen_addr = listen.local_addr()?;
        tokio::spawn(async move {
            let mut b = b;
            let _ = serve_udp_socket(&listen, &mut b).await;
        });

        let visitor = UdpSocket::bind("127.0.0.1:0").await?;
        visitor.connect(listen_addr).await?;
        visitor.send(b"hello").await?;
        let mut buf = [0u8; 16];
        let n = time::timeout(Duration::from_secs(2), visitor.recv(&mut buf)).await??;
        assert_eq!(&buf[..n], b"hello");
        Ok(())
    }
}
