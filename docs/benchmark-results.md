# Benchmark results

Results of the tunnel benchmark (`benches/tunnel.rs`) for every release since v0.5.0, compared with [frp](https://github.com/fatedier/frp) v0.38.0 from the original benchmark and the latest frp. The [README](../README.md#benchmark) shows v0.5.0, the version this fork started from, and the last two releases. The original benchmark from 2021 is in the [upstream repository](https://github.com/rathole-org/rathole/blob/5a9dd6d939744859af322aeff7fd60f7483a68bc/docs/benchmark.md).

## Setup

<!-- bench:setup -->
- Date: 2026-10-30
- Machine: GitHub Actions runner with 4 vCPUs (Intel Xeon Platinum 8370C) and 16 GB RAM, Linux
- rathole: every version built from its tag with Rust 1.99.1, release profile and default features. Tags that no longer build with current Rust get `cargo update -p time` first.
- frp: v0.38.0 and v0.71.1, the official linux_amd64 releases
- Command: `FRP_VERSIONS="latest 0.38.0" benches/scripts/compare.sh v0.6.5 v0.6.4 v0.6.3 v0.6.2 v0.6.1 v0.6.0 v0.5.2 v0.5.1 v0.5.0 -- --rounds 3`. Every value is the median of 3 runs.
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
| v0.6.5 | tcp | 16.81 Gbit/s | 101 µs | 140 µs | 10298 | 10.8 / 10.6 MiB |
| v0.6.5 | noise | 7.00 Gbit/s | 103 µs | 138 µs | 3730 | 14.5 / 14.4 MiB |
| v0.6.5 | tls | 11.62 Gbit/s | 107 µs | 144 µs | 4131 | 14.2 / 14.7 MiB |
| v0.6.4 | tcp | 16.10 Gbit/s | 102 µs | 133 µs | 10112 | 11.7 / 11.6 MiB |
| v0.6.4 | noise | 7.06 Gbit/s | 107 µs | 140 µs | 3662 | 15.2 / 15.2 MiB |
| v0.6.4 | tls | 8.42 Gbit/s | 115 µs | 154 µs | 1355 | 21.6 / 24.2 MiB |
| v0.6.3 | tcp | 8.54 Gbit/s | 102 µs | 132 µs | 10231 | 11.0 / 11.0 MiB |
| v0.6.3 | noise | 4.98 Gbit/s | 107 µs | 142 µs | 3358 | 43.1 / 34.2 MiB |
| v0.6.3 | tls | 7.18 Gbit/s | 113 µs | 152 µs | 1352 | 18.8 / 21.2 MiB |
| v0.6.2 | tcp | 8.88 Gbit/s | 99 µs | 133 µs | 10189 | 11.0 / 11.0 MiB |
| v0.6.2 | noise | 5.02 Gbit/s | 107 µs | 145 µs | 3392 | 43.1 / 34.3 MiB |
| v0.6.2 | tls | 7.10 Gbit/s | 113 µs | 154 µs | 1361 | 18.8 / 21.1 MiB |
| v0.6.1 | tcp | 7.88 Gbit/s | 101 µs | 137 µs | 10218 | 11.1 / 11.1 MiB |
| v0.6.1 | noise | 4.93 Gbit/s | 108 µs | 143 µs | 2865 | 43.0 / 34.6 MiB |
| v0.6.1 | tls | 7.53 Gbit/s | 112 µs | 152 µs | 748 | 18.8 / 21.6 MiB |
| v0.6.0 | tcp | 8.74 Gbit/s | 99 µs | 136 µs | 10367 | 11.0 / 11.0 MiB |
| v0.6.0 | noise | 5.04 Gbit/s | 108 µs | 140 µs | 2909 | 42.8 / 34.3 MiB |
| v0.6.0 | tls | 7.25 Gbit/s | 112 µs | 151 µs | 748 | 18.7 / 21.6 MiB |
| v0.5.2 | tcp | 8.20 Gbit/s | 101 µs | 140 µs | 10369 | 10.9 / 10.9 MiB |
| v0.5.2 | noise | 5.01 Gbit/s | 105 µs | 146 µs | 2923 | 42.5 / 34.4 MiB |
| v0.5.2 | tls | 7.11 Gbit/s | 111 µs | 152 µs | 747 | 18.8 / 21.2 MiB |
| v0.5.1 | tcp | 9.76 Gbit/s | 99 µs | 130 µs | 10492 | 11.1 / 10.9 MiB |
| v0.5.1 | noise | 5.02 Gbit/s | 109 µs | 149 µs | 2875 | 42.8 / 34.2 MiB |
| v0.5.1 | tls | 7.02 Gbit/s | 114 µs | 159 µs | 748 | 18.7 / 21.4 MiB |
| v0.5.0 | tcp | 8.70 Gbit/s | 98 µs | 150 µs | 10564 | 12.1 / 12.2 MiB |
| v0.5.0 | noise | 4.55 Gbit/s | 109 µs | 141 µs | 2946 | 43.6 / 35.6 MiB |
| v0.5.0 | tls | 6.89 Gbit/s | 113 µs | 150 µs | 741 | 19.5 / 22.3 MiB |
| frp v0.71.1 | tcp | 9.62 Gbit/s | 189 µs | 243 µs | 4804 | 36.1 / 44.1 MiB |
| frp v0.71.1 | tls | 5.96 Gbit/s | 194 µs | 240 µs | 4495 | 36.7 / 55.4 MiB |
| frp v0.38.0 | tcp | 4.96 Gbit/s | 183 µs | 227 µs | 5430 | 30.0 / 23.8 MiB |
| frp v0.38.0 | tls | 3.81 Gbit/s | 193 µs | 242 µs | 4849 | 28.9 / 24.3 MiB |

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
