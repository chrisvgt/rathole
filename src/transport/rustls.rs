use crate::config::{TlsConfig, TransportConfig};
use crate::helper::host_port_pair;
use crate::transport::{AddrMaybeCached, SocketOpts, TcpTransport, Transport};
use std::fmt::Debug;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};
pub(crate) use tokio_rustls::TlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub struct TlsTransport {
    tcp: TcpTransport,
    config: TlsConfig,
    connector: Option<TlsConnector>,
    tls_acceptor: Option<TlsAcceptor>,
}

// workaround for TlsConnector and TlsAcceptor not implementing Debug
impl Debug for TlsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsTransport")
            .field("tcp", &self.tcp)
            .field("config", &self.config)
            .finish()
    }
}

/// Reads the private key and its certificate chain, leaf first, from a PKCS#12
/// archive. The MAC is verified, so a modified archive or a wrong password is
/// rejected. Archives from OpenSSL 3 (AES) and older (3DES) both work
fn load_pkcs12(
    path: &str,
    password: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivatePkcs8KeyDer<'static>)> {
    let buf = fs::read(path).with_context(|| format!("Failed to read {path}"))?;
    let store = KeyStore::from_pkcs12(&buf, password, Pkcs12ImportPolicy::Strict)
        .map_err(|e| anyhow!("Failed to read {path}, check the password: {e}"))?;
    let (_, chain) = store
        .private_key_chain()
        .with_context(|| format!("No private key with its certificate in {path}"))?;

    let certs = chain
        .certs()
        .iter()
        .map(|c| CertificateDer::from(c.as_der().to_vec()))
        .collect();
    let key = PrivatePkcs8KeyDer::from(chain.key().as_der().to_vec());
    Ok((certs, key))
}

fn load_server_config(config: &TlsConfig) -> Result<Option<ServerConfig>> {
    if let Some(pkcs12_path) = config.pkcs12.as_ref() {
        let pass = config
            .pkcs12_password
            .as_ref()
            .with_context(|| "Missing pkcs12_password")?;
        let (chain, key) = load_pkcs12(pkcs12_path, pass)?;

        Ok(Some(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key.into())?,
        ))
    } else {
        Ok(None)
    }
}

fn load_root_certs(config: &TlsConfig) -> Result<RootCertStore> {
    let mut root_certs = RootCertStore::empty();

    if let Some(path) = config.trusted_root.as_ref() {
        for cert in CertificateDer::pem_file_iter(path)
            .with_context(|| format!("Failed to open certificate {path}"))?
        {
            root_certs
                .add(cert.with_context(|| format!("Failed to parse certificate {path}"))?)
                .with_context(|| format!("Invalid certificate in {path}"))?;
        }
        if root_certs.is_empty() {
            bail!("No certificate found in {path}");
        }
    } else {
        // read from native
        let native = rustls_native_certs::load_native_certs();
        for e in &native.errors {
            eprintln!("Failed to load native certs: {}", e);
        }
        root_certs.add_parsable_certificates(native.certs);
    }

    Ok(root_certs)
}

fn load_client_config(config: &TlsConfig) -> Result<Option<ClientConfig>> {
    let root_certs = load_root_certs(config)?;
    if root_certs.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        ClientConfig::builder()
            .with_root_certificates(root_certs)
            .with_no_client_auth(),
    ))
}

#[async_trait]
impl Transport for TlsTransport {
    type Acceptor = TcpListener;
    type RawStream = TcpStream;
    type Stream = TlsStream<TcpStream>;

    fn new(config: &TransportConfig) -> Result<Self> {
        let tcp = TcpTransport::new(config)?;
        let config = config
            .tls
            .as_ref()
            .ok_or_else(|| anyhow!("Missing tls config"))?;

        let connector = load_client_config(config)?.map(|c| Arc::new(c).into());
        let tls_acceptor = load_server_config(config)?.map(|c| Arc::new(c).into());

        Ok(TlsTransport {
            tcp,
            config: config.clone(),
            connector,
            tls_acceptor,
        })
    }

    fn hint(conn: &Self::Stream, opt: SocketOpts) {
        opt.apply(conn.get_ref().0);
    }

