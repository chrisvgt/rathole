# Benchmark results

Results of the tunnel benchmark (`benches/tunnel.rs`) for every release since v0.5.0, compared with the latest [frp](https://github.com/fatedier/frp). The [README](../README.md#benchmark) shows the last three releases. The original benchmark from 2021 is in [benchmark.md](benchmark.md).

## Setup

- Date: 2026-10-29
- QEMU VM with 4 vCPUs (QEMU Virtual CPU version 2.5+), 8 GB RAM, Arch Linux, kernel 7.2.9
- rathole: every version built from its tag with Rust 1.99.1, release profile and default features. v0.5.0 and v0.5.2 needed `cargo update -p time` to build with current Rust.
- frp: v0.71.1, the official linux_amd64 release
- Command: `benches/scripts/compare.sh v0.5.0 v0.5.1 v0.5.2 v0.6.0 v0.6.1 v0.6.2 v0.6.3 -- --rounds 3`. Every value is the median of 3 runs.

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
| v0.6.3 | tcp | 15.90 Gbit/s | 66 µs | 87 µs | 13260 | 11.0 / 10.9 MiB |
| v0.6.3 | noise | 3.05 Gbit/s | 71 µs | 101 µs | 5633 | 42.8 / 34.5 MiB |
| v0.6.3 | tls | 4.37 Gbit/s | 81 µs | 118 µs | 2121 | 20.7 / 24.8 MiB |
| v0.6.2 | tcp | 14.32 Gbit/s | 67 µs | 89 µs | 12696 | 11.3 / 11.2 MiB |
| v0.6.2 | noise | 3.04 Gbit/s | 70 µs | 86 µs | 5341 | 43.1 / 34.1 MiB |
| v0.6.2 | tls | 4.45 Gbit/s | 81 µs | 119 µs | 2017 | 21.0 / 25.0 MiB |
| v0.6.1 | tcp | 13.37 Gbit/s | 67 µs | 84 µs | 12325 | 11.3 / 11.2 MiB |
| v0.6.1 | noise | 3.07 Gbit/s | 70 µs | 90 µs | 4561 | 43.2 / 35.4 MiB |
| v0.6.1 | tls | 4.33 Gbit/s | 81 µs | 126 µs | 840 | 20.8 / 25.5 MiB |
| v0.6.0 | tcp | 11.40 Gbit/s | 67 µs | 87 µs | 12600 | 11.1 / 11.2 MiB |
| v0.6.0 | noise | 3.08 Gbit/s | 70 µs | 94 µs | 4665 | 43.1 / 35.2 MiB |
| v0.6.0 | tls | 4.37 Gbit/s | 81 µs | 112 µs | 807 | 20.7 / 25.2 MiB |
| v0.5.2 | tcp | 13.49 Gbit/s | 67 µs | 91 µs | 13557 | 11.2 / 11.2 MiB |
| v0.5.2 | noise | 3.06 Gbit/s | 71 µs | 96 µs | 4475 | 43.0 / 35.1 MiB |
| v0.5.2 | tls | 4.27 Gbit/s | 82 µs | 112 µs | 822 | 20.6 / 25.3 MiB |
| v0.5.1 | tcp | 12.04 Gbit/s | 67 µs | 98 µs | 13302 | 11.2 / 11.1 MiB |
| v0.5.1 | noise | 3.06 Gbit/s | 70 µs | 91 µs | 4468 | 43.2 / 34.6 MiB |
| v0.5.1 | tls | 4.33 Gbit/s | 80 µs | 115 µs | 822 | 20.6 / 25.3 MiB |
| v0.5.0 | tcp | 12.54 Gbit/s | 67 µs | 97 µs | 12165 | 12.4 / 12.3 MiB |
| v0.5.0 | noise | 3.05 Gbit/s | 71 µs | 94 µs | 4703 | 44.1 / 36.3 MiB |
| v0.5.0 | tls | 4.32 Gbit/s | 80 µs | 103 µs | 831 | 21.7 / 26.3 MiB |
| frp v0.71.1 | tcp | 11.46 Gbit/s | 105 µs | 134 µs | 5739 | 50.4 / 44.3 MiB |
| frp v0.71.1 | tls | 3.92 Gbit/s | 111 µs | 149 µs | 5594 | 50.5 / 65.1 MiB |

## Notes

- The tcp throughput of a single loopback stream changes by up to 20 % between runs on this VM, so the differences between versions there are noise. Latency, connection rate and memory stay within a few percent.
- The tls connection rate went up from about 800/s to about 2000/s in v0.6.2. Since then the server does the handshake outside its accept loop, so handshakes run in parallel.
- rathole opens a new connection to the server for every forwarded connection. frp multiplexes all of them over one connection, so it does not pay for a new TLS handshake each time. That is why frp opens more connections per second with tls, but fewer without encryption.
- Compared with frp, rathole has about a third lower latency and uses about a quarter of the memory without encryption.

## Running it

The benchmark is not part of `cargo test` and only runs on request. It needs Linux for the memory column.

```sh
# The rathole binary of this tree
cargo bench --bench tunnel

# Any rathole or frp binaries
cargo bench --bench tunnel -- --rathole v0.6.3=path/to/rathole --frp v0.71.1=path/to/frp_dir

# Options such as --transport, --duration and --rounds
cargo bench --bench tunnel -- --help

# Build tags, download frp (latest or FRP_VERSION) and compare them
benches/scripts/compare.sh v0.6.1 v0.6.2 v0.6.3 -- --rounds 3
```

`compare.sh` needs git, cargo, curl and the GitHub CLI (only to look up the latest frp version). It verifies the frp download against frp's published SHA256 checksums.
