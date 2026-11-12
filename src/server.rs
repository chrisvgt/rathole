use crate::backoff::ExponentialBackoff;
use crate::config::{
    Config, ServerConfig, ServerServiceConfig, ServiceMode, ServiceType, TransportType,
};
use crate::config_watcher::{ConfigChange, ServerServiceChange};
use crate::constants::{listen_backoff, COPY_BUFFER_SIZE, UDP_MAX_FORWARDERS};
use crate::helper::{retry_notify_with_deadline, write_and_flush};
use crate::multi_map::MultiMap;
use crate::protocol::Hello::{ControlChannelHello, DataChannelHello, VisitorHello};
use crate::protocol::{
    self, read_auth, read_data_cmd, read_hello, Ack, ControlChannelCmd, DataChannelCmd, Hello,
    HASH_WIDTH_IN_BYTES,
};
use crate::transport::{SocketOpts, TcpTransport, Transport};
use crate::udp::{forward_udp, serve_udp_socket};
use anyhow::{anyhow, bail, Context, Result};
use subtle::ConstantTimeEq;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{self, copy_bidirectional_with_sizes, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::time;
use tracing::{debug, error, info, info_span, instrument, warn, Instrument, Span};

#[cfg(feature = "noise")]
use crate::transport::NoiseTransport;
#[cfg(any(feature = "native-tls", feature = "rustls"))]
use crate::transport::TlsTransport;
#[cfg(any(feature = "websocket-native-tls", feature = "websocket-rustls"))]
use crate::transport::WebsocketTransport;

type ServiceDigest = protocol::Digest; // SHA256 of a service name
type Nonce = protocol::Digest; // Also called `session_key`

const TCP_POOL_SIZE: usize = 8; // The number of cached connections for TCP servies
const UDP_POOL_SIZE: usize = 2; // The number of cached connections for UDP services
const CHAN_SIZE: usize = 2048; // The capacity of various chans
const HANDSHAKE_TIMEOUT: u64 = 5; // Timeout for transport handshake and each handshake message
const MAX_PENDING_HANDSHAKES: usize = 1024; // Connections that have not finished the handshake

// The entrypoint of running a server
pub async fn run_server(
    config: Config,
    shutdown_rx: broadcast::Receiver<bool>,
    update_rx: mpsc::Receiver<ConfigChange>,
) -> Result<()> {
    let config = match config.server {
            Some(config) => config,
            None => {
                return Err(anyhow!("Try to run as a server, but the configuration is missing. Please add the `[server]` block"))
            }
        };

    match config.transport.transport_type {
        TransportType::Tcp => {
            let mut server = Server::<TcpTransport>::from(config).await?;
            server.run(shutdown_rx, update_rx).await?;
        }
        TransportType::Tls => {
            #[cfg(any(feature = "native-tls", feature = "rustls"))]
            {
                let mut server = Server::<TlsTransport>::from(config).await?;
                server.run(shutdown_rx, update_rx).await?;
            }
            #[cfg(not(any(feature = "native-tls", feature = "rustls")))]
            crate::helper::feature_neither_compile("native-tls", "rustls")
        }
        TransportType::Noise => {
            #[cfg(feature = "noise")]
            {
                let mut server = Server::<NoiseTransport>::from(config).await?;
                server.run(shutdown_rx, update_rx).await?;
            }
            #[cfg(not(feature = "noise"))]
            crate::helper::feature_not_compile("noise")
        }
        TransportType::Websocket => {
            #[cfg(any(feature = "websocket-native-tls", feature = "websocket-rustls"))]
            {
                let mut server = Server::<WebsocketTransport>::from(config).await?;
                server.run(shutdown_rx, update_rx).await?;
            }
            #[cfg(not(any(feature = "websocket-native-tls", feature = "websocket-rustls")))]
            crate::helper::feature_neither_compile("websocket-native-tls", "websocket-rustls")
        }
    }

    Ok(())
}

// A hash map of ControlChannelHandles, indexed by ServiceDigest or Nonce
// See also MultiMap
type ControlChannelMap<T> = MultiMap<ServiceDigest, Nonce, ControlChannelHandle<T>>;

// Stops the visitors of a reverse service once the service is changed or removed
type ReverseSessions = Arc<RwLock<HashMap<ServiceDigest, broadcast::Sender<bool>>>>;

// Server holds all states of running a server
struct Server<T: Transport> {
    // `[server]` config
    config: Arc<ServerConfig>,

    // `[server.services]` config, indexed by ServiceDigest
    services: Arc<RwLock<HashMap<ServiceDigest, ServerServiceConfig>>>,
    // Collection of contorl channels
    control_channels: Arc<RwLock<ControlChannelMap<T>>>,
    // Running visitors of reverse services
    reverse_sessions: ReverseSessions,
    // Wrapper around the transport layer
    transport: Arc<T>,
}

// Generate a hash map of services which is indexed by ServiceDigest
fn generate_service_hashmap(
    server_config: &ServerConfig,
) -> HashMap<ServiceDigest, ServerServiceConfig> {
    let mut ret = HashMap::new();
    for u in &server_config.services {
        ret.insert(protocol::digest(u.0.as_bytes()), (*u.1).clone());
    }
    ret
}

impl<T: 'static + Transport> Server<T> {
    // Create a server from `[server]`
    pub async fn from(config: ServerConfig) -> Result<Server<T>> {
        let config = Arc::new(config);
        let services = Arc::new(RwLock::new(generate_service_hashmap(&config)));
        let control_channels = Arc::new(RwLock::new(ControlChannelMap::new()));
        let transport = Arc::new(T::new(&config.transport)?);
        Ok(Server {
            config,
            services,
            control_channels,
            reverse_sessions: Default::default(),
            transport,
        })
    }

    // The entry point of Server
    pub async fn run(
        &mut self,
        mut shutdown_rx: broadcast::Receiver<bool>,
        mut update_rx: mpsc::Receiver<ConfigChange>,
    ) -> Result<()> {
        // Listen at `server.bind_addr`
        let l = self
            .transport
            .bind(&self.config.bind_addr)
            .await
            .with_context(|| "Failed to listen at `server.bind_addr`")?;
        info!("Listening at {}", self.config.bind_addr);

        // Retry at least every 100ms
        let mut backoff = ExponentialBackoff {
            max_interval: Duration::from_millis(100),
            max_elapsed_time: None,
            ..Default::default()
        };

        let pending = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));

        // Wait for connections and shutdown signals
        loop {
            tokio::select! {
                // Wait for incoming control and data channels
                ret = self.transport.accept(&l) => {
                    match ret {
                        Err(err) => {
                            // Detects whether it's an IO error
                            if let Some(err) = err.downcast_ref::<io::Error>() {
                                // If it is an IO error, then it's possibly an
                                // EMFILE. So sleep for a while and retry
                                // TODO: Only sleep for EMFILE, ENFILE, ENOMEM, ENOBUFS
                                if let Some(d) = backoff.next_backoff() {
                                    error!("Failed to accept: {:#}. Retry in {:?}...", err, d);
                                    time::sleep(d).await;
                                } else {
                                    // This branch will never be executed according to the current retry policy
                                    error!("Too many retries. Aborting...");
                                    break;
                                }
                            }
                            // If it's not an IO error, then it comes from
                            // the transport layer, so just ignore it
                        }
                        Ok((conn, addr)) => {
                            backoff.reset();

                            // Bound the number of connections that have not finished the handshake yet
                            let permit = match pending.clone().try_acquire_owned() {
                                Ok(v) => v,
                                Err(_) => {
                                    warn!("Too many pending handshakes. Dropping the connection from {}", addr);
                                    continue;
                                }
                            };

                            // Handshake in its own task, so a slow peer cannot block the accept loop
                            let transport = self.transport.clone();
                            let services = self.services.clone();
                            let control_channels = self.control_channels.clone();
                            let server_config = self.config.clone();
                            let reverse_sessions = self.reverse_sessions.clone();
                            let shutdown_rx = shutdown_rx.resubscribe();
                            tokio::spawn(async move {
                                // Fixed wording with the bare IP, so fail2ban and CrowdSec can match it
                                if let Err(err) = handle_connection(transport, conn, services, control_channels, reverse_sessions, server_config, shutdown_rx, permit).await {
                                    error!("Handshake from {} failed: {:#}", addr.ip(), err);
                                }
                            }.instrument(info_span!("connection", %addr)));
                        }
                    }
                },
                // Wait for the shutdown signal
                _ = shutdown_rx.recv() => {
                    info!("Shuting down gracefully...");
                    break;
                },
                e = update_rx.recv() => {
                    if let Some(e) = e {
                        self.handle_hot_reload(e).await;
                    }
                }
            }
        }

        info!("Shutdown");

        Ok(())
    }

    async fn handle_hot_reload(&mut self, e: ConfigChange) {
        match e {
            ConfigChange::ServerChange(server_change) => match server_change {
                ServerServiceChange::Add(cfg) => {
                    let hash = protocol::digest(cfg.name.as_bytes());
                    let mut wg = self.services.write().await;
                    let _ = wg.insert(hash, cfg);

                    let mut wg = self.control_channels.write().await;
                    let _ = wg.remove1(&hash);

                    // Visitors that were authenticated for the old config end
                    let _ = self.reverse_sessions.write().await.remove(&hash);
                }
                ServerServiceChange::Delete(s) => {
                    let hash = protocol::digest(s.as_bytes());
                    let _ = self.services.write().await.remove(&hash);
                    let _ = self.reverse_sessions.write().await.remove(&hash);

                    let mut wg = self.control_channels.write().await;
                    let _ = wg.remove1(&hash);
                }
            },
            ignored => warn!("Ignored {:?} since running as a server", ignored),
        }
    }
}

