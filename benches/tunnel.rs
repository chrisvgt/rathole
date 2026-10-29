//! Tunnel benchmark. Starts rathole (or frp) as separate processes on the
//! loopback interface and measures throughput, latency, connection rate and
//! peak memory through the tunnel.
//!
//! It is not part of `cargo test` and only runs on request:
//!
//! ```text
//! cargo bench --bench tunnel
//! cargo bench --bench tunnel -- --rathole v0.5.0=path/to/rathole --frp v0.71.1=path/to/frp
//! ```
//!
//! Without `--rathole` or `--frp` the rathole binary of this tree is tested.
//! Every run waits for the machine to be idle first and is repeated if other
//! processes used too much CPU during it (`--max-load`).
//! `benches/scripts/compare.sh` builds older tags and downloads frp.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

const TOKEN: &str = "bench";
const NOISE_PRIVATE_KEY: &str = "kQiSRtS3bs8BoGCJYgFnl1FLrTG1lV53Dj8jSjmg8tE=";
const NOISE_PUBLIC_KEY: &str = "mEnUEACy9UrTBmwoCJb6fcKWBRdvfD9XzuBVsroOLFg=";

const LATENCY_WARMUP: usize = 200;
const LATENCY_SAMPLES: usize = 5000;
const LATENCY_MSG_SIZE: usize = 64;
const CONN_TOTAL: usize = 1000;
const CONN_WORKERS: usize = 8;
const HELD_CONNECTIONS: usize = 200;

#[derive(Parser)]
#[command(about = "Benchmark rathole and frp through the loopback interface")]
struct Args {
    /// rathole binary to test, as LABEL=PATH. Can be repeated
    #[arg(long, value_name = "LABEL=PATH", value_parser = parse_target)]
    rathole: Vec<(String, PathBuf)>,

    /// Directory with frps and frpc to test, as LABEL=DIR. Can be repeated
    #[arg(long, value_name = "LABEL=DIR", value_parser = parse_target)]
    frp: Vec<(String, PathBuf)>,

    /// Transports to test. frp has no noise transport
    #[arg(long, value_delimiter = ',', default_value = "tcp,noise,tls")]
    transport: Vec<Transport>,

    /// Seconds to measure the throughput
    #[arg(long, default_value_t = 5)]
    duration: u64,

    /// Runs per version and transport. The table shows the median
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    rounds: u32,

    /// Share of all CPUs in percent that other processes may use. A run is
    /// only started below it, and repeated if it was exceeded during the run
    #[arg(long, default_value_t = 10.0)]
    max_load: f64,

    /// How often a run is repeated because the machine was busy
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// Passed by `cargo bench`
    #[arg(long, hide = true)]
    bench: bool,
}

fn parse_target(s: &str) -> Result<(String, PathBuf), String> {
    let (label, path) = s.split_once('=').ok_or("expected LABEL=PATH")?;
    Ok((label.to_string(), PathBuf::from(path)))
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Transport {
    Tcp,
    Noise,
    Tls,
}

impl Transport {
    fn name(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Noise => "noise",
            Transport::Tls => "tls",
        }
    }
}

enum Target {
    Rathole(PathBuf),
    Frp(PathBuf),
}

/// Local services the tunnel forwards to
struct Services {
    echo: SocketAddr,
    sink: SocketAddr,
    received: Arc<AtomicU64>,
}

struct Tunnel {
    server: Child,
    client: Child,
    echo: SocketAddr,
    sink: SocketAddr,
}

struct Row {
    throughput: f64,
    p50: Duration,
    p99: Duration,
    conn_rate: f64,
    memory: Option<(f64, f64)>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    if !args.bench {
        // Started by `cargo test --all-targets`
        return Ok(());
    }
    if args.rathole.is_empty() && args.frp.is_empty() {
        args.rathole.push((
            format!("v{} (this tree)", env!("CARGO_PKG_VERSION")),
            PathBuf::from(env!("CARGO_BIN_EXE_rathole")),
        ));
    }

    let mut targets = Vec::new();
    for (label, path) in args.rathole {
        targets.push((label, Target::Rathole(path)));
    }
    for (label, path) in args.frp {
        targets.push((format!("frp {}", label), Target::Frp(path)));
    }

