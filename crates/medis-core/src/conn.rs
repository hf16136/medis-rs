//! Redis connection setup: [`RedisConn`] (single multiplexed connection or cluster),
//! [`connect`] / [`connect_with`] and the verbatim SPEC-1 error strings.
//!
//! * RESP2 is used (parity with ioredis).
//! * Connect timeout 10 s (the original), response timeout 30 s (redis-rs defaults to 500 ms,
//!   which is far too low for SCAN/DUMP on big datasets).
//! * TLS through rustls (`aws-lc-rs`): custom CA, mTLS, "insecure" (no verification, the
//!   original's behaviour) and "skip hostname" (forced on when tunnelling over SSH).
//! * Cluster mode connects through the seed nodes; SSH + cluster is rejected with an error.

use std::time::Duration;

use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::cluster::ClusterClientBuilder;
use redis::cluster_async::ClusterConnection;
use redis::{
    AsyncConnectionConfig, Client, ClientTlsConfig, Cmd, ConnectionAddr, ConnectionInfo,
    IntoConnectionInfo, ProtocolVersion, RedisConnectionInfo, RedisError, RedisFuture, TlsCertificates,
    TlsMode, Value,
};

use crate::config::{ConnectionConfig, TlsMaterial};
use crate::ssh::{self, SshParams, Tunnel};

/// Connect timeout (original: ioredis default 10 s).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-command response timeout.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Alert shown for NOAUTH (SPEC-1).
pub const ERR_ACCESS_DENIED: &str = "Redis Error: Access denied. Please double-check your password.";
/// Alert shown when the server is older than 2.8 (SPEC-1).
pub const ERR_OLD_VERSION: &str = "Medis only supports Redis >= 2.8 because servers older than 2.8 don't support SCAN command, which means it not possible to access keys without blocking Redis.";
/// Status text while the tunnel is being established.
pub const STATUS_SSH: &str = "SSH connecting...";
/// Status text while Redis is being connected.
pub const STATUS_REDIS: &str = "Redis connecting...";

/// A Redis connection: one multiplexed connection, or a cluster connection.
///
/// Cheap to clone (shares the underlying connection). Implements [`redis::aio::ConnectionLike`],
/// so `redis::cmd("GET").arg("k").query_async(&mut conn)` works for both variants. Note that on
/// the cluster variant commands are routed by key, and pipelines must not span slots.
#[derive(Clone)]
pub enum RedisConn {
    /// Standalone server (supports `SELECT`).
    Single(MultiplexedConnection),
    /// Redis Cluster (db 0 only).
    Cluster(ClusterConnection),
}

impl std::fmt::Debug for RedisConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(_) => f.write_str("RedisConn::Single"),
            Self::Cluster(_) => f.write_str("RedisConn::Cluster"),
        }
    }
}

impl RedisConn {
    /// `true` for the cluster variant.
    pub fn is_cluster(&self) -> bool {
        matches!(self, Self::Cluster(_))
    }
}

impl ConnectionLike for RedisConn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Single(c) => c.req_packed_command(cmd),
            Self::Cluster(c) => c.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a redis::Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Single(c) => c.req_packed_commands(pipeline, offset, count),
            Self::Cluster(c) => c.req_packed_commands(pipeline, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Single(c) => c.get_db(),
            Self::Cluster(_) => 0,
        }
    }
}

/// Everything a successful connect yields.
pub struct Connected {
    /// The shared command connection.
    pub conn: RedisConn,
    /// The config used (defaults applied, e.g. port `6379`).
    pub config: ConnectionConfig,
    /// Instance title (`<name>/host:port`).
    pub title: String,
    /// `"<sshHost>|<host>|<port>"`.
    pub connection_key: String,
    /// `redis_version` from INFO (e.g. `7.2.4`), empty if unknown.
    pub version: String,
    /// A client for dedicated side connections (pub/sub, MONITOR, db-count probe). In cluster
    /// mode it points at the first seed node. Points at the tunnel's local port when tunnelling.
    pub client: Client,
    /// The SSH tunnel; keep it alive as long as the connection is used (dropping stops it).
    pub tunnel: Option<Tunnel>,
}

impl std::fmt::Debug for Connected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connected")
            .field("title", &self.title)
            .field("version", &self.version)
            .field("conn", &self.conn)
            .field("tunnel", &self.tunnel)
            .finish()
    }
}

/// Connects according to `cfg`; see [`connect_with`].
pub async fn connect(cfg: &ConnectionConfig) -> Result<Connected, String> {
    connect_with(cfg, |_| {}).await
}