// Handle connections to `server.bind_addr`
#[allow(clippy::too_many_arguments)]
async fn handle_connection<T: 'static + Transport>(
    transport: Arc<T>,
    conn: T::RawStream,
    services: Arc<RwLock<HashMap<ServiceDigest, ServerServiceConfig>>>,
    control_channels: Arc<RwLock<ControlChannelMap<T>>>,
    reverse_sessions: ReverseSessions,
    server_config: Arc<ServerConfig>,
    mut shutdown_rx: broadcast::Receiver<bool>,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
    // Do transport handshake with a timeout
    let mut conn = time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT),
        transport.handshake(conn),
    )
    .await
    .with_context(|| "Transport handshake timeout")?
    .with_context(|| "Failed to do transport handshake")?;

    // Read hello
    let hello = time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT),
        read_hello(&mut conn),
    )
    .await
    .with_context(|| "Timeout reading hello")??;
    match hello {
        ControlChannelHello(_, service_digest) => {
            do_control_channel_handshake(
                conn,
                services,
                control_channels,
                service_digest,
                server_config,
            )
            .await?;
        }
        DataChannelHello(_, nonce) => {
            do_data_channel_handshake(conn, control_channels, nonce).await?;
        }
        VisitorHello(_, service_digest) => {
            let (conn, service, cmd) =
                do_visitor_handshake::<T>(conn, services, service_digest).await?;
            // The visitor is authenticated and forwarding can run for a long
            // time, so it no longer counts as a pending handshake
            drop(permit);

            // Ends with the server, and with the service when it is changed or removed
            let mut service_rx = reverse_sessions
                .write()
                .await
                .entry(service_digest)
                .or_insert_with(|| broadcast::channel(1).0)
                .subscribe();
            tokio::select! {
                // Not part of the handshake, so it is not logged as a failed one
                ret = run_visitor_forward::<T>(conn, &service, cmd) => {
                    if let Err(err) = ret {
                        warn!(service = %service.name, "Reverse service failed: {:#}", err);
                    }
                }
                _ = shutdown_rx.recv() => {}
                _ = service_rx.recv() => {}
            }
        }
    }
    Ok(())
}

