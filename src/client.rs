use crate::backoff::{retry_notify, ExponentialBackoff};
use crate::config::{
    ClientConfig, ClientServiceConfig, Config, ServiceMode, ServiceType, TransportType,
};
use crate::config_watcher::{ClientServiceChange, ConfigChange};
use crate::protocol::Hello::{self, *};
use crate::protocol::{
    self, read_ack, read_control_cmd, read_data_cmd, read_hello, Ack, Auth, ControlChannelCmd,
    DataChannelCmd, CURRENT_PROTO_VERSION, HASH_WIDTH_IN_BYTES,
};
use crate::transport::{AddrMaybeCached, SocketOpts, TcpTransport, Transport};
use crate::udp::{forward_udp, serve_udp_socket};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{copy_bidirectional_with_sizes, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{self, Duration, Instant};
use tracing::{debug, error, info, instrument, warn, Instrument, Span};

#[cfg(feature = "noise")]
use crate::transport::NoiseTransport;
#[cfg(any(feature = "native-tls", feature = "rustls"))]
use crate::transport::TlsTransport;
#[cfg(any(feature = "websocket-native-tls", feature = "websocket-rustls"))]
use crate::transport::WebsocketTransport;

use crate::constants::{
    listen_backoff, run_control_chan_backoff, COPY_BUFFER_SIZE, UDP_MAX_FORWARDERS,
};

/// Timeout for connecting to the server and authenticating a reverse visitor
const VISITOR_HANDSHAKE_TIMEOUT: u64 = 10;

// The entrypoint of running a client
pub async fn run_client(
    config: Config,
    shutdown_rx: broadcast::Receiver<bool>,
    update_rx: mpsc::Receiver<ConfigChange>,
) -> Result<()> {
    let config = config.client.ok_or_else(|| {
        anyhow!(
        "Try to run as a client, but the configuration is missing. Please add the `[client]` block"
    )
    })?;

    match config.transport.transport_type {
        TransportType::Tcp => {
            let mut client = Client::<TcpTransport>::from(config).await?;
            client.run(shutdown_rx, update_rx).await
        }
        TransportType::Tls => {
            #[cfg(any(feature = "native-tls", feature = "rustls"))]
            {
                let mut client = Client::<TlsTransport>::from(config).await?;
                client.run(shutdown_rx, update_rx).await
            }
            #[cfg(not(any(feature = "native-tls", feature = "rustls")))]
            crate::helper::feature_neither_compile("native-tls", "rustls")
        }
        TransportType::Noise => {
            #[cfg(feature = "noise")]
            {
                let mut client = Client::<NoiseTransport>::from(config).await?;
                client.run(shutdown_rx, update_rx).await
            }
            #[cfg(not(feature = "noise"))]
            crate::helper::feature_not_compile("noise")
        }
        TransportType::Websocket => {
            #[cfg(any(feature = "websocket-native-tls", feature = "websocket-rustls"))]
            {
                let mut client = Client::<WebsocketTransport>::from(config).await?;
                client.run(shutdown_rx, update_rx).await
            }
            #[cfg(not(any(feature = "websocket-native-tls", feature = "websocket-rustls")))]
            crate::helper::feature_neither_compile("websocket-native-tls", "websocket-rustls")
        }
    }
}

type ServiceDigest = protocol::Digest;
type Nonce = protocol::Digest;

// Holds the state of a client
struct Client<T: Transport> {
    config: ClientConfig,
    service_handles: HashMap<String, ControlChannelHandle>,
    transport: Arc<T>,
}

impl<T: 'static + Transport> Client<T> {
    // Create a Client from `[client]` config block
    async fn from(config: ClientConfig) -> Result<Client<T>> {
        let transport =
            Arc::new(T::new(&config.transport).with_context(|| "Failed to create the transport")?);
        Ok(Client {
            config,
            service_handles: HashMap::new(),
            transport,
        })
    }

    // The entrypoint of Client
    async fn run(
        &mut self,
        mut shutdown_rx: broadcast::Receiver<bool>,
        mut update_rx: mpsc::Receiver<ConfigChange>,
    ) -> Result<()> {
        for (name, config) in &self.config.services {
            // Create a control channel for each service defined
            let handle = ControlChannelHandle::new(
                (*config).clone(),
                self.config.remote_addr.clone(),
                self.transport.clone(),
                self.config.heartbeat_timeout,
            );
            self.service_handles.insert(name.clone(), handle);
        }

        // Wait for the shutdown signal
        loop {
            tokio::select! {
                val = shutdown_rx.recv() => {
                    match val {
                        Ok(_) => {}
                        Err(err) => {
                            error!("Unable to listen for shutdown signal: {}", err);
                        }
                    }
                    break;
                },
                e = update_rx.recv() => {
                    if let Some(e) = e {
                        self.handle_hot_reload(e).await;
                    }
                }
            }
        }

        // Shutdown all services
        for (_, handle) in self.service_handles.drain() {
            handle.shutdown();
        }

        Ok(())
    }

    async fn handle_hot_reload(&mut self, e: ConfigChange) {
        match e {
            ConfigChange::ClientChange(client_change) => match client_change {
                ClientServiceChange::Add(cfg) => {
                    let name = cfg.name.clone();
                    let handle = ControlChannelHandle::new(
                        cfg,
                        self.config.remote_addr.clone(),
                        self.transport.clone(),
                        self.config.heartbeat_timeout,
                    );
                    let _ = self.service_handles.insert(name, handle);
                }
                ClientServiceChange::Delete(s) => {
                    let _ = self.service_handles.remove(&s);
                }
            },
            ignored => warn!("Ignored {:?} since running as a client", ignored),
        }
    }
}

