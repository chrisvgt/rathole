use std::net::SocketAddr;

use super::noise_stream::NoiseStream;
use super::{AddrMaybeCached, SocketOpts, TcpTransport, Transport};
use crate::config::{NoiseConfig, TransportConfig};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use snow::{
    params::{DHChoice, NoiseParams},
    Builder,
};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};

/// Length of X25519 private and public keys in bytes
pub(crate) const NOISE_KEY_LEN: usize = 32;

/// Parse the pattern and reject DH functions snow cannot implement
pub(crate) fn parse_noise_params(pattern: &str) -> Result<NoiseParams> {
    let params: NoiseParams = pattern
        .parse()
        .with_context(|| format!("Invalid noise pattern `{pattern}`"))?;
    if params.dh != DHChoice::Curve25519 {
        bail!("Unsupported DH function in noise pattern `{pattern}`. Only 25519 is supported");
    }
    Ok(params)
}

/// Decode a base64 key and check that it has the length snow expects
pub(crate) fn decode_noise_key(value: &str, field: &str) -> Result<Vec<u8>> {
    let key = BASE64
        .decode(value)
        .with_context(|| format!("Failed to decode `noise.{field}`"))?;
    if key.len() != NOISE_KEY_LEN {
        bail!(
            "Invalid `noise.{field}`: expected {NOISE_KEY_LEN} bytes, got {}. Generate a key with `--genkey`",
            key.len()
        );
    }
    Ok(key)
}

pub struct NoiseTransport {
    tcp: TcpTransport,
    config: NoiseConfig,
    params: NoiseParams,
    local_private_key: Vec<u8>,
    remote_public_key: Option<Vec<u8>>,
}

impl std::fmt::Debug for NoiseTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{:?}", self.config)
    }
}

impl NoiseTransport {
    fn builder(&self) -> Result<Builder<'_>> {
        let builder =
            Builder::new(self.params.clone()).local_private_key(&self.local_private_key)?;
        Ok(match &self.remote_public_key {
            Some(x) => builder.remote_public_key(x)?,
            None => builder,
        })
    }
}

#[async_trait]
impl Transport for NoiseTransport {
    type Acceptor = TcpListener;
    type RawStream = TcpStream;
    type Stream = NoiseStream<TcpStream>;

    fn new(config: &TransportConfig) -> Result<Self> {
        let tcp = TcpTransport::new(config)?;

        let config = match &config.noise {
            Some(v) => v.clone(),
            None => return Err(anyhow!("Missing noise config")),
        };
        let builder = Builder::new(parse_noise_params(&config.pattern)?);

        let remote_public_key = match &config.remote_public_key {
            Some(x) => Some(decode_noise_key(x, "remote_public_key")?),
            None => None,
        };

        let local_private_key = match &config.local_private_key {
            Some(x) => decode_noise_key(x, "local_private_key")?,
            None => builder.generate_keypair()?.private,
        };

        let params = parse_noise_params(&config.pattern)?;

        Ok(NoiseTransport {
            tcp,
            config,
            params,
            local_private_key,
            remote_public_key,
        })
    }

    fn hint(conn: &Self::Stream, opt: SocketOpts) {
        opt.apply(conn.get_inner());
    }

    async fn bind<T: ToSocketAddrs + Send + Sync>(&self, addr: T) -> Result<Self::Acceptor> {
        Ok(TcpListener::bind(addr).await?)
    }

    async fn accept(&self, a: &Self::Acceptor) -> Result<(Self::RawStream, SocketAddr)> {
        self.tcp
            .accept(a)
            .await
            .with_context(|| "Failed to accept TCP connection")
    }

    async fn handshake(&self, conn: Self::RawStream) -> Result<Self::Stream> {
        let conn = NoiseStream::handshake(conn, self.builder()?.build_responder()?)
            .await
            .with_context(|| "Failed to do noise handshake")?;
        Ok(conn)
    }

    async fn connect(&self, addr: &AddrMaybeCached) -> Result<Self::Stream> {
        let conn = self
            .tcp
            .connect(addr)
            .await
            .with_context(|| "Failed to connect TCP socket")?;

        let conn = NoiseStream::handshake(conn, self.builder()?.build_initiator()?)
            .await
            .with_context(|| "Failed to do noise handshake")?;
        return Ok(conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_noise_key() {
        assert_eq!(
            decode_noise_key(&BASE64.encode([7u8; 32]), "local_private_key")
                .unwrap()
                .len(),
            32
        );
        let e = decode_noise_key(&BASE64.encode([7u8; 31]), "local_private_key").unwrap_err();
        assert!(format!("{e:#}").contains("Invalid `noise.local_private_key`: expected 32 bytes, got 31"));
        // Longer than 56 bytes used to panic in snow on every connection
        let e = decode_noise_key(&BASE64.encode([7u8; 64]), "remote_public_key").unwrap_err();
        assert!(format!("{e:#}").contains("got 64"));
        let e = decode_noise_key("not base64!", "remote_public_key").unwrap_err();
        assert!(format!("{e:#}").contains("Failed to decode `noise.remote_public_key`"));
    }

    #[test]
    fn test_parse_noise_params() {
        assert!(parse_noise_params("Noise_NK_25519_ChaChaPoly_BLAKE2s").is_ok());
        let e = parse_noise_params("Noise_NK_448_ChaChaPoly_BLAKE2s").unwrap_err();
        assert!(format!("{e:#}").contains("Only 25519 is supported"));
        assert!(parse_noise_params("garbage").is_err());
    }
}