async fn do_control_channel_handshake<T: 'static + Transport>(
    mut conn: T::Stream,
    services: Arc<RwLock<HashMap<ServiceDigest, ServerServiceConfig>>>,
    control_channels: Arc<RwLock<ControlChannelMap<T>>>,
    service_digest: ServiceDigest,
    server_config: Arc<ServerConfig>,
) -> Result<()> {
    info!("Try to handshake a control channel");

    T::hint(&conn, SocketOpts::for_control_channel());

    // Generate a nonce
    let mut nonce = vec![0u8; HASH_WIDTH_IN_BYTES];
    rand::fill(&mut nonce[..]);

    // Send hello
    let hello_send = Hello::ControlChannelHello(
        protocol::CURRENT_PROTO_VERSION,
        nonce
            .clone()
            .try_into()
            .map_err(|_| anyhow!("Invalid nonce length"))?,
    );
    conn.write_all(&protocol::encode(&hello_send)?).await?;
    conn.flush().await?;

    // Lookup the service. The lock is not held across network I/O
    let service_config = services.read().await.get(&service_digest).cloned();

    // Read auth
    let protocol::Auth(d) =
        time::timeout(Duration::from_secs(HANDSHAKE_TIMEOUT), read_auth(&mut conn))
            .await
            .with_context(|| "Timeout reading auth")??;

    // An unknown service gets the same answer as a wrong token,
    // so a peer without a token cannot probe which services exist.
    // A reverse service cannot create a forward listener either: it is
    // answered the same way, so its token cannot be used for the wrong job
    let service_config = match service_config {
        Some(v) if v.mode == ServiceMode::Forward => v,
        _ => {
            write_and_flush(&mut conn, &protocol::encode(&Ack::AuthFailed)?).await?;
            bail!("No such a service {}", hex::encode(service_digest));
        }
    };

    let service_name = &service_config.name;

    // Calculate the checksum
    let mut concat = Vec::from(
        service_config
            .token
            .as_ref()
            .with_context(|| "Missing token")?
            .as_bytes(),
    );
    concat.append(&mut nonce);

    // Validate
    let session_key = protocol::digest(&concat);
    if !bool::from(session_key.ct_eq(&d)) {
        write_and_flush(&mut conn, &protocol::encode(&Ack::AuthFailed)?).await?;
        bail!("Service {} failed the authentication", service_name);
    } else {
        // Send ack
        conn.write_all(&protocol::encode(&Ack::Ok)?).await?;
        conn.flush().await?;

        let mut h = control_channels.write().await;

        // If there's already a control channel for the service, then drop the old one.
        // Because a control channel doesn't report back when it's dead,
        // the handle in the map could be stall, dropping the old handle enables
        // the client to reconnect.
        if h.remove1(&service_digest).is_some() {
            warn!(
                "Dropping previous control channel for service {}",
                service_name
            );
        }

        info!(service = %service_config.name, "Control channel established");
        let handle =
            ControlChannelHandle::new(conn, service_config, server_config.heartbeat_interval);

        // Insert the new handle
        let _ = h.insert(service_digest, session_key, handle);
    }

    Ok(())
}

