pub const HASH_WIDTH_IN_BYTES: usize = 32;

use anyhow::{bail, Context, Result};
use bytes::{Bytes, BytesMut};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::trace;

type ProtocolVersion = u8;
const _PROTO_V0: u8 = 0u8;
const _PROTO_V1: u8 = 1u8;
const PROTO_V2: u8 = 2u8;

pub const CURRENT_PROTO_VERSION: ProtocolVersion = PROTO_V2;

pub type Digest = [u8; HASH_WIDTH_IN_BYTES];

// The names are part of the protocol documentation
#[allow(clippy::enum_variant_names)]
#[derive(Deserialize, Serialize, Debug)]
pub enum Hello {
    ControlChannelHello(ProtocolVersion, Digest), // sha256sum(service name) or a nonce
    DataChannelHello(ProtocolVersion, Digest),    // token provided by CreateDataChannel
    // sha256sum(service name) of a reverse service. Keep it last: postcard
    // encodes the variant index, so the others keep their encoding
    VisitorHello(ProtocolVersion, Digest),
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Auth(pub Digest);

#[derive(Deserialize, Serialize, Debug)]
pub enum Ack {
    Ok,
    ServiceNotExist,
    AuthFailed,
}

impl std::fmt::Display for Ack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Ack::Ok => "Ok",
                Ack::ServiceNotExist => "Service not exist",
                Ack::AuthFailed => "Incorrect token",
            }
        )
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub enum ControlChannelCmd {
    CreateDataChannel,
    HeartBeat,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum DataChannelCmd {
    StartForwardTcp,
    StartForwardUdp,
}

type UdpPacketLen = u16; // `u16` should be enough for any practical UDP traffic on the Internet
#[derive(Deserialize, Serialize, Debug)]
struct UdpHeader {
    from: SocketAddr,
    len: UdpPacketLen,
}

#[derive(Debug)]
pub struct UdpTraffic {
    pub from: SocketAddr,
    pub data: Bytes,
}

impl UdpTraffic {
    pub async fn write<T: AsyncWrite + Unpin>(&self, writer: &mut T) -> Result<()> {
        let hdr = UdpHeader {
            from: self.from,
            len: self.data.len() as UdpPacketLen,
        };

        let v = encode(&hdr)?;

        trace!("Write {:?} of length {}", hdr, v.len());
        writer.write_u8(v.len() as u8).await?;
        writer.write_all(&v).await?;

        writer.write_all(&self.data).await?;

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn write_slice<T: AsyncWrite + Unpin>(
        writer: &mut T,
        from: SocketAddr,
        data: &[u8],
    ) -> Result<()> {
        let hdr = UdpHeader {
            from,
            len: data.len() as UdpPacketLen,
        };

        let v = encode(&hdr)?;

        trace!("Write {:?} of length {}", hdr, v.len());
        writer.write_u8(v.len() as u8).await?;
        writer.write_all(&v).await?;

        writer.write_all(data).await?;

        Ok(())
    }

    pub async fn read<T: AsyncRead + Unpin>(reader: &mut T, hdr_len: u8) -> Result<UdpTraffic> {
        let mut buf = vec![0; hdr_len as usize];
        reader
            .read_exact(&mut buf)
            .await
            .with_context(|| "Failed to read udp header")?;

        let hdr: UdpHeader =
            postcard::from_bytes(&buf).with_context(|| "Failed to deserialize UdpHeader")?;

        trace!("hdr {:?}", hdr);

        let mut data = BytesMut::new();
        data.resize(hdr.len as usize, 0);
        reader.read_exact(&mut data).await?;

        Ok(UdpTraffic {
            from: hdr.from,
            data: data.freeze(),
        })
    }
}

/// Serialize a protocol message to its wire format
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(value)?)
}

pub fn digest(data: &[u8]) -> Digest {
    use sha2::{Digest, Sha256};
    let d = Sha256::new().chain_update(data).finalize();
    d.into()
}

struct PacketLength {
    hello: usize,
    ack: usize,
    auth: usize,
    c_cmd: usize,
    d_cmd: usize,
}

impl PacketLength {
    // Serializing these fixed-size messages cannot fail
    #[allow(clippy::unwrap_used)]
    pub fn new() -> PacketLength {
        let username = "default";
        let d = digest(username.as_bytes());
        let hello = postcard::to_stdvec(&Hello::ControlChannelHello(CURRENT_PROTO_VERSION, d))
            .unwrap()
            .len();
        let c_cmd = postcard::to_stdvec(&ControlChannelCmd::CreateDataChannel)
            .unwrap()
            .len();
        let d_cmd = postcard::to_stdvec(&DataChannelCmd::StartForwardTcp)
            .unwrap()
            .len();
        let ack = Ack::Ok;
        let ack = postcard::to_stdvec(&ack).unwrap().len();

        let auth = postcard::to_stdvec(&Auth(d)).unwrap().len();
        PacketLength {
            hello,
            ack,
            auth,
            c_cmd,
            d_cmd,
        }
    }
}

