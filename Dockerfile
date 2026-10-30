# Build:  docker build -t rathole .
#         docker build -t rathole --build-arg FEATURES=server,noise .
# Run:    docker run -d --network host -v ./server.toml:/app/config.toml:ro rathole config.toml
#
# The config file must be readable by UID 1000.

FROM rust:1.99-trixie AS builder
WORKDIR /src
COPY . .
ARG FEATURES=default
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --no-default-features --features "$FEATURES" \
    && cp target/release/rathole /rathole

# Contains glibc, libgcc and OpenSSL for native-tls
FROM gcr.io/distroless/cc-debian13
WORKDIR /app
COPY --from=builder /rathole /usr/local/bin/rathole
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/rathole"]