struct RunDataChannelArgs<T: Transport> {
    session_key: Nonce,
    remote_addr: AddrMaybeCached,
    connector: Arc<T>,
    socket_opts: SocketOpts,
    service: ClientServiceConfig,
}

async fn do_data_channel_handshake<T: Transport>(
    args: Arc<RunDataChannelArgs<T>>,
) -> Result<T::Stream> {
    // Retry at least every 100ms, at most for 10 seconds
    let backoff = ExponentialBackoff {
        max_interval: Duration::from_millis(100),
        max_elapsed_time: Some(Duration::from_secs(10)),
        ..Default::default()
    };

    // Connect to remote_addr
    let mut conn: T::Stream = retry_notify(
        backoff,
        || async {
            args.connector
                .connect(&args.remote_addr)
                .await
                .with_context(|| format!("Failed to connect to {}", args.remote_addr))
        },
        |e, duration| {
            warn!("{:#}. Retry in {:?}", e, duration);
        },
    )
    .await?;

    T::hint(&conn, args.socket_opts);

    // Send nonce
    let session_key: [u8; HASH_WIDTH_IN_BYTES] = args.session_key[..]
        .try_into()
        .with_context(|| "Invalid session key length")?;
    let hello = Hello::DataChannelHello(CURRENT_PROTO_VERSION, session_key);
    conn.write_all(&protocol::encode(&hello)?).await?;
    conn.flush().await?;

    Ok(conn)
}

async fn run_data_channel<T: Transport>(args: Arc<RunDataChannelArgs<T>>) -> Result<()> {
    // Do the handshake
    let mut conn = do_data_channel_handshake(args.clone()).await?;

    let local_addr = args
        .service
        .local_addr
        .as_deref()
        .with_context(|| "Missing local_addr")?;

    // Forward
    match read_data_cmd(&mut conn).await? {
        DataChannelCmd::StartForwardTcp => {
            if args.service.service_type != ServiceType::Tcp {
                bail!("Expect TCP traffic. Please check the configuration.")
            }
            run_data_channel_for_tcp::<T>(conn, local_addr).await?;
        }
        DataChannelCmd::StartForwardUdp => {
            if args.service.service_type != ServiceType::Udp {
                bail!("Expect UDP traffic. Please check the configuration.")
            }
            run_data_channel_for_udp::<T>(conn, local_addr, args.service.prefer_ipv6).await?;
        }
    }
    Ok(())
}

// Simply copying back and forth for TCP
#[instrument(skip(conn))]
async fn run_data_channel_for_tcp<T: Transport>(
    mut conn: T::Stream,
    local_addr: &str,
) -> Result<()> {
    debug!("New data channel starts forwarding");

    let mut local = TcpStream::connect(local_addr)
        .await
        .with_context(|| format!("Failed to connect to {}", local_addr))?;
    let _ =
        copy_bidirectional_with_sizes(&mut conn, &mut local, COPY_BUFFER_SIZE, COPY_BUFFER_SIZE)
            .await;
    Ok(())
}

#[instrument(skip(conn))]
async fn run_data_channel_for_udp<T: Transport>(
    conn: T::Stream,
    local_addr: &str,
    prefer_ipv6: bool,
) -> Result<()> {
    forward_udp(conn, local_addr, prefer_ipv6, UDP_MAX_FORWARDERS).await
}