/// Handshake of a visitor of a reverse service. It is the same exchange as
/// for a control channel: the server sends a fresh nonce and the client
/// proves it knows the token by sending `sha256(token || nonce)`. Unknown
/// services and services that are not reverse are answered like a wrong token.
/// Returns the authenticated stream, the service and what the client asked for
async fn do_visitor_handshake<T: 'static + Transport>(
    mut conn: T::Stream,
    services: Arc<RwLock<HashMap<ServiceDigest, ServerServiceConfig>>>,
    service_digest: ServiceDigest,
) -> Result<(T::Stream, ServerServiceConfig, DataChannelCmd)> {
    info!("Try to handshake a visitor");

    T::hint(&conn, SocketOpts::for_control_channel());

    // Generate a nonce
    let mut nonce = vec![0u8; HASH_WIDTH_IN_BYTES];
    rand::fill(&mut nonce[..]);

    // Send hello
    let hello_send = Hello::VisitorHello(
        protocol::CURRENT_PROTO_VERSION,
        nonce
            .clone()
            .try_into()
            .map_err(|_| anyhow!("Invalid nonce length"))?,
    );
    conn.write_all(&protocol::encode(&hello_send)?).await?;
    conn.flush().await?;

    // Lookup the service. The lock is not held across network I/O
    let service_config = services.read().await.get(&service_digest).cloned();

    // Read auth
    let protocol::Auth(d) =
        time::timeout(Duration::from_secs(HANDSHAKE_TIMEOUT), read_auth(&mut conn))
            .await
            .with_context(|| "Timeout reading auth")??;

    let service_config = match service_config {
        Some(v) if v.mode == ServiceMode::Reverse => v,
        _ => {
            write_and_flush(&mut conn, &protocol::encode(&Ack::AuthFailed)?).await?;
            bail!("No such a service {}", hex::encode(service_digest));
        }
    };

    // Calculate the checksum
    let mut concat = Vec::from(
        service_config
            .token
            .as_ref()
            .with_context(|| "Missing token")?
            .as_bytes(),
    );
    concat.append(&mut nonce);

    // Validate
    if !bool::from(protocol::digest(&concat).ct_eq(&d)) {
        write_and_flush(&mut conn, &protocol::encode(&Ack::AuthFailed)?).await?;
        bail!("Service {} failed the authentication", service_config.name);
    }

    write_and_flush(&mut conn, &protocol::encode(&Ack::Ok)?).await?;

    T::hint(&conn, SocketOpts::from_server_cfg(&service_config));

    // The client says whether it brings TCP or UDP traffic
    let cmd = time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT),
        read_data_cmd(&mut conn),
    )
    .await
    .with_context(|| "Timeout reading the command")??;

    Ok((conn, service_config, cmd))
}

