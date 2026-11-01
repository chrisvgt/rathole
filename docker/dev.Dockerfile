# The rathole development/CI image.
#
# This is the toolchain container that the `docker.sh` wrapper and CI run
# everything in. It holds no source code: the repository is bind-mounted at
# runtime. It is intentionally fat (compilers, Zig, a full C toolchain) and is
# meant to be built once and reused.
#
# Build:  docker build -f docker/dev.Dockerfile -t rathole-dev:check .

FROM rust:1.99-trixie

# 1. Rust toolchain: clippy, rustfmt, and the native musl target.
RUN rustup component add clippy rustfmt \
    && rustup target add x86_64-unknown-linux-musl

# 2. System toolchain: C toolchain, TLS, musl, and general utilities.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git curl ca-certificates pkg-config libssl-dev musl-tools \
        python3 jq zip upx-ucl xz-utils \
    && rm -rf /var/lib/apt/lists/*

# 3. Zig, unpacked to /opt and symlinked into /usr/local/bin.
ARG ZIG_VERSION=0.14.1
RUN curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
        -o /tmp/zig.tar.xz \
    && tar -xJf /tmp/zig.tar.xz -C /opt \
    && ln -s "/opt/zig-x86_64-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig \
    && rm /tmp/zig.tar.xz

# 4. GitHub CLI.
RUN curl -fsSLo /tmp/gh.deb \
        "https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_linux_amd64.deb" \
    && apt-get install -y /tmp/gh.deb \
    && rm /tmp/gh.deb

# 5. Cargo helper crates. cargo-about only ships a CLI binary when its `cli`
#    feature is enabled, so it is installed separately.
RUN cargo install --locked --features cli cargo-about@0.9.3 \
    && cargo install --locked cargo-hack@0.6.46 cargo-zigbuild@0.24.0

# 6. Rust std for the cross-compiled musl targets. cargo-zigbuild links them
#    with Zig's musl, but still needs the target's prebuilt std from rustup.
RUN rustup target add aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