// Control channel, using T as the transport layer
struct ControlChannel<T: Transport> {
    digest: ServiceDigest,              // SHA256 of the service name
    service: ClientServiceConfig,       // `[client.services.foo]` config block
    shutdown_rx: oneshot::Receiver<u8>, // Receives the shutdown signal
    remote_addr: String,                // `client.remote_addr`
    transport: Arc<T>,                  // Wrapper around the transport layer
    heartbeat_timeout: u64,             // Application layer heartbeat timeout in secs
}

// Handle of a control channel
// Dropping it will also drop the actual control channel
struct ControlChannelHandle {
    shutdown_tx: oneshot::Sender<u8>,
}

impl<T: 'static + Transport> ControlChannel<T> {
    #[instrument(skip_all)]
    async fn run(&mut self) -> Result<()> {
        let mut remote_addr = AddrMaybeCached::new(&self.remote_addr);
        remote_addr.resolve().await?;

        let mut conn = self
            .transport
            .connect(&remote_addr)
            .await
            .with_context(|| format!("Failed to connect to {}", self.remote_addr))?;
        T::hint(&conn, SocketOpts::for_control_channel());

        // Send hello
        debug!("Sending hello");
        let hello_send = Hello::ControlChannelHello(CURRENT_PROTO_VERSION, self.digest);
        conn.write_all(&protocol::encode(&hello_send)?).await?;
        conn.flush().await?;

        // Read hello
        debug!("Reading hello");
        let nonce = match read_hello(&mut conn).await? {
            ControlChannelHello(_, d) => d,
            _ => {
                bail!("Unexpected type of hello");
            }
        };

        // Send auth
        debug!("Sending auth");
        let mut concat = Vec::from(
            self.service
                .token
                .as_ref()
                .with_context(|| "Missing token")?
                .as_bytes(),
        );
        concat.extend_from_slice(&nonce);

        let session_key = protocol::digest(&concat);
        let auth = Auth(session_key);
        conn.write_all(&protocol::encode(&auth)?).await?;
        conn.flush().await?;

        // Read ack
        debug!("Reading ack");
        match read_ack(&mut conn).await? {
            Ack::Ok => {}
            v => {
                return Err(anyhow!("{}", v))
                    .with_context(|| format!("Authentication failed: {}", self.service.name));
            }
        }

        // Channel ready
        info!("Control channel established");

        // Socket options for the data channel
        let socket_opts = SocketOpts::from_client_cfg(&self.service);
        let data_ch_args = Arc::new(RunDataChannelArgs {
            session_key,
            remote_addr,
            connector: self.transport.clone(),
            socket_opts,
            service: self.service.clone(),
        });

        loop {
            tokio::select! {
                val = read_control_cmd(&mut conn) => {
                    let val = val?;
                    debug!( "Received {:?}", val);
                    match val {
                        ControlChannelCmd::CreateDataChannel => {
                            let args = data_ch_args.clone();
                            tokio::spawn(async move {
                                if let Err(e) = run_data_channel(args).await.with_context(|| "Failed to run the data channel") {
                                    warn!("{:#}", e);
                                }
                            }.instrument(Span::current()));
                        },
                        ControlChannelCmd::HeartBeat => ()
                    }
                },
                _ = time::sleep(Duration::from_secs(self.heartbeat_timeout)), if self.heartbeat_timeout != 0 => {
                    return Err(anyhow!("Heartbeat timed out"))
                }
                _ = &mut self.shutdown_rx => {
                    break;
                }
            }
        }

        info!("Control channel shutdown");
        Ok(())
    }
}