/// Connects according to `cfg`, reporting progress through `status`
/// ([`STATUS_SSH`], [`STATUS_REDIS`]).
///
/// Flow (SPEC-1 §3): optional SSH tunnel, then Redis (AUTH is part of the handshake), `INFO`,
/// version check, `PING`. The error strings are the user-facing alert texts:
///
/// * `SSH Error: <msg>`
/// * `Redis Error: Access denied. Please double-check your password.`
/// * `Redis Error: Connection failed. (<error>)`
/// * the "Medis only supports Redis >= 2.8 ..." message.
pub async fn connect_with(
    cfg: &ConnectionConfig,
    status: impl Fn(&str),
) -> Result<Connected, String> {
    crate::install_crypto_provider();
    let cfg = cfg.with_defaults();
    let port = cfg.port_num().map_err(|e| format!("Redis Error: {e}"))?;
    let cluster = cfg.cluster.enabled;

    if cfg.ssh && cluster {
        return Err("Redis Error: SSH tunnel is not supported in cluster mode.".to_string());
    }

    // 1. SSH tunnel
    let mut tunnel = None;
    let (host, port) = if cfg.ssh {
        status(STATUS_SSH);
        let params = SshParams {
            host: cfg.ssh_host.clone(),
            port: cfg.ssh_port_num().map_err(|e| format!("SSH Error: {e}"))?,
            user: cfg.ssh_user.clone(),
            password: cfg.ssh_password.clone(),
            key: cfg.ssh_key.clone(),
            key_file: cfg.ssh_key_file.clone(),
            passphrase: cfg.ssh_key_passphrase.clone(),
        };
        let t = ssh::open(&params, &cfg.host, port)
            .await
            .map_err(|e| format!("SSH Error: {e}"))?;
        let p = t.local_port();
        tunnel = Some(t);
        ("127.0.0.1".to_string(), p)
    } else {
        (cfg.host.clone(), port)
    };

    // 2. Redis
    status(STATUS_REDIS);
    let tls = if cfg.tls_enabled() {
        Some(cfg.tls_material().map_err(|e| format!("Redis Error: Connection failed. ({e})"))?)
    } else {
        None
    };
    let tunneled = tunnel.is_some();

    let (conn, client) = if cluster {
        let seeds = cfg.cluster_seeds().map_err(|e| format!("Redis Error: {e}"))?;
        let conn = connect_cluster(&cfg, &seeds, tls.as_ref())
            .await
            .map_err(|e| connect_error_message(&e))?;
        let client = build_client(&cfg, &seeds[0].0, seeds[0].1, tls.as_ref(), false)
            .map_err(|e| connect_error_message(&e))?;
        (conn, client)
    } else {
        let client = build_client(&cfg, &host, port, tls.as_ref(), tunneled)
            .map_err(|e| connect_error_message(&e))?;
        let acfg = async_config();
        let c = client
            .get_multiplexed_async_connection_with_config(&acfg)
            .await
            .map_err(|e| connect_error_message(&e))?;
        (RedisConn::Single(c), client)
    };

    let mut conn = conn;
    let version = handshake(&mut conn).await?;

    Ok(Connected {
        title: cfg.title(),
        connection_key: cfg.connection_key(),
        config: cfg,
        conn,
        version,
        client,
        tunnel,
    })
}

/// INFO + version check + PING. Returns `redis_version`.
async fn handshake(conn: &mut RedisConn) -> Result<String, String> {
    // In cluster mode a plain INFO fans out to every node; ask a single one.
    let info: String = crate::ops::query_any(conn, redis::cmd("INFO").arg("server").clone())
        .await
        .map_err(|e| core_error_message(&e.message))?;
    let version = crate::ops::parse_info(&info)
        .get("redis_version")
        .cloned()
        .unwrap_or_default();
    if let Some(v) = parse_version(&version)
        && v < (2, 8)
    {
        return Err(ERR_OLD_VERSION.to_string());
    }
    let _: () = redis::cmd("PING")
        .query_async::<redis::Value>(conn)
        .await
        .map(|_| ())
        .map_err(|e| connect_error_message(&e))?;
    Ok(version)
}

