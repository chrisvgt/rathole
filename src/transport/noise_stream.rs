//! Encrypted stream for the noise transport.
//!
//! Every Noise message, during the handshake and afterwards, is sent as a
//! 2-byte little-endian length followed by the message. This is the framing
//! of snowstorm 0.4, which rathole used up to v0.6.3, so both sides stay
//! compatible.
//!
//! Buffers grow with the messages that actually arrive instead of being
//! allocated at the maximum size for every connection.
//!
//! The framing and the handshake loop follow snowstorm by black-binary
//! (https://github.com/black-binary/snowstorm), licensed under the Apache
//! License, Version 2.0.

use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use snow::{HandshakeState, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

const LEN_FIELD: usize = 2;
const MAX_MESSAGE_LEN: usize = u16::MAX as usize;
const TAG_LEN: usize = 16;
const MAX_PAYLOAD_LEN: usize = MAX_MESSAGE_LEN - TAG_LEN;

enum ReadState {
    Len { buf: [u8; LEN_FIELD], filled: usize },
    Message { filled: usize },
    Payload { pos: usize },
    Eof,
}

pub struct NoiseStream<T> {
    inner: T,
    transport: TransportState,
    read_state: ReadState,
    /// Encrypted message being read
    read_buf: Vec<u8>,
    /// Decrypted payload not yet handed to the reader
    payload: Vec<u8>,
    /// Encrypted message with its length, not yet fully written
    write_buf: Vec<u8>,
    write_pos: usize,
}

fn invalid_data(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl<T> NoiseStream<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn handshake(mut inner: T, mut state: HandshakeState) -> io::Result<Self> {
        let mut message = vec![0u8; MAX_MESSAGE_LEN];
        let mut payload = vec![0u8; MAX_MESSAGE_LEN];
        while !state.is_handshake_finished() {
            if state.is_my_turn() {
                let len = state
                    .write_message(&[], &mut message)
                    .map_err(invalid_data)?;
                inner.write_u16_le(len as u16).await?;
                inner.write_all(&message[..len]).await?;
                inner.flush().await?;
            } else {
                let len = inner.read_u16_le().await? as usize;
                inner.read_exact(&mut message[..len]).await?;
                state
                    .read_message(&message[..len], &mut payload)
                    .map_err(invalid_data)?;
            }
        }

        Ok(NoiseStream {
            inner,
            transport: state.into_transport_mode().map_err(invalid_data)?,
            read_state: ReadState::Len {
                buf: [0; LEN_FIELD],
                filled: 0,
            },
            read_buf: Vec::new(),
            payload: Vec::new(),
            write_buf: Vec::new(),
            write_pos: 0,
        })
    }
}

impl<T> NoiseStream<T> {
    pub fn get_inner(&self) -> &T {
        &self.inner
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for NoiseStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseStream")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<T: AsyncWrite + Unpin> NoiseStream<T> {
    /// Writes the pending encrypted message to the inner stream
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.write_pos < self.write_buf.len() {
            let n = ready!(
                Pin::new(&mut self.inner).poll_write(cx, &self.write_buf[self.write_pos..])
            )?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.write_pos += n;
        }
        self.write_buf.clear();
        self.write_pos = 0;
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for NoiseStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match &mut this.read_state {
                ReadState::Eof => return Poll::Ready(Ok(())),
                ReadState::Len { buf, filled } => {
                    if *filled == LEN_FIELD {
                        let len = u16::from_le_bytes(*buf) as usize;
                        this.read_buf.clear();
                        this.read_buf.resize(len, 0);
                        this.read_state = ReadState::Message { filled: 0 };
                        continue;
                    }
                    let mut rb = ReadBuf::new(&mut buf[*filled..]);
                    ready!(Pin::new(&mut this.inner).poll_read(cx, &mut rb))?;
                    let n = rb.filled().len();
                    if n == 0 {
                        if *filled == 0 {
                            this.read_state = ReadState::Eof;
                            continue;
                        }
                        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                    }
                    *filled += n;
                }
                ReadState::Message { filled } => {
                    if *filled == this.read_buf.len() {
                        this.payload.clear();
                        this.payload.resize(this.read_buf.len(), 0);
                        let n = this
                            .transport
                            .read_message(&this.read_buf, &mut this.payload)
                            .map_err(invalid_data)?;
                        this.payload.truncate(n);
                        this.read_state = ReadState::Payload { pos: 0 };
                        continue;
                    }
                    let mut rb = ReadBuf::new(&mut this.read_buf[*filled..]);
                    ready!(Pin::new(&mut this.inner).poll_read(cx, &mut rb))?;
                    let n = rb.filled().len();
                    if n == 0 {
                        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                    }
                    *filled += n;
                }
                ReadState::Payload { pos } => {
                    let rest = &this.payload[*pos..];
                    if rest.is_empty() {
                        this.read_state = ReadState::Len {
                            buf: [0; LEN_FIELD],
                            filled: 0,
                        };
                        continue;
                    }
                    let n = rest.len().min(out.remaining());
                    out.put_slice(&rest[..n]);
                    *pos += n;
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for NoiseStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        // The previous message must be out before the next one is encrypted
        ready!(this.poll_drain(cx))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let payload = &buf[..buf.len().min(MAX_PAYLOAD_LEN)];
        this.write_buf
            .resize(LEN_FIELD + payload.len() + TAG_LEN, 0);
        let len = this
            .transport
            .write_message(payload, &mut this.write_buf[LEN_FIELD..])
            .map_err(invalid_data)?;
        this.write_buf[..LEN_FIELD].copy_from_slice(&(len as u16).to_le_bytes());
        this.write_buf.truncate(LEN_FIELD + len);

        // The payload is taken over now. What the inner stream does not
        // accept right away goes out with the next write, flush or shutdown
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(payload.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow::Builder;
    use tokio::io::{duplex, DuplexStream};

    const PATTERN: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";

    fn states() -> anyhow::Result<(HandshakeState, HandshakeState)> {
        let key = Builder::new(PATTERN.parse()?).generate_keypair()?;
        let initiator = Builder::new(PATTERN.parse()?)
            .remote_public_key(&key.public)?
            .build_initiator()?;
        let responder = Builder::new(PATTERN.parse()?)
            .local_private_key(&key.private)?
            .build_responder()?;
        Ok((initiator, responder))
    }

    /// The same as `states`, but the side of snowstorm uses the snow 0.9 it was
    /// built with
    fn mixed_states(
        snowstorm_is_initiator: bool,
    ) -> anyhow::Result<(HandshakeState, snowstorm::snow::HandshakeState)> {
        use snowstorm::snow::Builder as OldBuilder;
        let key = Builder::new(PATTERN.parse()?).generate_keypair()?;
        if snowstorm_is_initiator {
            let theirs = OldBuilder::new(PATTERN.parse()?)
                .remote_public_key(&key.public)
                .build_initiator()?;
            let ours = Builder::new(PATTERN.parse()?)
                .local_private_key(&key.private)?
                .build_responder()?;
            Ok((ours, theirs))
        } else {
            let ours = Builder::new(PATTERN.parse()?)
                .remote_public_key(&key.public)?
                .build_initiator()?;
            let theirs = OldBuilder::new(PATTERN.parse()?)
                .local_private_key(&key.private)
                .build_responder()?;
            Ok((ours, theirs))
        }
    }

    async fn pair() -> anyhow::Result<(NoiseStream<DuplexStream>, NoiseStream<DuplexStream>)> {
        let (initiator, responder) = states()?;
        // A small pipe, so writes have to wait for the reader
        let (a, b) = duplex(1024);
        let (a, b) = tokio::join!(
            NoiseStream::handshake(a, initiator),
            NoiseStream::handshake(b, responder)
        );
        Ok((a?, b?))
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[tokio::test]
    async fn test_transfer_both_ways() -> anyhow::Result<()> {
        let (mut a, mut b) = pair().await?;
        // Larger than one Noise message
        let sent = data(300_000);

        let expected = sent.clone();
        let reader = tokio::spawn(async move {
            let mut got = vec![0u8; expected.len()];
            b.read_exact(&mut got).await?;
            assert_eq!(got, expected);
            b.write_all(b"done").await?;
            b.shutdown().await?;
            anyhow::Ok(())
        });

        a.write_all(&sent).await?;
        a.flush().await?;
        let mut reply = Vec::new();
        a.read_to_end(&mut reply).await?;
        assert_eq!(reply, b"done");
        reader.await??;
        Ok(())
    }

    #[tokio::test]
    async fn test_idle_buffers_stay_small() -> anyhow::Result<()> {
        let (mut a, mut b) = pair().await?;
        a.write_all(b"x").await?;
        a.flush().await?;
        let mut buf = [0u8; 1];
        b.read_exact(&mut buf).await?;

        for s in [&a, &b] {
            assert!(s.read_buf.capacity() < 1024);
            assert!(s.payload.capacity() < 1024);
            assert!(s.write_buf.capacity() < 1024);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_truncated_message_is_an_error() -> anyhow::Result<()> {
        let (initiator, responder) = states()?;
        let (a, mut b) = duplex(1024);
        let responder = tokio::spawn(NoiseStream::handshake(a, responder));

        // Do the handshake by hand, then send half a message
        let mut init = initiator;
        let mut msg = vec![0u8; MAX_MESSAGE_LEN];
        let len = init.write_message(&[], &mut msg)?;
        b.write_u16_le(len as u16).await?;
        b.write_all(&msg[..len]).await?;
        let len = b.read_u16_le().await? as usize;
        let mut reply = vec![0u8; len];
        b.read_exact(&mut reply).await?;
        let mut payload = vec![0u8; MAX_MESSAGE_LEN];
        init.read_message(&reply, &mut payload)?;
        let mut transport = init.into_transport_mode()?;
        let len = transport.write_message(b"hello", &mut msg)?;
        b.write_u16_le(len as u16).await?;
        b.write_all(&msg[..len / 2]).await?;
        drop(b);

        let mut a = responder.await??;
        let mut got = Vec::new();
        let err = a.read_to_end(&mut got).await.err();
        assert_eq!(err.map(|e| e.kind()), Some(io::ErrorKind::UnexpectedEof));
        Ok(())
    }

    /// rathole up to v0.6.3 used snowstorm. Both must understand each other
    #[tokio::test]
    async fn test_compatible_with_snowstorm() -> anyhow::Result<()> {
        for snowstorm_is_initiator in [true, false] {
            let (our_state, their_state) = mixed_states(snowstorm_is_initiator)?;
            let (a, b) = duplex(1024);
            let (ours, theirs) = tokio::join!(
                NoiseStream::handshake(a, our_state),
                snowstorm::NoiseStream::handshake(b, their_state)
            );
            let (mut ours, mut theirs) = (ours?, theirs?);

            let sent = data(200_000);
            let expected = sent.clone();
            let reader = tokio::spawn(async move {
                let mut got = vec![0u8; expected.len()];
                theirs.read_exact(&mut got).await?;
                assert_eq!(got, expected);
                theirs.write_all(&expected).await?;
                anyhow::Ok(())
            });
            ours.write_all(&sent).await?;
            ours.flush().await?;
            let mut back = vec![0u8; sent.len()];
            ours.read_exact(&mut back).await?;
            assert_eq!(back, sent);
            reader.await??;
        }
        Ok(())
    }
}