impl ControlChannelHandle {
    #[instrument(name="handle", skip_all, fields(service = %service.name))]
    fn new<T: 'static + Transport>(
        service: ClientServiceConfig,
        remote_addr: String,
        transport: Arc<T>,
        heartbeat_timeout: u64,
    ) -> ControlChannelHandle {
        let digest = protocol::digest(service.name.as_bytes());

        info!("Starting {}", hex::encode(digest));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // A reverse service has no control channel. The client listens and
        // opens a connection to the server for every visitor
        if service.mode == ServiceMode::Reverse {
            let args = Arc::new(ReverseArgs {
                digest,
                service,
                remote_addr,
                transport,
            });
            tokio::spawn(
                async move {
                    if let Err(e) = run_reverse_service(args, shutdown_rx)
                        .await
                        .with_context(|| "Failed to run the reverse service")
                    {
                        error!("{:#}", e);
                    }
                }
                .instrument(Span::current()),
            );
            return ControlChannelHandle { shutdown_tx };
        }

        let mut retry_backoff = run_control_chan_backoff(
            service
                .retry_interval
                .unwrap_or(crate::config::DEFAULT_CLIENT_RETRY_INTERVAL_SECS),
        );

        let mut s = ControlChannel {
            digest,
            service,
            shutdown_rx,
            remote_addr,
            transport,
            heartbeat_timeout,
        };

        tokio::spawn(
            async move {
                let mut start = Instant::now();

                while let Err(err) = s
                    .run()
                    .await
                    .with_context(|| "Failed to run the control channel")
                {
                    if s.shutdown_rx.try_recv() != Err(oneshot::error::TryRecvError::Empty) {
                        break;
                    }

                    if start.elapsed() > Duration::from_secs(3) {
                        // The client runs for at least 3 secs and then disconnects
                        retry_backoff.reset();
                    }

                    if let Some(duration) = retry_backoff.next_backoff() {
                        error!("{:#}. Retry in {:?}...", err, duration);
                        time::sleep(duration).await;
                    } else {
                        // The backoff has no deadline, so this is not expected to happen
                        error!("{:#}. Giving up", err);
                        break;
                    }

                    start = Instant::now();
                }
            }
            .instrument(Span::current()),
        );

        ControlChannelHandle { shutdown_tx }
    }

    fn shutdown(self) {
        // A send failure shows that the actor has already shutdown.
        let _ = self.shutdown_tx.send(0u8);
    }
}

/// What a reverse service needs to open connections to the server
struct ReverseArgs<T: Transport> {
    digest: ServiceDigest,        // SHA256 of the service name
    service: ClientServiceConfig, // `[client.services.foo]` config block
    remote_addr: String,          // `client.remote_addr`
    transport: Arc<T>,            // Wrapper around the transport layer
}

impl<T: Transport> ReverseArgs<T> {
    fn bind_addr(&self) -> Result<&str> {
        self.service
            .bind_addr
            .as_deref()
            .with_context(|| "Missing bind_addr")
    }
}

/// Opens a connection to the server with the configured transport and
/// authenticates it for the reverse service. The server answers with a fresh
/// nonce and the client proves it knows the token, like for a control channel.
/// The token itself is never sent. Returns the stream after the server
/// accepted it and `cmd` was sent
async fn connect_visitor<T: Transport>(
    args: &ReverseArgs<T>,
    cmd: DataChannelCmd,
) -> Result<T::Stream> {
    time::timeout(Duration::from_secs(VISITOR_HANDSHAKE_TIMEOUT), async {
        // Resolve again, the address of the server may have changed
        let mut remote_addr = AddrMaybeCached::new(&args.remote_addr);
        remote_addr.resolve().await?;

        let mut conn = args
            .transport
            .connect(&remote_addr)
            .await
            .with_context(|| format!("Failed to connect to {}", args.remote_addr))?;
        T::hint(&conn, SocketOpts::from_client_cfg(&args.service));

        // Send hello
        let hello_send = Hello::VisitorHello(CURRENT_PROTO_VERSION, args.digest);
        conn.write_all(&protocol::encode(&hello_send)?).await?;
        conn.flush().await?;

        // Read hello
        let nonce = match read_hello(&mut conn).await? {
            VisitorHello(_, d) => d,
            _ => bail!("Unexpected type of hello"),
        };

        // Send auth
        let mut concat = Vec::from(
            args.service
                .token
                .as_ref()
                .with_context(|| "Missing token")?
                .as_bytes(),
        );
        concat.extend_from_slice(&nonce);
        let auth = Auth(protocol::digest(&concat));
        conn.write_all(&protocol::encode(&auth)?).await?;
        conn.flush().await?;

        // Read ack
        match read_ack(&mut conn).await? {
            Ack::Ok => {}
            v => {
                return Err(anyhow!("{}", v))
                    .with_context(|| format!("Authentication failed: {}", args.service.name));
            }
        }

        // Tell the server what to forward
        conn.write_all(&protocol::encode(&cmd)?).await?;
        conn.flush().await?;

        Ok(conn)
    })
    .await
    .with_context(|| "Timeout connecting to the server")?
}

async fn run_reverse_service<T: 'static + Transport>(
    args: Arc<ReverseArgs<T>>,
    shutdown_rx: oneshot::Receiver<u8>,
) -> Result<()> {
    match args.service.service_type {
        ServiceType::Tcp => run_reverse_tcp(args, shutdown_rx).await,
        ServiceType::Udp => run_reverse_udp(args, shutdown_rx).await,
    }
}