/// Connects an authenticated visitor to the local service of a reverse
/// service. The target is always the configured `local_addr`, the client
/// cannot choose it
async fn run_visitor_forward<T: Transport>(
    mut conn: T::Stream,
    service: &ServerServiceConfig,
    cmd: DataChannelCmd,
) -> Result<()> {
    let local_addr = service
        .local_addr
        .as_deref()
        .with_context(|| "Missing local_addr")?;

    match (cmd, service.service_type) {
        (DataChannelCmd::StartForwardTcp, ServiceType::Tcp) => {
            debug!("Visitor starts forwarding to {}", local_addr);
            let mut local = TcpStream::connect(local_addr)
                .await
                .with_context(|| format!("Failed to connect to {}", local_addr))?;
            let _ = copy_bidirectional_with_sizes(
                &mut conn,
                &mut local,
                COPY_BUFFER_SIZE,
                COPY_BUFFER_SIZE,
            )
            .await;
            Ok(())
        }
        (DataChannelCmd::StartForwardUdp, ServiceType::Udp) => {
            debug!("Visitor starts forwarding to {}", local_addr);
            forward_udp(conn, local_addr, false, UDP_MAX_FORWARDERS).await
        }
        (cmd, service_type) => {
            bail!(
                "The visitor asked for {:?}, but the service is {:?}. Please check the configuration",
                cmd,
                service_type
            )
        }
    }
}

async fn do_data_channel_handshake<T: 'static + Transport>(
    conn: T::Stream,
    control_channels: Arc<RwLock<ControlChannelMap<T>>>,
    nonce: Nonce,
) -> Result<()> {
    debug!("Try to handshake a data channel");

    // Validate. The lock is not held while waiting for the connection pool
    let (data_ch_tx, socket_opts) = match control_channels.read().await.get2(&nonce) {
        Some(handle) => (
            handle.data_ch_tx.clone(),
            SocketOpts::from_server_cfg(&handle.service),
        ),
        None => {
            warn!("Data channel has incorrect nonce");
            return Ok(());
        }
    };

    T::hint(&conn, socket_opts);

    // Send the data channel to the corresponding control channel
    data_ch_tx
        .send(conn)
        .await
        .with_context(|| "Data channel for a stale control channel")?;
    Ok(())
}

pub struct ControlChannelHandle<T: Transport> {
    // Shutdown the control channel by dropping it
    _shutdown_tx: broadcast::Sender<bool>,
    data_ch_tx: mpsc::Sender<T::Stream>,
    service: ServerServiceConfig,
}

