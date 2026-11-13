# Build Guide

`rathole` is built and tested inside Docker. The host needs no Rust toolchain, C compiler or other build dependencies: only Docker.

## Requirements

- Docker Engine or Docker Desktop with Linux containers.
- A 64-bit x86 Linux host. The development image is `linux/amd64`; running it on an arm64 host through emulation is untested and not supported.
- The commands below are run from the root of the checkout.

The dev image itself is built from [`docker/dev.Dockerfile`](../docker/dev.Dockerfile). It is fairly large (Rust, clippy, rustfmt, Zig, a C toolchain, `cargo-about`, `cargo-hack`, `cargo-zigbuild`, `upx` and the musl targets), so it is meant to be built once and reused.

## Quick start

```sh
./docker.sh build
./docker.sh test
./docker.sh lint
./docker.sh fmt
```

`docker.sh` builds the dev image on first use. That first build downloads the base image and the toolchain and can take several minutes; later commands reuse the image. The image tag contains a checksum of `docker/dev.Dockerfile`, so editing the Dockerfile automatically produces a new image on the next command. `./docker.sh rebuild` forces a rebuild and `RATHOLE_DEV_IMAGE=name:tag` overrides the tag.

Every command runs as the host user with the checkout bind-mounted at its absolute path. Cargo build artifacts stay in `target/` in the checkout, while `CARGO_HOME` (the crate registry, git dependencies and tool caches) lives in the persistent named volume `rathole-cargo-home`. Both survive between invocations; `.docker-cargo-home-uid` records the volume's owner and is gitignored along with `target/`.

## Commands

| Command | Description |
|---|---|
| `./docker.sh build [args...]` | `cargo build --locked`, forwarding extra arguments |
| `./docker.sh test [args...]` | Run the test suites, or a single `cargo test` with arguments |
| `./docker.sh lint` | Clippy on the default and native-tls feature sets, generate third-party licenses, check the feature powerset |
| `./docker.sh fmt` | Format the code (`fmt --check` to verify without writing) |
| `./docker.sh bench [args...]` | Run the tunnel benchmark |
| `./docker.sh release [--target T]` | Build release archives into `dist/` |
| `./docker.sh image` | Build the runtime image as `rathole:local` |
| `./docker.sh run -- CMD...` | Run an arbitrary command in the dev container |
| `./docker.sh shell` | Open a shell in the dev container |
| `./docker.sh clean` | Run `cargo clean` and remove `dist/` |
| `./docker.sh rebuild` | Force a rebuild of the dev image |

With no arguments, `./docker.sh test` runs the default test suite, the tests with the `native-tls`/`hot-reload` features, and a release-mode test of the static musl build. Arguments after `test` are passed to a single `cargo test` invocation.

`./docker.sh clean` keeps the dev image and the cargo cache. To also reset the cache, remove the volume and its owner marker:

```sh
docker volume rm rathole-cargo-home
rm -f .docker-cargo-home-uid
```

## Releasing

```sh
./docker.sh release
```

This builds and packages all supported targets and writes `dist/rathole-<target>.zip` plus a matching `.sha256` file for each. Build one target only with:

```sh
./docker.sh release --target aarch64-unknown-linux-musl
```

Each archive contains the `rathole` binary, `LICENSE`, `NOTICE` and `THIRD-PARTY-LICENSES.txt`. UPX is applied on a best-effort basis to shrink the binary. All of the packaging happens inside the container, so the host only needs Docker.

The supported release targets are:

| Target | Notes |
|---|---|
| `x86_64-unknown-linux-gnu` | Default features, dynamically linked against glibc |
| `x86_64-unknown-linux-musl` | `embedded` features, statically linked |
| `aarch64-unknown-linux-musl` | `embedded` features, cross-compiled with Zig |
| `armv7-unknown-linux-musleabihf` | `embedded` features, cross-compiled with Zig |

### Linux only

Releases are Linux-only: there are no macOS or Windows artifacts, and the Docker workflow targets `linux/amd64`. The `gnu` build covers glibc systems; the `musl` builds are static and cover x86_64, aarch64 and armv7.

Before packaging, the two x86_64 targets also run their release-mode tests, which therefore execute on the build host. The cross-compiled aarch64 and armv7 targets are built and packaged but not runtime-tested: the release flow has no emulation, so for them a successful cross-compile is the only check.

## Customize the build

`rathole` comes with lots of *crate features* that determine whether a certain feature will be compiled or not. Supported features can be checked out in `[features]` of [Cargo.toml](../Cargo.toml).

For example, to build `rathole` with the `client` and `noise` feature:

```sh
./docker.sh build --release --no-default-features --features client,noise
```

Extra arguments are passed straight to `cargo build`, so any cargo flag or profile works. Use `./docker.sh run -- CMD...` for other cargo commands.

## TLS backends

TLS uses `rustls` by default, which needs no system library. To use the OpenSSL of the system instead, replace `rustls` and `websocket-rustls` with `native-tls` and `websocket-native-tls`:

```sh
./docker.sh build --release --no-default-features --features server,client,native-tls,noise,websocket-native-tls,hot-reload
```

The dev image already includes the OpenSSL development files. The `rustls` and `native-tls` features are mutually exclusive, enabling both is a compile error.

## Minimalize the binary

1. Build with the `minimal` profile.

The `release` build profile optimizes for program running time, not binary size. The `minimal` profile enables lots of optimization for size to produce a much smaller binary.

For example, to build `rathole` with the `client` feature with the `minimal` profile:

```sh
./docker.sh build --profile minimal --no-default-features --features client
```

2. `strip` and `upx`

The binary that step 1 produces can be even smaller by using `strip` and `upx` to remove the symbols and compress the binary:

```sh
./docker.sh run sh -c 'strip target/minimal/rathole && upx --best --lzma target/minimal/rathole'
```

At the time of writing the build guide, the produced binary for `x86_64-unknown-linux-gnu` has the size of **574 KiB**, while `frpc` has the size of **~10 MiB**, which is much larger.