    let services = start_services().await?;
    let dir = std::env::temp_dir().join(format!("rathole-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let mut max_load = 0.0_f64;
    let mut table = String::new();
    writeln!(table, "| Version | Transport | Throughput ↑ | Latency p50 ↓ | Latency p99 ↓ | Connections/s ↑ | Peak memory server / client ↓ |")?;
    writeln!(table, "|---|---|---|---|---|---|---|")?;
    for (label, target) in &targets {
        for &transport in &args.transport {
            if matches!(target, Target::Frp(_)) && transport == Transport::Noise {
                continue;
            }
            let mut rows = Vec::new();
            let mut busy = false;
            'rounds: for round in 1..=args.rounds {
                for attempt in 0..=args.retries {
                    wait_idle(args.max_load).await;
                    eprint!(
                        "{} {} ({}/{}): ",
                        label,
                        transport.name(),
                        round,
                        args.rounds
                    );
                    match run(target, transport, &services, &dir, args.duration).await {
                        Ok((row, load)) => {
                            eprintln!("{:.1} % CPU used by other processes", load);
                            if load > args.max_load && attempt < args.retries {
                                eprintln!("  machine was busy, repeating");
                                continue;
                            }
                            busy |= load > args.max_load;
                            max_load = max_load.max(load);
                            rows.push(row);
                            break;
                        }
                        Err(e) => {
                            eprintln!("failed: {:#}", e);
                            break 'rounds;
                        }
                    }
                }
            }
            match median(rows, args.rounds) {
                Some(r) => writeln!(
                    table,
                    "| {}{} | {} | {:.2} Gbit/s | {} µs | {} µs | {:.0} | {} |",
                    label,
                    if busy { " *" } else { "" },
                    transport.name(),
                    r.throughput,
                    r.p50.as_micros(),
                    r.p99.as_micros(),
                    r.conn_rate,
                    match r.memory {
                        Some((s, c)) => format!("{:.1} / {:.1} MiB", s, c),
                        None => "-".to_string(),
                    }
                )?,
                None => {
                    writeln!(
                        table,
                        "| {} | {} | failed | | | | |",
                        label,
                        transport.name()
                    )?;
                }
            }
        }
    }

    std::fs::remove_dir_all(&dir)?;
    println!("{}", system_info());
    println!(
        "Other processes used at most {:.1} % CPU during a run.",
        max_load
    );
    println!();
    print!("{}", table);
    println!();
    println!("↑ higher is better, ↓ lower is better.");
    if table.contains(" * |") {
        println!(
            "\\* Other processes used more than {} % CPU in every attempt.",
            args.max_load
        );
    }
    Ok(())
}

/// CPU time of the machine and of this process, in clock ticks
#[derive(Clone, Copy)]
struct CpuSample {
    /// user, nice, system and the time the hypervisor gave to other VMs
    busy: u64,
    total: u64,
    own: u64,
}

fn cpu_sample() -> Option<CpuSample> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    // user nice system idle iowait irq softirq steal guest guest_nice. Guest
    // time is already part of user. irq and softirq are not counted as busy,
    // as the loopback traffic of the benchmark itself shows up there
    let t: Vec<u64> = stat
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    Some(CpuSample {
        busy: t.first()? + t.get(1)? + t.get(2)? + t.get(7)?,
        total: t.iter().sum(),
        own: process_ticks("self")?,
    })
}

/// User and system time of a process, in clock ticks
fn process_ticks(pid: &str) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // The fields after the command name, starting with the state
    let fields: Vec<&str> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

/// Share of all CPUs in percent that processes other than the benchmark,
/// rathole and frp used between two samples
fn other_load(before: CpuSample, after: CpuSample, children: u64) -> f64 {
    let total = after.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    let other = after
        .busy
        .saturating_sub(before.busy)
        .saturating_sub(after.own.saturating_sub(before.own))
        .saturating_sub(children);
    other as f64 * 100.0 / total as f64
}

/// Waits up to a minute for the machine to become idle
async fn wait_idle(max_load: f64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Some(before) = cpu_sample() else {
            return;
        };
        sleep(Duration::from_secs(1)).await;
        let Some(after) = cpu_sample() else {
            return;
        };
        let load = other_load(before, after, 0);
        if load <= max_load {
            return;
        }
        if Instant::now() > deadline {
            eprintln!("  machine still busy, starting anyway");
            return;
        }
        eprintln!(
            "  machine busy ({:.1} % CPU used by other processes), waiting",
            load
        );
    }
}