impl<T> ControlChannelHandle<T>
where
    T: 'static + Transport,
{
    // Create a control channel handle, where the control channel handling task
    // and the connection pool task are created.
    #[instrument(name = "handle", skip_all, fields(service = %service.name))]
    fn new(
        conn: T::Stream,
        service: ServerServiceConfig,
        heartbeat_interval: u64,
    ) -> ControlChannelHandle<T> {
        // Create a shutdown channel
        let (shutdown_tx, shutdown_rx) = broadcast::channel::<bool>(1);

        // Store data channels
        let (data_ch_tx, data_ch_rx) = mpsc::channel(CHAN_SIZE * 2);

        // Store data channel creation requests
        let (data_ch_req_tx, data_ch_req_rx) = mpsc::unbounded_channel();

        // Cache some data channels for later use
        let pool_size = match service.service_type {
            ServiceType::Tcp => TCP_POOL_SIZE,
            ServiceType::Udp => UDP_POOL_SIZE,
        };

        for _i in 0..pool_size {
            if let Err(e) = data_ch_req_tx.send(true) {
                error!("Failed to request data channel {}", e);
            };
        }

        let shutdown_rx_clone = shutdown_tx.subscribe();
        // Only forward services have a control channel, so there is a bind_addr
        let bind_addr = service.bind_addr.clone().unwrap_or_default();
        match service.service_type {
            ServiceType::Tcp => tokio::spawn(
                async move {
                    if let Err(e) = run_tcp_connection_pool::<T>(
                        bind_addr,
                        data_ch_rx,
                        data_ch_req_tx,
                        shutdown_rx_clone,
                    )
                    .await
                    .with_context(|| "Failed to run TCP connection pool")
                    {
                        error!("{:#}", e);
                    }
                }
                .instrument(Span::current()),
            ),
            ServiceType::Udp => tokio::spawn(
                async move {
                    if let Err(e) = run_udp_connection_pool::<T>(
                        bind_addr,
                        data_ch_rx,
                        data_ch_req_tx,
                        shutdown_rx_clone,
                    )
                    .await
                    .with_context(|| "Failed to run UDP connection pool")
                    {
                        error!("{:#}", e);
                    }
                }
                .instrument(Span::current()),
            ),
        };

        // Create the control channel
        let ch = ControlChannel::<T> {
            conn,
            shutdown_rx,
            data_ch_req_rx,
            heartbeat_interval,
        };

        // Run the control channel
        tokio::spawn(
            async move {
                if let Err(err) = ch.run().await {
                    error!("{:#}", err);
                }
            }
            .instrument(Span::current()),
        );

        ControlChannelHandle {
            _shutdown_tx: shutdown_tx,
            data_ch_tx,
            service,
        }
    }
}

// Control channel, using T as the transport layer. P is TcpStream or UdpTraffic
struct ControlChannel<T: Transport> {
    conn: T::Stream,                               // The connection of control channel
    shutdown_rx: broadcast::Receiver<bool>,        // Receives the shutdown signal
    data_ch_req_rx: mpsc::UnboundedReceiver<bool>, // Receives visitor connections
    heartbeat_interval: u64,                       // Application-layer heartbeat interval in secs
}

impl<T: Transport> ControlChannel<T> {
    async fn write_and_flush(&mut self, data: &[u8]) -> Result<()> {
        write_and_flush(&mut self.conn, data)
            .await
            .with_context(|| "Failed to write control cmds")?;
        Ok(())
    }
    // Run a control channel
    #[instrument(skip_all)]
    async fn run(mut self) -> Result<()> {
        let create_ch_cmd = protocol::encode(&ControlChannelCmd::CreateDataChannel)?;
        let heartbeat = protocol::encode(&ControlChannelCmd::HeartBeat)?;

        // Wait for data channel requests and the shutdown signal
        loop {
            tokio::select! {
                val = self.data_ch_req_rx.recv() => {
                    match val {
                        Some(_) => {
                            if let Err(e) = self.write_and_flush(&create_ch_cmd).await {
                                error!("{:#}", e);
                                break;
                            }
                        }
                        None => {
                            break;
                        }
                    }
                },
                _ = time::sleep(Duration::from_secs(self.heartbeat_interval)), if self.heartbeat_interval != 0 => {
                            if let Err(e) = self.write_and_flush(&heartbeat).await {
                                error!("{:#}", e);
                                break;
                            }
                }
                // Wait for the shutdown signal
                _ = self.shutdown_rx.recv() => {
                    break;
                }
            }
        }

        info!("Control channel shutdown");

        Ok(())
    }
}

