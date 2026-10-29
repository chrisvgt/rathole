use anyhow::{anyhow, bail, Context, Result};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::net::SocketAddr;
use std::ops::{Deref, RangeInclusive};
use std::path::Path;
use tokio::fs;
use url::Url;

use crate::transport::{DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_KEEPALIVE_SECS, DEFAULT_NODELAY};

/// Application-layer heartbeat interval in secs
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;
const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 40;

/// Client
pub const DEFAULT_CLIENT_RETRY_INTERVAL_SECS: u64 = 1;

/// String with Debug implementation that emits "MASKED"
/// Used to mask sensitive strings when logging
#[derive(Serialize, Deserialize, Default, PartialEq, Eq, Clone)]
pub struct MaskedString(String);

impl Debug for MaskedString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        f.write_str("MASKED")
    }
}

impl Deref for MaskedString {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<&str> for MaskedString {
    fn from(s: &str) -> MaskedString {
        MaskedString(String::from(s))
    }
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone, PartialEq, Eq, Default)]
pub enum TransportType {
    #[default]
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "tls")]
    Tls,
    #[serde(rename = "noise")]
    Noise,
    #[serde(rename = "websocket")]
    Websocket,
}

/// Per service config
/// All Option are optional in configuration but must be Some value in runtime
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ClientServiceConfig {
    #[serde(rename = "type", default = "default_service_type")]
    pub service_type: ServiceType,
    #[serde(skip)]
    pub name: String,
    pub local_addr: String,
    #[serde(default)] // Default to false
    pub prefer_ipv6: bool,
    pub token: Option<MaskedString>,
    pub nodelay: Option<bool>,
    pub retry_interval: Option<u64>,
}