lazy_static! {
    static ref PACKET_LEN: PacketLength = PacketLength::new();
}

pub async fn read_hello<T: AsyncRead + AsyncWrite + Unpin>(conn: &mut T) -> Result<Hello> {
    let mut buf = vec![0u8; PACKET_LEN.hello];
    conn.read_exact(&mut buf)
        .await
        .with_context(|| "Failed to read hello")?;
    let hello = postcard::from_bytes(&buf).with_context(|| "Failed to deserialize hello")?;

    let (Hello::ControlChannelHello(v, _)
    | Hello::DataChannelHello(v, _)
    | Hello::VisitorHello(v, _)) = hello;
    if v != CURRENT_PROTO_VERSION {
        bail!(
            "Protocol version mismatched. Expected {}, got {}. Please update `rathole`.",
            CURRENT_PROTO_VERSION,
            v
        );
    }

    Ok(hello)
}

pub async fn read_auth<T: AsyncRead + AsyncWrite + Unpin>(conn: &mut T) -> Result<Auth> {
    let mut buf = vec![0u8; PACKET_LEN.auth];
    conn.read_exact(&mut buf)
        .await
        .with_context(|| "Failed to read auth")?;
    postcard::from_bytes(&buf).with_context(|| "Failed to deserialize auth")
}

pub async fn read_ack<T: AsyncRead + AsyncWrite + Unpin>(conn: &mut T) -> Result<Ack> {
    let mut bytes = vec![0u8; PACKET_LEN.ack];
    conn.read_exact(&mut bytes)
        .await
        .with_context(|| "Failed to read ack")?;
    postcard::from_bytes(&bytes).with_context(|| "Failed to deserialize ack")
}

pub async fn read_control_cmd<T: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut T,
) -> Result<ControlChannelCmd> {
    let mut bytes = vec![0u8; PACKET_LEN.c_cmd];
    conn.read_exact(&mut bytes)
        .await
        .with_context(|| "Failed to read cmd")?;
    postcard::from_bytes(&bytes).with_context(|| "Failed to deserialize control cmd")
}

pub async fn read_data_cmd<T: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut T,
) -> Result<DataChannelCmd> {
    let mut bytes = vec![0u8; PACKET_LEN.d_cmd];
    conn.read_exact(&mut bytes)
        .await
        .with_context(|| "Failed to read cmd")?;
    postcard::from_bytes(&bytes).with_context(|| "Failed to deserialize data cmd")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that has `hello` ready to be read
    async fn piped(hello: &Hello) -> Result<tokio::io::DuplexStream> {
        let (a, mut b) = tokio::io::duplex(256);
        b.write_all(&encode(hello)?).await?;
        Ok(a)
    }

    #[test]
    fn test_hello_length_is_the_same_for_all_variants() -> Result<()> {
        let d = digest(b"service");
        for hello in [
            Hello::ControlChannelHello(CURRENT_PROTO_VERSION, d),
            Hello::DataChannelHello(CURRENT_PROTO_VERSION, d),
            Hello::VisitorHello(CURRENT_PROTO_VERSION, d),
            Hello::VisitorHello(CURRENT_PROTO_VERSION, [0xff; HASH_WIDTH_IN_BYTES]),
        ] {
            assert_eq!(encode(&hello)?.len(), PACKET_LEN.hello, "{:?}", hello);
        }
        Ok(())
    }

    #[test]
    fn test_existing_hello_encoding_is_unchanged() -> Result<()> {
        let d = [7u8; HASH_WIDTH_IN_BYTES];
        // The first byte is the variant index
        assert_eq!(encode(&Hello::ControlChannelHello(2, d))?[..2], [0, 2]);
        assert_eq!(encode(&Hello::DataChannelHello(2, d))?[..2], [1, 2]);
        assert_eq!(encode(&Hello::VisitorHello(2, d))?[..2], [2, 2]);
        Ok(())
    }

    #[tokio::test]
    async fn test_read_hello_checks_the_version_of_every_variant() -> Result<()> {
        let d = digest(b"service");
        for hello in [
            Hello::ControlChannelHello(CURRENT_PROTO_VERSION + 1, d),
            Hello::DataChannelHello(CURRENT_PROTO_VERSION + 1, d),
            Hello::VisitorHello(CURRENT_PROTO_VERSION + 1, d),
        ] {
            assert!(
                read_hello(&mut piped(&hello).await?).await.is_err(),
                "{:?}",
                hello
            );
        }
        let hello = Hello::VisitorHello(CURRENT_PROTO_VERSION, d);
        assert!(matches!(
            read_hello(&mut piped(&hello).await?).await?,
            Hello::VisitorHello(_, v) if v == d
        ));
        Ok(())
    }
}