fn tcp_listen_and_send(
    addr: String,
    data_ch_req_tx: mpsc::UnboundedSender<bool>,
    mut shutdown_rx: broadcast::Receiver<bool>,
) -> mpsc::Receiver<TcpStream> {
    let (tx, rx) = mpsc::channel(CHAN_SIZE);

    tokio::spawn(async move {
        let l = retry_notify_with_deadline(listen_backoff(),  || async {
            Ok(TcpListener::bind(&addr).await?)
        }, |e, duration| {
            error!("{:#}. Retry in {:?}", e, duration);
        }, &mut shutdown_rx).await
        .with_context(|| "Failed to listen for the service");

        let l: TcpListener = match l {
            Ok(v) => v,
            Err(e) => {
                error!("{:#}", e);
                return;
            }
        };

        info!("Listening at {}", &addr);

        // Retry at least every 1s
        let mut backoff = ExponentialBackoff {
            max_interval: Duration::from_secs(1),
            max_elapsed_time: None,
            ..Default::default()
        };

        // Wait for visitors and the shutdown signal
        loop {
            tokio::select! {
                val = l.accept() => {
                    match val {
                        Err(e) => {
                            // `l` is a TCP listener so this must be a IO error
                            // Possibly a EMFILE. So sleep for a while
                            error!("{}. Sleep for a while", e);
                            if let Some(d) = backoff.next_backoff() {
                                time::sleep(d).await;
                            } else {
                                // This branch will never be reached for current backoff policy
                                error!("Too many retries. Aborting...");
                                break;
                            }
                        }
                        Ok((incoming, addr)) => {
                            // For every visitor, request to create a data channel
                            if data_ch_req_tx.send(true).with_context(|| "Failed to send data chan create request").is_err() {
                                // An error indicates the control channel is broken
                                // So break the loop
                                break;
                            }

                            backoff.reset();

                            debug!("New visitor from {}", addr);

                            // Send the visitor to the connection pool
                            let _ = tx.send(incoming).await;
                        }
                    }
                },
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }

        info!("TCPListener shutdown");
    }.instrument(Span::current()));

    rx
}

#[instrument(skip_all)]
async fn run_tcp_connection_pool<T: Transport>(
    bind_addr: String,
    mut data_ch_rx: mpsc::Receiver<T::Stream>,
    data_ch_req_tx: mpsc::UnboundedSender<bool>,
    shutdown_rx: broadcast::Receiver<bool>,
) -> Result<()> {
    let mut visitor_rx = tcp_listen_and_send(bind_addr, data_ch_req_tx.clone(), shutdown_rx);
    let cmd = protocol::encode(&DataChannelCmd::StartForwardTcp)?;

    'pool: while let Some(mut visitor) = visitor_rx.recv().await {
        loop {
            if let Some(mut ch) = data_ch_rx.recv().await {
                if write_and_flush(&mut ch, &cmd).await.is_ok() {
                    tokio::spawn(async move {
                        let _ = copy_bidirectional_with_sizes(
                            &mut ch,
                            &mut visitor,
                            COPY_BUFFER_SIZE,
                            COPY_BUFFER_SIZE,
                        )
                        .await;
                    });
                    break;
                } else {
                    // Current data channel is broken. Request for a new one
                    if data_ch_req_tx.send(true).is_err() {
                        break 'pool;
                    }
                }
            } else {
                break 'pool;
            }
        }
    }

    info!("Shutdown");
    Ok(())
}