/// Parses `major.minor[.patch...]` into `(major, minor)`.
pub fn parse_version(v: &str) -> Option<(u32, u32)> {
    let mut it = v.split(|c: char| !c.is_ascii_digit());
    let major = it.next()?.parse().ok()?;
    let minor = it.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

fn core_error_message(msg: &str) -> String {
    if msg.starts_with("NOAUTH") || msg.starts_with("WRONGPASS") {
        ERR_ACCESS_DENIED.to_string()
    } else {
        format!("Redis Error: Connection failed. ({msg})")
    }
}

/// Maps a [`RedisError`] raised during connect to the alert text of SPEC-1.
pub fn connect_error_message(e: &RedisError) -> String {
    let code = e.code().unwrap_or("");
    if code == "NOAUTH" || code == "WRONGPASS" || e.kind() == redis::ErrorKind::AuthenticationFailed {
        ERR_ACCESS_DENIED.to_string()
    } else {
        format!("Redis Error: Connection failed. ({})", crate::error::redis_error_message(e))
    }
}

fn async_config() -> AsyncConnectionConfig {
    AsyncConnectionConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(RESPONSE_TIMEOUT))
}

fn redis_settings(cfg: &ConnectionConfig) -> RedisConnectionInfo {
    let mut r = RedisConnectionInfo::default()
        .set_db(0)
        .set_protocol(ProtocolVersion::RESP2);
    if !cfg.username.is_empty() {
        r = r.set_username(&cfg.username);
    }
    if !cfg.password.is_empty() {
        r = r.set_password(&cfg.password);
    }
    r
}

fn node_addr(host: &str, port: u16, tls: Option<&TlsMaterial>) -> ConnectionAddr {
    match tls {
        None => ConnectionAddr::Tcp(host.to_string(), port),
        Some(t) => ConnectionAddr::TcpTls {
            host: host.to_string(),
            port,
            insecure: t.insecure,
            tls_params: None,
        },
    }
}

fn certificates(t: &TlsMaterial) -> TlsCertificates {
    let client_tls = match (&t.cert, &t.key) {
        (Some(c), Some(k)) => Some(ClientTlsConfig {
            client_cert: c.clone(),
            client_key: k.clone(),
        }),
        _ => None,
    };
    TlsCertificates {
        client_tls,
        root_cert: t.ca.clone(),
    }
}

/// Builds a standalone [`Client`] for `host:port`.
pub fn build_client(
    cfg: &ConnectionConfig,
    host: &str,
    port: u16,
    tls: Option<&TlsMaterial>,
    tunneled: bool,
) -> Result<Client, RedisError> {
    let info = node_addr(host, port, tls)
        .into_connection_info()?
        .set_redis_settings(redis_settings(cfg));
    let Some(t) = tls else {
        return Client::open(info);
    };
    let client = Client::build_with_tls(info, certificates(t))?;
    if t.skip_hostname || tunneled {
        let info = client.get_connection_info().clone();
        let mut addr = info.addr().clone();
        addr.set_danger_accept_invalid_hostnames(true);
        return Client::open(info.set_addr(addr));
    }
    Ok(client)
}

async fn connect_cluster(
    cfg: &ConnectionConfig,
    seeds: &[(String, u16)],
    tls: Option<&TlsMaterial>,
) -> Result<RedisConn, RedisError> {
    let nodes: Vec<ConnectionInfo> = seeds
        .iter()
        .map(|(h, p)| node_addr(h, *p, tls).into_connection_info())
        .collect::<Result<_, _>>()?;
    let mut b = ClusterClientBuilder::new(nodes)
        .connection_timeout(CONNECT_TIMEOUT)
        .response_timeout(RESPONSE_TIMEOUT)
        .use_protocol(ProtocolVersion::RESP2);
    if !cfg.username.is_empty() {
        b = b.username(&cfg.username);
    }
    if !cfg.password.is_empty() {
        b = b.password(&cfg.password);
    }
    if let Some(t) = tls {
        b = b
            .tls(if t.insecure { TlsMode::Insecure } else { TlsMode::Secure })
            .certs(certificates(t));
        if t.skip_hostname {
            b = b.danger_accept_invalid_hostnames(true);
        }
    }
    let c = b.build()?.get_async_connection().await?;
    Ok(RedisConn::Cluster(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("7.2.4"), Some((7, 2)));
        assert_eq!(parse_version("2.6.17"), Some((2, 6)));
        assert_eq!(parse_version("8.0-rc1"), Some((8, 0)));
        assert_eq!(parse_version(""), None);
        assert!(parse_version("2.6.0").unwrap() < (2, 8));
        assert!(parse_version("2.8.0").unwrap() >= (2, 8));
    }

    #[test]
    fn error_strings() {
        let e = RedisError::from((redis::ErrorKind::AuthenticationFailed, "bad"));
        assert_eq!(connect_error_message(&e), ERR_ACCESS_DENIED);
        let e = RedisError::from(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"));
        let m = connect_error_message(&e);
        assert!(m.starts_with("Redis Error: Connection failed. ("), "{m}");
    }
}