/// Listens at `bind_addr`. Every visitor gets a connection to the server of its own
async fn run_reverse_tcp<T: 'static + Transport>(
    args: Arc<ReverseArgs<T>>,
    mut shutdown_rx: oneshot::Receiver<u8>,
) -> Result<()> {
    let bind_addr = args.bind_addr()?.to_string();

    let l = tokio::select! {
        l = retry_notify(
            listen_backoff(),
            || async { Ok(TcpListener::bind(&bind_addr).await?) },
            |e: &anyhow::Error, duration| error!("{:#}. Retry in {:?}", e, duration),
        ) => l.with_context(|| "Failed to listen for the service")?,
        _ = &mut shutdown_rx => return Ok(()),
    };
    info!("Listening at {}", bind_addr);

    // Retry at least every 1s
    let mut backoff = ExponentialBackoff {
        max_interval: Duration::from_secs(1),
        max_elapsed_time: None,
        ..Default::default()
    };

    loop {
        tokio::select! {
            val = l.accept() => match val {
                Err(e) => {
                    // Possibly a EMFILE. So sleep for a while
                    error!("{}. Sleep for a while", e);
                    match backoff.next_backoff() {
                        Some(d) => time::sleep(d).await,
                        None => {
                            // This branch will never be reached for current backoff policy
                            error!("Too many retries. Aborting...");
                            break;
                        }
                    }
                }
                Ok((visitor, addr)) => {
                    backoff.reset();
                    debug!("New visitor from {}", addr);
                    let args = args.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_reverse_tcp_visitor(args, visitor)
                            .await
                            .with_context(|| "Failed to forward the visitor")
                        {
                            warn!("{:#}", e);
                        }
                    }.instrument(Span::current()));
                }
            },
            _ = &mut shutdown_rx => break,
        }
    }

    info!("TCPListener shutdown");
    Ok(())
}

async fn run_reverse_tcp_visitor<T: Transport>(
    args: Arc<ReverseArgs<T>>,
    mut visitor: TcpStream,
) -> Result<()> {
    SocketOpts::from_client_cfg(&args.service).apply(&visitor);
    let mut conn = connect_visitor(&args, DataChannelCmd::StartForwardTcp).await?;
    let _ =
        copy_bidirectional_with_sizes(&mut conn, &mut visitor, COPY_BUFFER_SIZE, COPY_BUFFER_SIZE)
            .await;
    Ok(())
}

/// Listens at `bind_addr` and keeps one tunnel to the server for all datagrams.
/// A broken tunnel is set up again
async fn run_reverse_udp<T: 'static + Transport>(
    args: Arc<ReverseArgs<T>>,
    mut shutdown_rx: oneshot::Receiver<u8>,
) -> Result<()> {
    let bind_addr = args.bind_addr()?.to_string();

    let l = tokio::select! {
        l = retry_notify(
            listen_backoff(),
            || async { Ok(UdpSocket::bind(&bind_addr).await?) },
            |e: &anyhow::Error, duration| warn!("{:#}. Retry in {:?}", e, duration),
        ) => l.with_context(|| "Failed to listen for the service")?,
        _ = &mut shutdown_rx => return Ok(()),
    };
    info!("Listening at {}", bind_addr);

    let mut retry_backoff = run_control_chan_backoff(
        args.service
            .retry_interval
            .unwrap_or(crate::config::DEFAULT_CLIENT_RETRY_INTERVAL_SECS),
    );

    loop {
        let start = Instant::now();

        let err = tokio::select! {
            ret = async {
                let mut conn = connect_visitor(&args, DataChannelCmd::StartForwardUdp).await?;
                info!("UDP tunnel established");
                serve_udp_socket(&l, &mut conn)
                    .await
                    .with_context(|| "The UDP tunnel is broken")
            } => ret.err(),
            _ = &mut shutdown_rx => break,
        };

        if start.elapsed() > Duration::from_secs(3) {
            // The tunnel ran for at least 3 secs and then broke
            retry_backoff.reset();
        }

        let Some(duration) = retry_backoff.next_backoff() else {
            // The backoff has no deadline, so this is not expected to happen
            bail!("Giving up the UDP tunnel");
        };
        match err {
            Some(err) => error!("{:#}. Retry in {:?}...", err, duration),
            None => warn!("The UDP tunnel ended. Retry in {:?}...", duration),
        }
        tokio::select! {
            _ = time::sleep(duration) => {}
            _ = &mut shutdown_rx => break,
        }
    }

    info!("UDP tunnel shutdown");
    Ok(())
}