    async fn bind<A: ToSocketAddrs + Send + Sync>(&self, addr: A) -> Result<Self::Acceptor> {
        let l = TcpListener::bind(addr)
            .await
            .with_context(|| "Failed to create tcp listener")?;
        Ok(l)
    }

    async fn accept(&self, a: &Self::Acceptor) -> Result<(Self::RawStream, SocketAddr)> {
        self.tcp
            .accept(a)
            .await
            .with_context(|| "Failed to accept TCP connection")
    }

    async fn handshake(&self, conn: Self::RawStream) -> Result<Self::Stream> {
        let conn = self
            .tls_acceptor
            .as_ref()
            .with_context(|| "TLS acceptor is not configured")?
            .accept(conn)
            .await?;
        Ok(tokio_rustls::TlsStream::Server(conn))
    }

    async fn connect(&self, addr: &AddrMaybeCached) -> Result<Self::Stream> {
        let conn = self.tcp.connect(addr).await?;

        let connector = self
            .connector
            .as_ref()
            .with_context(|| {
                "No trusted root certificate. Set `tls.trusted_root` or install the system CA certificates"
            })?;

        let host_name = self
            .config
            .hostname
            .as_deref()
            .unwrap_or(host_port_pair(&addr.addr)?.0);

        Ok(tokio_rustls::TlsStream::Client(
            connector
                .connect(ServerName::try_from(host_name)?.to_owned(), conn)
                .await?,
        ))
    }
}

pub(crate) fn get_tcpstream(s: &TlsStream<TcpStream>) -> &TcpStream {
    s.get_ref().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_all_trusted_roots() -> Result<()> {
        let mut bundle = fs::read("examples/tls/server.crt")?;
        bundle.extend(fs::read("examples/tls/rootCA.crt")?);
        let path = std::env::temp_dir().join(format!("rathole-bundle-{}.crt", std::process::id()));
        fs::write(&path, bundle)?;

        let config = TlsConfig {
            hostname: None,
            trusted_root: Some(path.to_string_lossy().into_owned()),
            pkcs12: None,
            pkcs12_password: None,
            pkcs12_password_file: None,
        };
        let roots = load_root_certs(&config);
        fs::remove_file(&path)?;

        assert_eq!(roots?.len(), 2);
        Ok(())
    }

    const PKCS12: &str = "examples/tls/identity.pfx";

    #[test]
    fn test_load_pkcs12() -> Result<()> {
        // The example uses 3DES like OpenSSL 1.1 and a SHA-256 MAC
        let (certs, _) = load_pkcs12(PKCS12, "1234")?;
        assert_eq!(certs.len(), 2);
        assert_eq!(
            certs[0].as_ref(),
            CertificateDer::from_pem_file("examples/tls/server.crt")?.as_ref()
        );

        // Written again with AES-256 like OpenSSL 3
        let store = KeyStore::from_pkcs12(&fs::read(PKCS12)?, "1234", Pkcs12ImportPolicy::Strict)?;
        let path = std::env::temp_dir().join(format!("rathole-aes-{}.pfx", std::process::id()));
        fs::write(&path, store.writer("1234").write()?)?;
        let loaded = load_pkcs12(path.to_str().unwrap(), "1234");
        fs::remove_file(&path)?;
        assert_eq!(loaded?.0, certs);
        Ok(())
    }

    #[test]
    fn test_reject_modified_pkcs12() -> Result<()> {
        assert!(load_pkcs12(PKCS12, "wrong").is_err());

        // Flip one bit in the MAC at the end of the archive
        let mut buf = fs::read(PKCS12)?;
        let last = buf.len() - 40;
        buf[last] ^= 1;
        let path =
            std::env::temp_dir().join(format!("rathole-modified-{}.pfx", std::process::id()));
        fs::write(&path, buf)?;
        let loaded = load_pkcs12(path.to_str().unwrap(), "1234");
        fs::remove_file(&path)?;
        let err = format!("{:#}", loaded.unwrap_err());
        assert!(err.contains("MAC"), "{}", err);
        Ok(())
    }
}