#[instrument(skip_all)]
async fn run_udp_connection_pool<T: Transport>(
    bind_addr: String,
    mut data_ch_rx: mpsc::Receiver<T::Stream>,
    _data_ch_req_tx: mpsc::UnboundedSender<bool>,
    mut shutdown_rx: broadcast::Receiver<bool>,
) -> Result<()> {
    // TODO: Load balance

    let l = retry_notify_with_deadline(
        listen_backoff(),
        || async { Ok(UdpSocket::bind(&bind_addr).await?) },
        |e, duration| {
            warn!("{:#}. Retry in {:?}", e, duration);
        },
        &mut shutdown_rx,
    )
    .await
    .with_context(|| "Failed to listen for the service")?;

    info!("Listening at {}", &bind_addr);

    let cmd = protocol::encode(&DataChannelCmd::StartForwardUdp)?;

    // Receive one data channel
    let mut conn = data_ch_rx
        .recv()
        .await
        .ok_or_else(|| anyhow!("No available data channels"))?;
    write_and_flush(&mut conn, &cmd).await?;

    tokio::select! {
        ret = serve_udp_socket(&l, &mut conn) => ret?,
        _ = shutdown_rx.recv() => {}
    }

    debug!("UDP pool dropped");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{read_ack, Auth};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn test_unknown_service_looks_like_wrong_token() -> Result<()> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let mut client = TcpStream::connect(l.local_addr()?).await?;
        let (conn, _) = l.accept().await?;

        let server = tokio::spawn(do_control_channel_handshake::<TcpTransport>(
            conn,
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(RwLock::new(ControlChannelMap::new())),
            protocol::digest(b"unknown"),
            Arc::new(ServerConfig::default()),
        ));

        read_hello(&mut client).await?;
        client
            .write_all(&protocol::encode(&Auth([0u8; HASH_WIDTH_IN_BYTES]))?)
            .await?;
        assert!(matches!(read_ack(&mut client).await?, Ack::AuthFailed));
        assert!(server.await?.is_err());
        Ok(())
    }

    fn service(name: &str, mode: ServiceMode) -> ServerServiceConfig {
        ServerServiceConfig {
            name: name.to_string(),
            mode,
            token: Some(TOKEN.into()),
            ..Default::default()
        }
    }

    fn services(
        list: &[(&str, ServiceMode)],
    ) -> Arc<RwLock<HashMap<ServiceDigest, ServerServiceConfig>>> {
        Arc::new(RwLock::new(
            list.iter()
                .map(|(name, mode)| (protocol::digest(name.as_bytes()), service(name, *mode)))
                .collect(),
        ))
    }

    /// Runs the server half of the visitor handshake and answers it as a
    /// client that sends `Auth(digest(token || nonce))`
    async fn visitor(
        name: &str,
        token: &str,
        list: &[(&str, ServiceMode)],
    ) -> Result<(Ack, Result<DataChannelCmd>)> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let mut client = TcpStream::connect(l.local_addr()?).await?;
        let (conn, _) = l.accept().await?;

        let services = services(list);
        let digest = protocol::digest(name.as_bytes());
        let server = tokio::spawn(async move {
            do_visitor_handshake::<TcpTransport>(conn, services, digest)
                .await
                .map(|(_, _, cmd)| cmd)
        });

        let Hello::VisitorHello(_, nonce) = read_hello(&mut client).await? else {
            bail!("Unexpected hello");
        };
        let mut concat = Vec::from(token.as_bytes());
        concat.extend_from_slice(&nonce);
        client
            .write_all(&protocol::encode(&Auth(protocol::digest(&concat)))?)
            .await?;
        let ack = read_ack(&mut client).await?;
        if matches!(ack, Ack::Ok) {
            client
                .write_all(&protocol::encode(&DataChannelCmd::StartForwardTcp)?)
                .await?;
        }
        Ok((ack, server.await?))
    }

    #[tokio::test]
    async fn test_visitor_handshake_accepts_the_token() -> Result<()> {
        let (ack, ret) = visitor("a", TOKEN, &[("a", ServiceMode::Reverse)]).await?;
        assert!(matches!(ack, Ack::Ok));
        assert!(matches!(ret?, DataChannelCmd::StartForwardTcp));
        Ok(())
    }

    #[tokio::test]
    async fn test_visitor_handshake_rejects_a_wrong_token() -> Result<()> {
        let (ack, ret) = visitor("a", "wrong", &[("a", ServiceMode::Reverse)]).await?;
        assert!(matches!(ack, Ack::AuthFailed));
        assert!(ret.is_err());
        Ok(())
    }

    // A forward service and an unknown one both look like a wrong token
    #[tokio::test]
    async fn test_visitor_handshake_rejects_other_services() -> Result<()> {
        for name in ["a", "unknown"] {
            // The right token must not help
            let (ack, ret) = visitor(name, TOKEN, &[("a", ServiceMode::Forward)]).await?;
            assert!(matches!(ack, Ack::AuthFailed), "{}", name);
            assert!(ret.is_err());
        }
        Ok(())
    }

    // A reverse service must not be able to create a forward listener
    #[tokio::test]
    async fn test_control_channel_rejects_a_reverse_service() -> Result<()> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let mut client = TcpStream::connect(l.local_addr()?).await?;
        let (conn, _) = l.accept().await?;

        let control_channels = Arc::new(RwLock::new(ControlChannelMap::new()));
        let server = tokio::spawn(do_control_channel_handshake::<TcpTransport>(
            conn,
            services(&[("a", ServiceMode::Reverse)]),
            control_channels.clone(),
            protocol::digest(b"a"),
            Arc::new(ServerConfig::default()),
        ));

        let Hello::ControlChannelHello(_, nonce) = read_hello(&mut client).await? else {
            bail!("Unexpected hello");
        };
        let mut concat = Vec::from(TOKEN.as_bytes());
        concat.extend_from_slice(&nonce);
        client
            .write_all(&protocol::encode(&Auth(protocol::digest(&concat)))?)
            .await?;
        assert!(matches!(read_ack(&mut client).await?, Ack::AuthFailed));
        assert!(server.await?.is_err());
        assert!(control_channels
            .read()
            .await
            .get1(&protocol::digest(b"a"))
            .is_none());
        Ok(())
    }
}