impl ClientServiceConfig {
    pub fn with_name(name: &str) -> ClientServiceConfig {
        ClientServiceConfig {
            name: name.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceType {
    #[serde(rename = "tcp")]
    #[default]
    Tcp,
    #[serde(rename = "udp")]
    Udp,
}

fn default_service_type() -> ServiceType {
    Default::default()
}

/// Most ports one service may cover. Every port becomes a service of its own
/// with its own control channel
const MAX_PORT_RANGE: usize = 100;

/// Splits `host:first-last` into the host and the ports. Addresses without a
/// range give `None`
fn port_range(addr: &str) -> Result<Option<(&str, RangeInclusive<u16>)>> {
    let Some((host, ports)) = addr.rsplit_once(':') else {
        return Ok(None);
    };
    let Some((first, last)) = ports.split_once('-') else {
        return Ok(None);
    };
    let parse = |port: &str| {
        port.parse::<u16>()
            .with_context(|| format!("Invalid port range in `{}`", addr))
    };
    let (first, last) = (parse(first)?, parse(last)?);
    if first > last {
        bail!("The port range in `{}` ends before it starts", addr);
    }
    if usize::from(last - first) >= MAX_PORT_RANGE {
        bail!(
            "The port range in `{}` has more than {} ports",
            addr,
            MAX_PORT_RANGE
        );
    }
    Ok(Some((host, first..=last)))
}

/// Splits a service with a port range or `type = "tcp+udp"` into one service
/// per port and protocol, e.g. `game` into `game#0/tcp`, `game#0/udp`,
/// `game#1/tcp` and so on. Server and client name the parts the same way, so
/// the protocol does not change.
fn expand_service(
    name: &str,
    table: toml::Table,
    addr_key: &str,
) -> Result<Vec<(String, toml::Table)>> {
    let types = match table.get("type").and_then(|v| v.as_str()) {
        Some("tcp+udp") => vec![Some("tcp"), Some("udp")],
        _ => vec![None],
    };
    let addrs = match table.get(addr_key).and_then(|v| v.as_str()) {
        Some(addr) => match port_range(addr)? {
            Some((host, ports)) => ports
                .enumerate()
                .map(|(i, port)| {
                    (
                        format!("{}#{}", name, i),
                        Some(format!("{}:{}", host, port)),
                    )
                })
                .collect(),
            None => vec![(name.to_string(), None)],
        },
        None => vec![(name.to_string(), None)],
    };

    let mut services = Vec::new();
    for (part, addr) in addrs {
        for service_type in &types {
            let mut table = table.clone();
            let mut name = part.clone();
            if let Some(addr) = &addr {
                table.insert(addr_key.to_string(), addr.as_str().into());
            }
            if let Some(service_type) = service_type {
                table.insert("type".to_string(), (*service_type).into());
                name = format!("{}/{}", name, service_type);
            }
            services.push((name, table));
        }
    }
    Ok(services)
}

fn expand_services<'de, D, T>(
    deserializer: D,
    addr_key: &str,
) -> std::result::Result<HashMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let mut services = HashMap::new();
    for (name, table) in HashMap::<String, toml::Table>::deserialize(deserializer)? {
        let parts = expand_service(&name, table, addr_key)
            .map_err(|e| D::Error::custom(format!("service `{}`: {:#}", name, e)))?;
        for (part, table) in parts {
            let service = toml::Value::Table(table)
                .try_into()
                .map_err(|e| D::Error::custom(format!("service `{}`: {}", name, e)))?;
            if services.insert(part.clone(), service).is_some() {
                return Err(D::Error::custom(format!(
                    "service `{}` is defined twice",
                    part
                )));
            }
        }
    }
    Ok(services)
}

fn server_services<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<HashMap<String, ServerServiceConfig>, D::Error> {
    expand_services(deserializer, "bind_addr")
}

fn client_services<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<HashMap<String, ClientServiceConfig>, D::Error> {
    expand_services(deserializer, "local_addr")
}

/// Whether two services would bind the same port
fn same_port(a: &ServerServiceConfig, b: &ServerServiceConfig) -> bool {
    if a.service_type != b.service_type {
        return false;
    }
    match (
        a.bind_addr.parse::<SocketAddr>(),
        b.bind_addr.parse::<SocketAddr>(),
    ) {
        (Ok(x), Ok(y)) => {
            x.port() == y.port()
                && (x.ip() == y.ip() || x.ip().is_unspecified() || y.ip().is_unspecified())
        }
        _ => a.bind_addr == b.bind_addr,
    }
}

/// Per service config
/// All Option are optional in configuration but must be Some value in runtime
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ServerServiceConfig {
    #[serde(rename = "type", default = "default_service_type")]
    pub service_type: ServiceType,
    #[serde(skip)]
    pub name: String,
    pub bind_addr: String,
    pub token: Option<MaskedString>,
    pub nodelay: Option<bool>,
}

impl ServerServiceConfig {
    pub fn with_name(name: &str) -> ServerServiceConfig {
        ServerServiceConfig {
            name: name.to_string(),
            ..Default::default()
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub hostname: Option<String>,
    pub trusted_root: Option<String>,
    pub pkcs12: Option<String>,
    pub pkcs12_password: Option<MaskedString>,
}

fn default_noise_pattern() -> String {
    String::from("Noise_NK_25519_ChaChaPoly_BLAKE2s")
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoiseConfig {
    #[serde(default = "default_noise_pattern")]
    pub pattern: String,
    pub local_private_key: Option<MaskedString>,
    pub remote_public_key: Option<String>,
    // TODO: Maybe psk can be added
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebsocketConfig {
    pub tls: bool,
}

fn default_nodelay() -> bool {
    DEFAULT_NODELAY
}

fn default_keepalive_secs() -> u64 {
    DEFAULT_KEEPALIVE_SECS
}

fn default_keepalive_interval() -> u64 {
    DEFAULT_KEEPALIVE_INTERVAL
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TcpConfig {
    #[serde(default = "default_nodelay")]
    pub nodelay: bool,
    #[serde(default = "default_keepalive_secs")]
    pub keepalive_secs: u64,
    #[serde(default = "default_keepalive_interval")]
    pub keepalive_interval: u64,
    pub proxy: Option<Url>,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            nodelay: default_nodelay(),
            keepalive_secs: default_keepalive_secs(),
            keepalive_interval: default_keepalive_interval(),
            proxy: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    #[serde(rename = "type")]
    pub transport_type: TransportType,
    #[serde(default)]
    pub tcp: TcpConfig,
    pub tls: Option<TlsConfig>,
    pub noise: Option<NoiseConfig>,
    pub websocket: Option<WebsocketConfig>,
}

fn default_heartbeat_timeout() -> u64 {
    DEFAULT_HEARTBEAT_TIMEOUT_SECS
}

fn default_client_retry_interval() -> u64 {
    DEFAULT_CLIENT_RETRY_INTERVAL_SECS
}

#[derive(Debug, Serialize, Deserialize, Default, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub remote_addr: String,
    pub default_token: Option<MaskedString>,
    pub prefer_ipv6: Option<bool>,
    #[serde(deserialize_with = "client_services")]
    pub services: HashMap<String, ClientServiceConfig>,
    #[serde(default)]
    pub transport: TransportConfig,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u64,
    #[serde(default = "default_client_retry_interval")]
    pub retry_interval: u64,
}

fn default_heartbeat_interval() -> u64 {
    DEFAULT_HEARTBEAT_INTERVAL_SECS
}

#[derive(Debug, Serialize, Deserialize, Default, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub default_token: Option<MaskedString>,
    #[serde(deserialize_with = "server_services")]
    pub services: HashMap<String, ServerServiceConfig>,
    #[serde(default)]
    pub transport: TransportConfig,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub client: Option<ClientConfig>,
}

impl Config {
    fn from_str(s: &str) -> Result<Config> {
        let mut config: Config = toml::from_str(s).with_context(|| "Failed to parse the config")?;

        if let Some(server) = config.server.as_mut() {
            Config::validate_server_config(server)?;
        }

        if let Some(client) = config.client.as_mut() {
            Config::validate_client_config(client)?;
        }

        if config.server.is_none() && config.client.is_none() {
            Err(anyhow!("Neither of `[server]` or `[client]` is defined"))
        } else {
            Ok(config)
        }
    }

    fn validate_server_config(server: &mut ServerConfig) -> Result<()> {
        // Validate services
        for (name, s) in &mut server.services {
            s.name = name.clone();
            if s.token.is_none() {
                s.token = server.default_token.clone();
                if s.token.is_none() {
                    bail!("The token of service {} is not set", name);
                }
            }
        }

        let mut services: Vec<_> = server.services.values().collect();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        for (i, a) in services.iter().enumerate() {
            if let Some(b) = services[i + 1..].iter().find(|b| same_port(a, b)) {
                let protocol = match a.service_type {
                    ServiceType::Tcp => "tcp",
                    ServiceType::Udp => "udp",
                };
                bail!(
                    "Services {} and {} both bind {} {}",
                    a.name,
                    b.name,
                    protocol,
                    a.bind_addr
                );
            }
        }

        Config::validate_transport_config(&server.transport, true)?;

        Ok(())
    }

    fn validate_client_config(client: &mut ClientConfig) -> Result<()> {
        // Validate services
        for (name, s) in &mut client.services {
            s.name = name.clone();
            if s.token.is_none() {
                s.token = client.default_token.clone();
                if s.token.is_none() {
                    bail!("The token of service {} is not set", name);
                }
            }
            if s.retry_interval.is_none() {
                s.retry_interval = Some(client.retry_interval);
            }
        }

        Config::validate_transport_config(&client.transport, false)?;

        Ok(())
    }

    /// Only accept Noise patterns where the initiator authenticates the responder
    /// with a pre-known static key, so that a man-in-the-middle cannot be silently accepted
    fn validate_noise_config(config: &NoiseConfig, is_server: bool) -> Result<()> {
        let handshake = config
            .pattern
            .split('_')
            .nth(1)
            .ok_or_else(|| anyhow!("Invalid noise pattern: {}", config.pattern))?;
        // Strip PSK modifiers, e.g. `NKpsk0`
        let base: String = handshake.chars().take(2).collect();
        if !matches!(base.as_str(), "NK" | "KK" | "XK" | "IK") {
            bail!(
                "Unsupported noise pattern `{}`. The pattern must authenticate the server, use one of NK, KK, XK or IK",
                config.pattern
            );
        }

        if is_server {
            if config.local_private_key.is_none() {
                bail!("Missing `noise.local_private_key`. Generate one with `--genkey`");
            }
        } else if config.remote_public_key.is_none() {
            bail!(
                "Missing `noise.remote_public_key`, the client must know the server's public key"
            );
        }

        if base == "KK"
            && (config.local_private_key.is_none() || config.remote_public_key.is_none())
        {
            bail!("The KK pattern requires both `noise.local_private_key` and `noise.remote_public_key`");
        }
        Ok(())
    }

    fn validate_transport_config(config: &TransportConfig, is_server: bool) -> Result<()> {
        config
            .tcp
            .proxy
            .as_ref()
            .map_or(Ok(()), |u| match u.scheme() {
                "socks5" => Ok(()),
                "http" => Ok(()),
                _ => Err(anyhow!(format!("Unknown proxy scheme: {}", u.scheme()))),
            })?;
        match config.transport_type {
            TransportType::Tcp => Ok(()),
            TransportType::Tls => {
                let tls_config = config
                    .tls
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing TLS configuration"))?;
                if is_server {
                    tls_config
                        .pkcs12
                        .as_ref()
                        .and(tls_config.pkcs12_password.as_ref())
                        .ok_or_else(|| anyhow!("Missing `pkcs12` or `pkcs12_password`"))?;
                }
                Ok(())
            }
            TransportType::Noise => {
                let noise_config = config
                    .noise
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing noise configuration"))?;
                Config::validate_noise_config(noise_config, is_server)
            }
            TransportType::Websocket => Ok(()),
        }
    }

    pub async fn from_file(path: &Path) -> Result<Config> {
        let s: String = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read the config {:?}", path))?;
        Config::from_str(&s).with_context(|| {
            "Configuration is invalid. Please refer to the configuration specification."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    use anyhow::Result;

    fn list_config_files<T: AsRef<Path>>(root: T) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            } else if path.is_dir() {
                files.append(&mut list_config_files(path)?);
            }
        }
        Ok(files)
    }

    fn get_all_example_config() -> Result<Vec<PathBuf>> {
        Ok(list_config_files("./examples")?
            .into_iter()
            .filter(|x| x.ends_with(".toml"))
            .collect())
    }

    #[test]
    fn test_example_config() -> Result<()> {
        let paths = get_all_example_config()?;
        for p in paths {
            let s = fs::read_to_string(p)?;
            Config::from_str(&s)?;
        }
        Ok(())
    }

    #[test]
    fn test_valid_config() -> Result<()> {
        let paths = list_config_files("tests/config_test/valid_config")?;
        for p in paths {
            let s = fs::read_to_string(p)?;
            Config::from_str(&s)?;
        }
        Ok(())
    }

    #[test]
    fn test_invalid_config() -> Result<()> {
        let paths = list_config_files("tests/config_test/invalid_config")?;
        for p in paths {
            let s = fs::read_to_string(p)?;
            assert!(Config::from_str(&s).is_err());
        }
        Ok(())
    }

    #[test]
    fn test_validate_server_config() -> Result<()> {
        let mut cfg = ServerConfig::default();

        cfg.services.insert(
            "foo1".into(),
            ServerServiceConfig {
                service_type: ServiceType::Tcp,
                name: "foo1".into(),
                bind_addr: "127.0.0.1:80".into(),
                token: None,
                ..Default::default()
            },
        );

        // Missing the token
        assert!(Config::validate_server_config(&mut cfg).is_err());

        // Use the default token
        cfg.default_token = Some("123".into());
        assert!(Config::validate_server_config(&mut cfg).is_ok());
        assert_eq!(
            cfg.services
                .get("foo1")
                .as_ref()
                .unwrap()
                .token
                .as_ref()
                .unwrap()
                .0,
            "123"
        );

        // The default token won't override the service token
        cfg.services.get_mut("foo1").unwrap().token = Some("4".into());
        assert!(Config::validate_server_config(&mut cfg).is_ok());
        assert_eq!(
            cfg.services
                .get("foo1")
                .as_ref()
                .unwrap()
                .token
                .as_ref()
                .unwrap()
                .0,
            "4"
        );
        Ok(())
    }

    #[test]
    fn test_validate_client_config() -> Result<()> {
        let mut cfg = ClientConfig::default();

        cfg.services.insert(
            "foo1".into(),
            ClientServiceConfig {
                service_type: ServiceType::Tcp,
                name: "foo1".into(),
                local_addr: "127.0.0.1:80".into(),
                token: None,
                ..Default::default()
            },
        );

        // Missing the token
        assert!(Config::validate_client_config(&mut cfg).is_err());

        // Use the default token
        cfg.default_token = Some("123".into());
        assert!(Config::validate_client_config(&mut cfg).is_ok());
        assert_eq!(
            cfg.services
                .get("foo1")
                .as_ref()
                .unwrap()
                .token
                .as_ref()
                .unwrap()
                .0,
            "123"
        );

        // The default token won't override the service token
        cfg.services.get_mut("foo1").unwrap().token = Some("4".into());
        assert!(Config::validate_client_config(&mut cfg).is_ok());
        assert_eq!(
            cfg.services
                .get("foo1")
                .as_ref()
                .unwrap()
                .token
                .as_ref()
                .unwrap()
                .0,
            "4"
        );
        Ok(())
    }

    #[test]
    fn test_port_ranges() -> Result<()> {
        let cfg = Config::from_str(
            r#"
            [server]
            bind_addr = "0.0.0.0:2333"
            default_token = "t"
            [server.services.game]
            type = "tcp+udp"
            bind_addr = "0.0.0.0:27015-27016"
            [server.services.voice]
            type = "udp"
            bind_addr = "[::]:9987"
            [server.services.web]
            bind_addr = "127.0.0.1:8080"

            [client]
            remote_addr = "example.com:2333"
            default_token = "t"
            [client.services.game]
            type = "tcp+udp"
            local_addr = "192.168.1.10:37015-37016"
            nodelay = false
            "#,
        )?;

        let server = cfg.server.unwrap();
        let mut names: Vec<_> = server.services.keys().cloned().collect();
        names.sort();
        assert_eq!(
            names,
            [
                "game#0/tcp",
                "game#0/udp",
                "game#1/tcp",
                "game#1/udp",
                "voice",
                "web"
            ]
        );
        let s = &server.services["game#1/udp"];
        assert_eq!(s.service_type, ServiceType::Udp);
        assert_eq!(s.bind_addr, "0.0.0.0:27016");
        assert_eq!(s.name, "game#1/udp");
        assert_eq!(server.services["voice"].bind_addr, "[::]:9987");

        // The client names the parts the same way and keeps the other options
        let client = cfg.client.unwrap();
        let c = &client.services["game#1/tcp"];
        assert_eq!(c.service_type, ServiceType::Tcp);
        assert_eq!(c.local_addr, "192.168.1.10:37016");
        assert_eq!(c.nodelay, Some(false));
        assert_eq!(client.services.len(), 4);
        Ok(())
    }

    #[test]
    fn test_invalid_port_ranges() {
        let server = |services: &str| {
            Config::from_str(&format!(
                "[server]\nbind_addr = \"0.0.0.0:2333\"\ndefault_token = \"t\"\n{}",
                services
            ))
        };
        let service = |name: &str, ty: &str, addr: &str| {
            format!(
                "[server.services.{}]\ntype = \"{}\"\nbind_addr = \"{}\"\n",
                name, ty, addr
            )
        };

        assert!(server(&service("a", "tcp", "0.0.0.0:1-100")).is_ok());
        for addr in [
            "0.0.0.0:1-101",
            "0.0.0.0:20-10",
            "0.0.0.0:1-x",
            "0.0.0.0:1-70000",
        ] {
            assert!(server(&service("a", "tcp", addr)).is_err(), "{}", addr);
        }
        assert!(server(&service("a", "tcp+tcp", "0.0.0.0:1")).is_err());
        assert!(server(&format!(
            "{}{}",
            service("a", "tcp", "0.0.0.0:1"),
            "unknown = 1\n"
        ))
        .is_err());

        // The same port twice for one protocol
        let overlap = |b: &str| {
            server(&format!(
                "{}{}",
                service("a", "tcp", "0.0.0.0:7000-7004"),
                b
            ))
        };
        assert!(overlap(&service("b", "udp", "0.0.0.0:7001")).is_ok());
        assert!(overlap(&service("b", "tcp+udp", "0.0.0.0:7001")).is_err());
        assert!(overlap(&service("b", "tcp", "127.0.0.1:7004")).is_err());
        assert!(overlap(&service("b", "tcp", "127.0.0.1:7005")).is_ok());

        // A name that clashes with a part of a range
        assert!(server(&format!(
            "{}{}",
            service("a", "tcp", "0.0.0.0:1-2"),
            service("\"a#0\"", "tcp", "0.0.0.0:3")
        ))
        .is_err());
    }

    #[test]
    fn test_noise_pattern_validation() {
        let key = Some("a2V5".to_string());
        let mk = |pattern: &str, local: bool, remote: bool| NoiseConfig {
            pattern: pattern.to_string(),
            local_private_key: local.then(|| MaskedString("a2V5".to_string())),
            remote_public_key: remote.then(|| key.clone().unwrap()),
        };
        let nk = "Noise_NK_25519_ChaChaPoly_BLAKE2s";

        // Server needs its own key, client needs the server's public key
        assert!(Config::validate_noise_config(&mk(nk, true, false), true).is_ok());
        assert!(Config::validate_noise_config(&mk(nk, false, false), true).is_err());
        assert!(Config::validate_noise_config(&mk(nk, false, true), false).is_ok());
        assert!(Config::validate_noise_config(&mk(nk, false, false), false).is_err());

        // Patterns without server authentication are rejected
        for p in [
            "Noise_NN_25519_ChaChaPoly_BLAKE2s",
            "Noise_NX_25519_ChaChaPoly_BLAKE2s",
            "Noise_N_25519_ChaChaPoly_BLAKE2s",
            "garbage",
        ] {
            assert!(Config::validate_noise_config(&mk(p, true, true), true).is_err());
            assert!(Config::validate_noise_config(&mk(p, true, true), false).is_err());
        }

        // PSK modifiers keep the base pattern, KK needs both keys
        assert!(Config::validate_noise_config(
            &mk("Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s", true, false),
            true
        )
        .is_ok());
        assert!(Config::validate_noise_config(
            &mk("Noise_KK_25519_ChaChaPoly_BLAKE2s", true, false),
            true
        )
        .is_err());
        assert!(Config::validate_noise_config(
            &mk("Noise_KK_25519_ChaChaPoly_BLAKE2s", true, true),
            true
        )
        .is_ok());
    }
}
