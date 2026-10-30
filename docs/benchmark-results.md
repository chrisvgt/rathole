# Benchmark results

Results of the tunnel benchmark (`benches/tunnel.rs`) for every release since v0.5.0, compared with [frp](https://github.com/fatedier/frp) v0.38.0 from the original benchmark and the latest frp. The [README](../README.md#benchmark) shows v0.5.0, the version this fork started from, and the last two releases. The original benchmark from 2021 is in the [upstream repository](https://github.com/rathole-org/rathole/blob/5a9dd6d939744859af322aeff7fd60f7483a68bc/docs/benchmark.md).

## Setup

<!-- bench:setup -->
- Date: 2026-10-30
- Machine: GitHub Actions runner with 4 vCPUs (AMD EPYC 7763) and 16 GB RAM, Linux
- rathole: every version built from its tag with Rust 1.99.1, release profile and default features. Tags that no longer build with current Rust get `cargo update -p time` first.
- frp: v0.38.0 and v0.71.1, the official linux_amd64 releases
- Command: `FRP_VERSIONS="latest 0.38.0" benches/scripts/compare.sh v0.6.4 v0.6.3 v0.6.2 v0.6.1 v0.6.0 v0.5.2 v0.5.1 v0.5.0 -- --rounds 3`. Every value is the median of 3 runs.
- Load: before each run the benchmark waits until other processes use less than 10 % of the CPUs, and repeats a run if they used more during it. No run had to be repeated. In the kept runs other processes used at most 0.0 %.
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
- `tls`: the default TLS backend of each version, native-tls up to v0.6.4 and rustls since v0.6.5, with the certificate from `examples/tls`

For frp, `tcp` means `transport.tls.enable = false` and `tls` is frp's default. frp has no noise transport.

## Results

<!-- bench:results -->
| Version | Transport | Throughput ↑ | Latency p50 ↓ | Latency p99 ↓ | Connections/s ↑ | Peak memory server / client ↓ |
|---|---|---|---|---|---|---|
| v0.6.4 | tcp | 10.12 Gbit/s | 122 µs | 170 µs | 6326 | 11.7 / 11.5 MiB |
| v0.6.4 | noise | 5.30 Gbit/s | 128 µs | 176 µs | 2910 | 15.1 / 15.0 MiB |
| v0.6.4 | tls | 3.81 Gbit/s | 150 µs | 198 µs | 1123 | 21.5 / 23.8 MiB |
| v0.6.3 | tcp | 7.23 Gbit/s | 123 µs | 173 µs | 6274 | 10.9 / 10.7 MiB |
| v0.6.3 | noise | 3.28 Gbit/s | 135 µs | 184 µs | 2699 | 42.5 / 33.7 MiB |
| v0.6.3 | tls | 4.62 Gbit/s | 147 µs | 200 µs | 1129 | 18.9 / 21.0 MiB |
| v0.6.2 | tcp | 5.25 Gbit/s | 122 µs | 171 µs | 6301 | 11.0 / 10.9 MiB |
| v0.6.2 | noise | 3.40 Gbit/s | 136 µs | 184 µs | 2723 | 42.9 / 34.4 MiB |
| v0.6.2 | tls | 4.71 Gbit/s | 152 µs | 202 µs | 1122 | 18.9 / 20.9 MiB |
| v0.6.1 | tcp | 4.70 Gbit/s | 122 µs | 170 µs | 6104 | 10.9 / 10.9 MiB |
| v0.6.1 | noise | 3.40 Gbit/s | 137 µs | 186 µs | 2254 | 42.6 / 34.0 MiB |
| v0.6.1 | tls | 4.73 Gbit/s | 148 µs | 202 µs | 587 | 18.6 / 21.3 MiB |
| v0.6.0 | tcp | 4.66 Gbit/s | 121 µs | 171 µs | 6292 | 10.8 / 10.9 MiB |
| v0.6.0 | noise | 3.38 Gbit/s | 137 µs | 185 µs | 2287 | 42.2 / 33.3 MiB |
| v0.6.0 | tls | 4.64 Gbit/s | 149 µs | 201 µs | 585 | 18.6 / 21.3 MiB |
| v0.5.2 | tcp | 4.69 Gbit/s | 123 µs | 172 µs | 6227 | 10.8 / 10.7 MiB |
| v0.5.2 | noise | 3.38 Gbit/s | 136 µs | 186 µs | 2295 | 42.3 / 34.2 MiB |
| v0.5.2 | tls | 4.21 Gbit/s | 149 µs | 201 µs | 586 | 18.6 / 21.3 MiB |
| v0.5.1 | tcp | 4.77 Gbit/s | 122 µs | 171 µs | 6151 | 10.9 / 10.8 MiB |
| v0.5.1 | noise | 3.45 Gbit/s | 136 µs | 184 µs | 2289 | 42.0 / 34.1 MiB |
| v0.5.1 | tls | 4.65 Gbit/s | 150 µs | 199 µs | 586 | 18.7 / 21.2 MiB |
| v0.5.0 | tcp | 7.88 Gbit/s | 122 µs | 172 µs | 6312 | 12.0 / 12.0 MiB |
| v0.5.0 | noise | 3.18 Gbit/s | 136 µs | 184 µs | 2435 | 43.2 / 34.1 MiB |
| v0.5.0 | tls | 4.21 Gbit/s | 149 µs | 201 µs | 587 | 19.7 / 22.3 MiB |
| frp v0.71.1 | tcp | 5.01 Gbit/s | 248 µs | 298 µs | 3288 | 36.0 / 35.5 MiB |
| frp v0.71.1 | tls | 3.22 Gbit/s | 264 µs | 316 µs | 3091 | 36.4 / 33.8 MiB |
| frp v0.38.0 | tcp | 2.72 Gbit/s | 243 µs | 293 µs | 3529 | 28.6 / 24.0 MiB |
| frp v0.38.0 | tls | 2.30 Gbit/s | 263 µs | 319 µs | 3318 | 29.2 / 23.8 MiB |

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