/// Median of each column, or None if a round failed
fn median(rows: Vec<Row>, rounds: u32) -> Option<Row> {
    if rows.is_empty() || rows.len() != rounds as usize {
        return None;
    }
    fn mid<T: Copy + PartialOrd>(mut v: Vec<T>) -> T {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    }
    let memory = rows
        .iter()
        .map(|r| r.memory)
        .collect::<Option<Vec<_>>>()
        .map(|m| {
            (
                mid(m.iter().map(|x| x.0).collect()),
                mid(m.iter().map(|x| x.1).collect()),
            )
        });
    Some(Row {
        throughput: mid(rows.iter().map(|r| r.throughput).collect()),
        p50: mid(rows.iter().map(|r| r.p50).collect()),
        p99: mid(rows.iter().map(|r| r.p99).collect()),
        conn_rate: mid(rows.iter().map(|r| r.conn_rate).collect()),
        memory,
    })
}

async fn run(
    target: &Target,
    transport: Transport,
    services: &Services,
    dir: &Path,
    duration: u64,
) -> Result<(Row, f64)> {
    let before = cpu_sample();
    let mut tunnel = match target {
        Target::Rathole(bin) => start_rathole(bin, transport, services, dir).await?,
        Target::Frp(frp_dir) => start_frp(frp_dir, transport, services, dir).await?,
    };
    let result = measure(&tunnel, services, duration).await;
    let memory = tunnel.peak_memory();
    let children = tunnel.cpu_ticks();
    let after = cpu_sample();
    tunnel.stop().await;
    let load = match (before, after) {
        (Some(before), Some(after)) => other_load(before, after, children),
        _ => 0.0,
    };
    if result.is_err() {
        for name in ["server.log", "client.log"] {
            if let Ok(log) = std::fs::read_to_string(dir.join(name)) {
                eprint!("{}:\n{}", name, log);
            }
        }
    }
    let (throughput, p50, p99, conn_rate) = result?;
    let row = Row {
        throughput,
        p50,
        p99,
        conn_rate,
        memory,
    };
    Ok((row, load))
}

async fn measure(
    tunnel: &Tunnel,
    services: &Services,
    duration: u64,
) -> Result<(f64, Duration, Duration, f64)> {
    wait_ready(tunnel.echo).await?;
    let (p50, p99) = latency(tunnel.echo).await.context("latency")?;
    let throughput = throughput(tunnel.sink, &services.received, duration)
        .await
        .context("throughput")?;
    let conn_rate = conn_rate(tunnel.echo).await.context("connection rate")?;
    hold_connections(tunnel.echo)
        .await
        .context("concurrent connections")?;
    Ok((throughput, p50, p99, conn_rate))
}

async fn start_services() -> Result<Services> {
    let echo = TcpListener::bind("127.0.0.1:0").await?;
    let sink = TcpListener::bind("127.0.0.1:0").await?;
    let services = Services {
        echo: echo.local_addr()?,
        sink: sink.local_addr()?,
        received: Arc::new(AtomicU64::new(0)),
    };

    tokio::spawn(async move {
        while let Ok((conn, _)) = echo.accept().await {
            tokio::spawn(async move {
                let _ = conn.set_nodelay(true);
                let (mut rd, mut wr) = conn.into_split();
                let _ = io::copy(&mut rd, &mut wr).await;
            });
        }
    });

    let received = services.received.clone();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = sink.accept().await {
            let received = received.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n @ 1..) = conn.read(&mut buf).await {
                    received.fetch_add(n as u64, Ordering::Relaxed);
                }
            });
        }
    });

    Ok(services)
}

/// Picks a free port. Another process may grab it before it is used, which
/// is unlikely on a benchmark machine.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn spawn(bin: &Path, args: &[&Path], dir: &Path, log: &str) -> Result<Child> {
    let log = std::fs::File::create(dir.join(log))?;
    Command::new(bin)
        .args(args)
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start {}", bin.display()))
}

