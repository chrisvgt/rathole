# Benchmark results

Results of the tunnel benchmark (`benches/tunnel.rs`) for every release since v0.5.0, compared with [frp](https://github.com/fatedier/frp) v0.38.0 from the original benchmark and the latest frp. The [README](../README.md#benchmark) shows v0.5.0, the version this fork started from, and the last two releases. The original benchmark from 2021 is in the [upstream repository](https://github.com/rathole-org/rathole/blob/5a9dd6d939744859af322aeff7fd60f7483a68bc/docs/benchmark.md).

## Setup

<!-- bench:setup -->
- Date: 2026-10-29
- Machine: VM with 4 vCPUs (Intel, 2.5 GHz) and 8 GB RAM, Linux
- rathole: every version built from its tag with Rust 1.99.1, release profile and default features. Tags that no longer build with current Rust get `cargo update -p time` first.
- frp: v0.38.0 and v0.71.1, the official linux_amd64 releases
- Command: `FRP_VERSIONS="0.38.0 latest" benches/scripts/compare.sh v0.5.0 v0.5.1 v0.5.2 v0.6.0 v0.6.1 v0.6.2 v0.6.3 -- --rounds 3`. Every value is the median of 3 runs.
- Load: before each run the benchmark waits until other processes use less than 10 % of the CPUs, and repeats a run if they used more during it. One run was repeated because the machine was busy. In the kept runs other processes used at most 6.1 %.
<!-- /bench:setup -->

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

<!-- bench:results -->
| Version | Transport | Throughput ↑ | Latency p50 ↓ | Latency p99 ↓ | Connections/s ↑ | Peak memory server / client ↓ |
|---|---|---|---|---|---|---|
| v0.6.3 | tcp | 10.92 Gbit/s | 68 µs | 104 µs | 12265 | 11.0 / 11.0 MiB |
| v0.6.3 | noise | 2.94 Gbit/s | 74 µs | 112 µs | 5198 | 42.9 / 34.9 MiB |
| v0.6.3 | tls | 4.25 Gbit/s | 83 µs | 141 µs | 2053 | 20.8 / 24.9 MiB |
| v0.6.2 | tcp | 13.30 Gbit/s | 67 µs | 105 µs | 12812 | 11.0 / 11.0 MiB |
| v0.6.2 | noise | 3.03 Gbit/s | 71 µs | 91 µs | 5736 | 43.0 / 34.2 MiB |
| v0.6.2 | tls | 4.13 Gbit/s | 84 µs | 142 µs | 2072 | 20.9 / 24.9 MiB |
| v0.6.1 | tcp | 11.98 Gbit/s | 66 µs | 87 µs | 12446 | 11.0 / 11.0 MiB |
| v0.6.1 | noise | 3.01 Gbit/s | 70 µs | 100 µs | 4575 | 42.8 / 34.3 MiB |
| v0.6.1 | tls | 4.30 Gbit/s | 82 µs | 117 µs | 823 | 20.5 / 25.0 MiB |
| v0.6.0 | tcp | 13.78 Gbit/s | 67 µs | 98 µs | 13307 | 10.9 / 10.9 MiB |
| v0.6.0 | noise | 2.99 Gbit/s | 72 µs | 105 µs | 4443 | 42.7 / 34.4 MiB |
| v0.6.0 | tls | 4.40 Gbit/s | 81 µs | 114 µs | 837 | 20.6 / 25.2 MiB |
| v0.5.2 | tcp | 12.44 Gbit/s | 67 µs | 86 µs | 13488 | 10.8 / 10.9 MiB |
| v0.5.2 | noise | 3.05 Gbit/s | 71 µs | 97 µs | 4274 | 42.8 / 35.4 MiB |
| v0.5.2 | tls | 4.38 Gbit/s | 82 µs | 112 µs | 837 | 20.6 / 25.1 MiB |
| v0.5.1 | tcp | 12.15 Gbit/s | 69 µs | 100 µs | 12207 | 10.9 / 10.9 MiB |
| v0.5.1 | noise | 3.08 Gbit/s | 71 µs | 101 µs | 4278 | 42.7 / 34.1 MiB |
| v0.5.1 | tls | 4.29 Gbit/s | 82 µs | 127 µs | 802 | 20.5 / 25.1 MiB |
| v0.5.0 | tcp | 11.97 Gbit/s | 66 µs | 84 µs | 11982 | 11.9 / 12.0 MiB |
| v0.5.0 | noise | 3.01 Gbit/s | 73 µs | 103 µs | 4507 | 43.7 / 36.3 MiB |
| v0.5.0 | tls | 4.35 Gbit/s | 81 µs | 111 µs | 831 | 21.4 / 25.9 MiB |
| frp v0.71.1 | tcp | 11.10 Gbit/s | 107 µs | 146 µs | 6120 | 51.3 / 45.2 MiB |
| frp v0.71.1 | tls | 4.08 Gbit/s | 112 µs | 150 µs | 5507 | 50.0 / 66.4 MiB |
| frp v0.38.0 | tcp | 6.61 Gbit/s | 103 µs | 137 µs | 6946 | 39.8 / 33.5 MiB |
| frp v0.38.0 | tls | 2.96 Gbit/s | 119 µs | 175 µs | 6282 | 40.2 / 33.9 MiB |

↑ higher is better, ↓ lower is better.
<!-- /bench:results -->

## Notes

- Throughput and connection rate change by up to 20 % between runs, so the smaller differences between versions there are noise. Latency and memory stay within a few percent.
- The tls connection rate more than doubled in v0.6.2. Since then the server does the handshake outside its accept loop, so handshakes run in parallel.
- rathole opens a new connection to the server for every forwarded connection. frp multiplexes all of them over one connection, so it does not pay for a new TLS handshake each time. That is why frp opens more connections per second with tls, but fewer without encryption.
- The results are updated by the Benchmark workflow (`.github/workflows/benchmark.yml`) after every release, which opens a pull request with the new numbers.

## Compared with 2021

The [original benchmark](https://github.com/rathole-org/rathole/blob/5a9dd6d939744859af322aeff7fd60f7483a68bc/docs/benchmark.md) from 2021 measured rathole and frp 0.38 with iperf3 on two Xeon E5-2620. Hardware and tools differ, so the absolute numbers cannot be compared. The 2026 values are from a run on 2026-10-29 on a VM with 4 vCPUs (Intel, 2.5 GHz) and are not updated by the workflow.

| | 2021 rathole | 2021 frp 0.38 | 2026 rathole v0.5.0 | 2026 frp v0.38.0 | 2026 frp v0.71.1 |
|---|---|---|---|---|---|
| TCP throughput | 3.6 Gbit/s | 0.8 Gbit/s | 12.0 Gbit/s | 6.6 Gbit/s | 11.1 Gbit/s |
| Memory server / client | about 8 / 7 MB | about 67 / 62 MB | 12 / 12 MiB | 40 / 34 MiB | 51 / 45 MiB |

In 2021 rathole had about 4 times the throughput of frp 0.38. Here the gap is smaller, about 1.8 times, and the latest frp has caught up. rathole still needs a quarter to a third of frp's memory.

## Running it

The benchmark is not part of `cargo test` and only runs on request. It needs Linux for the memory column.

```sh
# The rathole binary of this tree
cargo bench --bench tunnel

# Any rathole or frp binaries
cargo bench --bench tunnel -- --rathole v0.6.3=path/to/rathole --frp v0.71.1=path/to/frp_dir

# Options such as --transport, --duration, --rounds and --max-load
cargo bench --bench tunnel -- --help

# Build tags, download frp (FRP_VERSIONS, default latest) and compare them
FRP_VERSIONS="0.38.0 latest" benches/scripts/compare.sh v0.5.0 v0.6.2 v0.6.3 -- --rounds 3

# Draw the README charts from a result table (Python 3, no other dependencies)
benches/scripts/charts.py docs/benchmark-results.md docs/img v0.5.0 v0.6.2 v0.6.3 "frp v0.38.0" "frp v0.71.1"
```

`benches/scripts/update-docs.py` writes a benchmark output into this page and the README, the Benchmark workflow shows how it is called. `compare.sh` needs git, cargo, curl and the GitHub CLI (only to look up the latest frp version). It verifies the frp download against frp's published SHA256 checksums.
