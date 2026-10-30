# Build:  docker build -t rathole .
#         docker build -t rathole --build-arg FEATURES=server,noise .
# Image:  ghcr.io/chrisvgt/rathole, published with every release
# Run:    docker run -d --network host -v ./server.toml:/app/config.toml:ro rathole config.toml
#
# The config file must be readable by UID 1000.

FROM rust:1.99-trixie AS builder
WORKDIR /src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked cargo-about@0.9.3
COPY . .
ARG FEATURES=default
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --no-default-features --features "$FEATURES" \
    && cp target/release/rathole /rathole
# The Apache License requires a copy of the license with every binary, and the
# licenses of the crates compiled into it require theirs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    mkdir /licenses \
    && cp LICENSE NOTICE /licenses/ \
    && cargo about generate --no-default-features --features "$FEATURES" \
        about.hbs -o /licenses/THIRD-PARTY-LICENSES.txt

# Contains glibc, libgcc and OpenSSL for native-tls
FROM gcr.io/distroless/cc-debian13
WORKDIR /app
COPY --from=builder /licenses /usr/share/doc/rathole
COPY --from=builder /rathole /usr/local/bin/rathole
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/rathole"]