async fn start_rathole(
    bin: &Path,
    transport: Transport,
    services: &Services,
    dir: &Path,
) -> Result<Tunnel> {
    let control = free_port()?;
    let echo = free_port()?;
    let sink = free_port()?;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tls");

    let (server_transport, client_transport) = match transport {
        Transport::Tcp => (String::new(), String::new()),
        Transport::Noise => (
            format!(
                "[server.transport]\ntype = \"noise\"\n[server.transport.noise]\nlocal_private_key = \"{}\"\n",
                NOISE_PRIVATE_KEY
            ),
            format!(
                "[client.transport]\ntype = \"noise\"\n[client.transport.noise]\nremote_public_key = \"{}\"\n",
                NOISE_PUBLIC_KEY
            ),
        ),
        Transport::Tls => (
            format!(
                "[server.transport]\ntype = \"tls\"\n[server.transport.tls]\npkcs12 = \"{}\"\npkcs12_password = \"1234\"\n",
                examples.join("identity.pfx").display()
            ),
            format!(
                "[client.transport]\ntype = \"tls\"\n[client.transport.tls]\ntrusted_root = \"{}\"\nhostname = \"localhost\"\n",
                examples.join("rootCA.crt").display()
            ),
        ),
    };

    let server_config = dir.join("server.toml");
    std::fs::write(
        &server_config,
        format!(
            "[server]\nbind_addr = \"127.0.0.1:{control}\"\ndefault_token = \"{TOKEN}\"\n{server_transport}\
             [server.services.echo]\nbind_addr = \"127.0.0.1:{echo}\"\n\
             [server.services.sink]\nbind_addr = \"127.0.0.1:{sink}\"\n"
        ),
    )?;
    let client_config = dir.join("client.toml");
    std::fs::write(
        &client_config,
        format!(
            "[client]\nremote_addr = \"127.0.0.1:{control}\"\ndefault_token = \"{TOKEN}\"\n{client_transport}\
             [client.services.echo]\nlocal_addr = \"{}\"\n\
             [client.services.sink]\nlocal_addr = \"{}\"\n",
            services.echo, services.sink
        ),
    )?;

    let server = spawn(
        bin,
        &[Path::new("--server"), &server_config],
        dir,
        "server.log",
    )?;
    wait_listening(control).await?;
    let client = spawn(
        bin,
        &[Path::new("--client"), &client_config],
        dir,
        "client.log",
    )?;
    Ok(Tunnel {
        server,
        client,
        echo: SocketAddr::from(([127, 0, 0, 1], echo)),
        sink: SocketAddr::from(([127, 0, 0, 1], sink)),
    })
}

async fn start_frp(
    frp_dir: &Path,
    transport: Transport,
    services: &Services,
    dir: &Path,
) -> Result<Tunnel> {
    let control = free_port()?;
    let echo = free_port()?;
    let sink = free_port()?;
    let tls = transport == Transport::Tls;

    let server_config = dir.join("frps.toml");
    std::fs::write(
        &server_config,
        format!(
            "bindAddr = \"127.0.0.1\"\nbindPort = {control}\nauth.token = \"{TOKEN}\"\nlog.level = \"warn\"\n"
        ),
    )?;
    let client_config = dir.join("frpc.toml");
    std::fs::write(
        &client_config,
        format!(
            "serverAddr = \"127.0.0.1\"\nserverPort = {control}\nauth.token = \"{TOKEN}\"\n\
             log.level = \"warn\"\ntransport.tls.enable = {tls}\n\n\
             [[proxies]]\nname = \"echo\"\ntype = \"tcp\"\nlocalIP = \"127.0.0.1\"\nlocalPort = {}\nremotePort = {echo}\n\n\
             [[proxies]]\nname = \"sink\"\ntype = \"tcp\"\nlocalIP = \"127.0.0.1\"\nlocalPort = {}\nremotePort = {sink}\n",
            services.echo.port(),
            services.sink.port()
        ),
    )?;

    let server = spawn(
        &frp_dir.join("frps"),
        &[Path::new("-c"), &server_config],
        dir,
        "server.log",
    )?;
    wait_listening(control).await?;
    let client = spawn(
        &frp_dir.join("frpc"),
        &[Path::new("-c"), &client_config],
        dir,
        "client.log",
    )?;
    Ok(Tunnel {
        server,
        client,
        echo: SocketAddr::from(([127, 0, 0, 1], echo)),
        sink: SocketAddr::from(([127, 0, 0, 1], sink)),
    })
}

impl Tunnel {
    /// Peak resident memory of server and client in MiB. Linux only
    fn peak_memory(&self) -> Option<(f64, f64)> {
        Some((vm_hwm(self.server.id()?)?, vm_hwm(self.client.id()?)?))
    }

    /// CPU time of server and client so far, in clock ticks
    fn cpu_ticks(&self) -> u64 {
        [self.server.id(), self.client.id()]
            .into_iter()
            .flatten()
            .filter_map(|pid| process_ticks(&pid.to_string()))
            .sum()
    }

    async fn stop(&mut self) {
        let _ = self.client.kill().await;
        let _ = self.server.kill().await;
    }
}

fn vm_hwm(pid: u32) -> Option<f64> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

/// Sends one byte through the echo service and waits for it to come back
async fn roundtrip(addr: SocketAddr) -> Result<TcpStream> {
    let mut conn = TcpStream::connect(addr).await?;
    conn.write_all(&[1]).await?;
    let mut buf = [0u8; 1];
    conn.read_exact(&mut buf).await?;
    Ok(conn)
}

