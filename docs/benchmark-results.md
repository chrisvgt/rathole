# Benchmark results

Results of the tunnel benchmark (`benches/tunnel.rs`) for every release since v0.5.0, compared with the latest [frp](https://github.com/fatedier/frp). The [README](../README.md#benchmark) shows the last three releases. The original benchmark from 2021 is in [benchmark.md](benchmark.md).

## Setup

- Date: 2026-10-29
- Machine: VM with 4 vCPUs (Intel, 2.5 GHz) and 8 GB RAM, Linux
- rathole: every version built from its tag with Rust 1.99.1, release profile and default features. v0.5.0 and v0.5.2 needed `cargo update -p time` to build with current Rust.
- frp: v0.71.1, the official linux_amd64 release
- Command: `benches/scripts/compare.sh v0.5.0 v0.5.1 v0.5.2 v0.6.0 v0.6.1 v0.6.2 v0.6.3 -- --rounds 3`. Every value is the median of 3 runs.
- Load: before each run the benchmark waits until other processes use less than 10 % of the CPUs, and repeats a run if they used more during it. In this run other processes used at most 3.1 %, so no run had to be repeated.

## What is measured

Server, client and the forwarded services all run on the loopback interface. Every version and transport gets a fresh server and client.

- **Throughput**: one connection writes as fast as it can for 5 s, after 1 s of warm-up, to a service that discards the data.
- **Latency**: round trip time of 64-byte messages on one connection, 5000 samples.
- **Connections/s**: 1000 new connections, 8 at a time, each with a round trip of one byte.
- **Peak memory**: highest resident memory (VmHWM) of server and client over the whole run, which includes 200 connections held open at once.

Transports:

- `tcp`: no encryption
- `noise`: `Noise_NK_25519_ChaChaPoly_BLAKE2s`
- `tls`: native-tls with the certificate from `examples/tls`

For frp, `tcp` means `transport.tls.enable = false` and `tls` is frp's default. frp has no noise transport.

## Results

| Version | Transport | Throughput | Latency p50 | Latency p99 | Connections/s | Peak memory server / client |
|---|---|---|---|---|---|---|
| v0.6.3 | tcp | 12.45 Gbit/s | 67 µs | 92 µs | 11469 | 11.2 / 11.3 MiB |
| v0.6.3 | noise | 3.04 Gbit/s | 72 µs | 101 µs | 5300 | 43.0 / 34.8 MiB |
| v0.6.3 | tls | 4.19 Gbit/s | 83 µs | 122 µs | 1914 | 20.9 / 25.1 MiB |
| v0.6.2 | tcp | 11.74 Gbit/s | 68 µs | 110 µs | 12656 | 11.2 / 11.2 MiB |
| v0.6.2 | noise | 3.05 Gbit/s | 71 µs | 97 µs | 5354 | 43.4 / 35.5 MiB |
| v0.6.2 | tls | 4.29 Gbit/s | 84 µs | 122 µs | 2051 | 21.0 / 25.2 MiB |
| v0.6.1 | tcp | 12.08 Gbit/s | 67 µs | 91 µs | 12480 | 11.4 / 11.4 MiB |
| v0.6.1 | noise | 3.01 Gbit/s | 73 µs | 112 µs | 3942 | 43.5 / 36.4 MiB |
| v0.6.1 | tls | 4.31 Gbit/s | 83 µs | 119 µs | 824 | 20.8 / 25.4 MiB |
| v0.6.0 | tcp | 12.36 Gbit/s | 70 µs | 107 µs | 13063 | 11.2 / 11.2 MiB |
| v0.6.0 | noise | 2.84 Gbit/s | 84 µs | 133 µs | 3787 | 43.2 / 34.2 MiB |
| v0.6.0 | tls | 4.22 Gbit/s | 90 µs | 143 µs | 751 | 20.7 / 25.3 MiB |
| v0.5.2 | tcp | 11.47 Gbit/s | 69 µs | 104 µs | 11940 | 11.2 / 11.1 MiB |
| v0.5.2 | noise | 2.89 Gbit/s | 74 µs | 117 µs | 3727 | 43.0 / 35.1 MiB |
| v0.5.2 | tls | 4.23 Gbit/s | 87 µs | 144 µs | 772 | 20.7 / 25.3 MiB |
| v0.5.1 | tcp | 11.14 Gbit/s | 67 µs | 92 µs | 12421 | 11.2 / 11.1 MiB |
| v0.5.1 | noise | 3.05 Gbit/s | 70 µs | 90 µs | 4425 | 43.2 / 34.8 MiB |
| v0.5.1 | tls | 4.27 Gbit/s | 87 µs | 135 µs | 725 | 20.7 / 25.2 MiB |
| v0.5.0 | tcp | 12.60 Gbit/s | 67 µs | 99 µs | 12910 | 12.4 / 12.3 MiB |
| v0.5.0 | noise | 3.00 Gbit/s | 72 µs | 103 µs | 4610 | 44.6 / 37.2 MiB |
| v0.5.0 | tls | 4.37 Gbit/s | 81 µs | 119 µs | 838 | 21.7 / 26.4 MiB |
| frp v0.71.1 | tcp | 12.32 Gbit/s | 107 µs | 139 µs | 6356 | 53.6 / 53.1 MiB |
| frp v0.71.1 | tls | 4.18 Gbit/s | 110 µs | 158 µs | 5632 | 53.7 / 71.4 MiB |

## Notes

- Throughput and connection rate change by up to 20 % between runs on this VM, so the smaller differences between versions there are noise. Latency and memory stay within a few percent.
- The tls connection rate went up from about 800/s to about 2000/s in v0.6.2. Since then the server does the handshake outside its accept loop, so handshakes run in parallel.
- rathole opens a new connection to the server for every forwarded connection. frp multiplexes all of them over one connection, so it does not pay for a new TLS handshake each time. That is why frp opens more connections per second with tls, but fewer without encryption.
- Compared with frp, rathole has about a third lower latency and uses about a fifth of the memory without encryption. The throughput is about the same.

## Running it

The benchmark is not part of `cargo test` and only runs on request. It needs Linux for the memory column.

```sh
# The rathole binary of this tree
cargo bench --bench tunnel

# Any rathole or frp binaries
cargo bench --bench tunnel -- --rathole v0.6.3=path/to/rathole --frp v0.71.1=path/to/frp_dir

# Options such as --transport, --duration, --rounds and --max-load
cargo bench --bench tunnel -- --help

# Build tags, download frp (latest or FRP_VERSION) and compare them
benches/scripts/compare.sh v0.6.1 v0.6.2 v0.6.3 -- --rounds 3
```

`compare.sh` needs git, cargo, curl and the GitHub CLI (only to look up the latest frp version). It verifies the frp download against frp's published SHA256 checksums.