/// Waits until the server accepts connections, so the client does not give
/// up on its first login
async fn wait_listening(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        if Instant::now() > deadline {
            bail!("server did not listen on port {} within 10s", port);
        }
        sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn wait_ready(echo: SocketAddr) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Ok(_)) = timeout(Duration::from_secs(2), roundtrip(echo)).await {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("tunnel did not come up within 30s");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Round trip time of small messages on one connection
async fn latency(echo: SocketAddr) -> Result<(Duration, Duration)> {
    let mut conn = TcpStream::connect(echo).await?;
    conn.set_nodelay(true)?;
    let msg = [0u8; LATENCY_MSG_SIZE];
    let mut buf = [0u8; LATENCY_MSG_SIZE];
    let mut samples = Vec::with_capacity(LATENCY_SAMPLES);
    for i in 0..LATENCY_WARMUP + LATENCY_SAMPLES {
        let start = Instant::now();
        conn.write_all(&msg).await?;
        conn.read_exact(&mut buf).await?;
        if i >= LATENCY_WARMUP {
            samples.push(start.elapsed());
        }
    }
    samples.sort();
    Ok((
        samples[LATENCY_SAMPLES / 2],
        samples[LATENCY_SAMPLES * 99 / 100],
    ))
}

/// Bulk transfer on one connection, in Gbit/s
async fn throughput(sink: SocketAddr, received: &AtomicU64, duration: u64) -> Result<f64> {
    let mut conn = TcpStream::connect(sink).await?;
    let writer = tokio::spawn(async move {
        let buf = vec![0u8; 64 * 1024];
        while conn.write_all(&buf).await.is_ok() {}
    });

    // Skip the slow start
    sleep(Duration::from_secs(1)).await;
    let (start, start_bytes) = (Instant::now(), received.load(Ordering::Relaxed));
    sleep(Duration::from_secs(duration)).await;
    let (end, end_bytes) = (Instant::now(), received.load(Ordering::Relaxed));
    writer.abort();

    if end_bytes == start_bytes {
        bail!("no data arrived");
    }
    Ok((end_bytes - start_bytes) as f64 * 8.0 / (end - start).as_secs_f64() / 1e9)
}

/// New connections per second, each with one round trip
async fn conn_rate(echo: SocketAddr) -> Result<f64> {
    let next = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let workers: Vec<_> = (0..CONN_WORKERS)
        .map(|_| {
            let next = next.clone();
            tokio::spawn(async move {
                while next.fetch_add(1, Ordering::Relaxed) < CONN_TOTAL {
                    timeout(Duration::from_secs(5), roundtrip(echo)).await??;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .collect();
    for w in workers {
        w.await??;
    }
    Ok(CONN_TOTAL as f64 / start.elapsed().as_secs_f64())
}

/// Keeps many connections open at once, so they show up in the peak memory
async fn hold_connections(echo: SocketAddr) -> Result<()> {
    let mut conns = Vec::with_capacity(HELD_CONNECTIONS);
    for _ in 0..HELD_CONNECTIONS {
        conns.push(timeout(Duration::from_secs(5), roundtrip(echo)).await??);
    }
    Ok(())
}

/// Hardware and OS the benchmark ran on
fn system_info() -> String {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let field = |name: &str| {
        cpuinfo
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
    };
    let cpu = field("model name").unwrap_or_else(|| "unknown CPU".to_string());
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    let mhz = field("cpu MHz")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| format!(", {:.0} MHz", v))
        .unwrap_or_default();
    // AES-NI and AVX2 speed up the encryption of the tls and noise transports
    let flags = field("flags").unwrap_or_default();
    let has = |flag: &str| {
        if flags.split_whitespace().any(|f| f == flag) {
            "yes"
        } else {
            "no"
        }
    };
    let vm = if has("hypervisor") == "yes" {
        ", VM"
    } else {
        ""
    };
    let ram = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<f64>().ok())
        })
        .map(|kib| format!("{:.1} GiB RAM", kib / 1024.0 / 1024.0))
        .unwrap_or_else(|| "unknown RAM".to_string());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| format!("Linux {}", s.trim()))
        .unwrap_or_else(|_| std::env::consts::OS.to_string());
    format!(
        "{}, {} threads{}{}, AES-NI: {}, AVX2: {}, {}, {}",
        cpu,
        threads,
        mhz,
        vm,
        has("aes"),
        has("avx2"),
        ram,
        kernel
    )
}
