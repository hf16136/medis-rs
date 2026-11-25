//! Connection configuration (a favorite / the quick-connect form) and `connection_key()`.
//!
//! The JSON field names match the original Medis localStorage objects (`sshHost`, `tlskeyFile`,
//! ...) so that favorites stay recognisable. Extra fields added by the port (`username`, `tls`,
//! `cluster`) default when absent.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// TLS options added by the port.
///
/// TLS is considered enabled when either [`ConnectionConfig::ssl`] (the original checkbox) or
/// [`TlsConfig::enabled`] is set. PEM material is taken from the in-line contents
/// (`tlsca`/`tlscert`/`tlskey`) when non-empty, otherwise read from the path
/// (`tlscaFile`/... or the `*_path` fields here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// Enable TLS (OR-ed with `ConnectionConfig::ssl`).
    pub enabled: bool,
    /// Path to a CA PEM file (optional).
    pub ca_path: String,
    /// Path to a client certificate PEM file (optional, needs `key_path`).
    pub cert_path: String,
    /// Path to a client private key PEM file (optional, needs `cert_path`).
    pub key_path: String,
    /// Skip certificate verification entirely. The original Medis never verified
    /// (`rejectUnauthorized: false`), so this defaults to `true` for parity.
    pub insecure: bool,
    /// Verify the certificate chain but ignore the hostname. Forced on when tunnelling over SSH.
    pub skip_hostname: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ca_path: String::new(),
            cert_path: String::new(),
            key_path: String::new(),
            insecure: true,
            skip_hostname: false,
        }
    }
}

/// Redis Cluster options added by the port.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterConfig {
    /// Connect in cluster mode (db 0 only; SCAN iterates all primaries).
    pub enabled: bool,
    /// Seed nodes as `host:port` (a `redis://` prefix is tolerated). When empty, `host:port` of
    /// the connection config is used as the single seed.
    pub nodes: Vec<String>,
}

/// One connection definition: a favorite or the quick-connect form.
///
/// `port` and `ssh_port` are strings, as in the original, so that an empty form field means
/// "use the default". Call [`ConnectionConfig::with_defaults`] before connecting (the connect
/// function does it for you).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    /// `favorite-<uuid>`; empty for quick connect.
    pub key: String,
    /// Display name (default `New Favorite`).
    pub name: String,
    /// Redis host (effective default `localhost`).
    pub host: String,
    /// Redis port as text (effective default `6379`).
    #[serde(deserialize_with = "de_string_or_number")]
    pub port: String,
    /// Redis password (AUTH).
    pub password: String,
    /// ACL username (port addition; empty = none).
    pub username: String,
    /// TLS checkbox of the original form.
    pub ssl: bool,
    /// Client private key PEM contents.
    #[serde(rename = "tlskey")]
    pub tls_key: String,
    /// Client certificate PEM contents.
    #[serde(rename = "tlscert")]
    pub tls_cert: String,
    /// CA PEM contents.
    #[serde(rename = "tlsca")]
    pub tls_ca: String,
    /// Path the private key was loaded from.
    #[serde(rename = "tlskeyFile")]
    pub tls_key_file: String,
    /// Path the certificate was loaded from.
    #[serde(rename = "tlscertFile")]
    pub tls_cert_file: String,
    /// Path the CA was loaded from.
    #[serde(rename = "tlscaFile")]
    pub tls_ca_file: String,
    /// Use an SSH tunnel.
    pub ssh: bool,
    /// SSH server host.
    #[serde(rename = "sshHost")]
    pub ssh_host: String,
    /// SSH user.
    #[serde(rename = "sshUser")]
    pub ssh_user: String,
    /// SSH password (used when no key is set).
    #[serde(rename = "sshPassword")]
    pub ssh_password: String,
    /// SSH private key contents (serialised as `false` when absent, like the original).
    #[serde(
        rename = "sshKey",
        deserialize_with = "de_opt_string_or_false",
        serialize_with = "ser_opt_string_or_false"
    )]
    pub ssh_key: Option<String>,
    /// SSH private key path (used when `ssh_key` is absent/empty).
    #[serde(
        rename = "sshKeyFile",
        deserialize_with = "de_opt_string_or_false",
        serialize_with = "ser_opt_string_or_false"
    )]
    pub ssh_key_file: Option<String>,
    /// Passphrase of the SSH key.
    #[serde(rename = "sshKeyPassphrase")]
    pub ssh_key_passphrase: String,
    /// SSH port as text (effective default `22`).
    #[serde(rename = "sshPort", deserialize_with = "de_string_or_number")]
    pub ssh_port: String,
    /// Extra TLS options (port addition).
    pub tls: TlsConfig,
    /// Cluster options (port addition).
    pub cluster: ClusterConfig,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            name: "New Favorite".to_string(),
            host: String::new(),
            port: String::new(),
            password: String::new(),
            username: String::new(),
            ssl: false,
            tls_key: String::new(),
            tls_cert: String::new(),
            tls_ca: String::new(),
            tls_key_file: String::new(),
            tls_cert_file: String::new(),
            tls_ca_file: String::new(),
            ssh: false,
            ssh_host: String::new(),
            ssh_user: String::new(),
            ssh_password: String::new(),
            ssh_key: None,
            ssh_key_file: None,
            ssh_key_passphrase: String::new(),
            ssh_port: String::new(),
            tls: TlsConfig::default(),
            cluster: ClusterConfig::default(),
        }
    }
}

fn de_string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        S(String),
        N(i64),
        Null(()),
    }
    Ok(match V::deserialize(d)? {
        V::S(s) => s,
        V::N(n) => n.to_string(),
        V::Null(()) => String::new(),
    })
}

fn de_opt_string_or_false<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        S(String),
        B(#[allow(dead_code)] bool),
        Null(()),
    }
    Ok(match V::deserialize(d)? {
        V::S(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

fn ser_opt_string_or_false<S: Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => s.serialize_str(v),
        None => s.serialize_bool(false),
    }
}

/// PEM material resolved from a [`ConnectionConfig`] (contents, or files read from disk).
#[derive(Debug, Clone, Default)]
pub struct TlsMaterial {
    /// CA certificates (PEM).
    pub ca: Option<Vec<u8>>,
    /// Client certificate (PEM).
    pub cert: Option<Vec<u8>>,
    /// Client private key (PEM).
    pub key: Option<Vec<u8>>,
    /// Skip certificate verification altogether.
    pub insecure: bool,
    /// Ignore the hostname in the certificate only.
    pub skip_hostname: bool,
}

impl ConnectionConfig {
    /// Creates a new favorite with a fresh `favorite-<uuid>` key and the default name.
    pub fn new_favorite() -> Self {
        Self {
            key: format!("favorite-{}", uuid::Uuid::new_v4().simple()),
            ..Self::default()
        }
    }

    /// Creates the config of a quick connection (empty key, name `Quick Connect`).
    pub fn quick() -> Self {
        Self {
            name: "Quick Connect".to_string(),
            ..Self::default()
        }
    }

    /// Returns a copy with the effective defaults applied: host `localhost`, port `6379`,
    /// sshPort `22`.
    pub fn with_defaults(&self) -> Self {
        let mut c = self.clone();
        if c.host.trim().is_empty() {
            c.host = "localhost".into();
        }
        if c.port.trim().is_empty() {
            c.port = "6379".into();
        }
        if c.ssh_port.trim().is_empty() {
            c.ssh_port = "22".into();
        }
        c
    }

    /// Effective Redis port number (default 6379).
    pub fn port_num(&self) -> Result<u16, String> {
        parse_port(&self.port, 6379, "Redis")
    }

    /// Effective SSH port number (default 22).
    pub fn ssh_port_num(&self) -> Result<u16, String> {
        parse_port(&self.ssh_port, 22, "SSH")
    }

    /// The per-instance key used to index patterns: `"<sshHost||''>|<host>|<port>"`.
    ///
    /// Matches the original, which uses `sshHost` regardless of whether the SSH checkbox is on.
    /// Host and port are used as given (apply [`with_defaults`](Self::with_defaults) first to
    /// get the same key the connected instance has).
    pub fn connection_key(&self) -> String {
        format!("{}|{}|{}", self.ssh_host, self.host, self.port)
    }

    /// The key of the pattern list for a database: `"<connectionKey>|<db>"`.
    pub fn pattern_key(&self, db: u32) -> String {
        format!("{}|{}", self.connection_key(), db)
    }

    /// Instance title after connecting: `<name>/` (or `<sshHost>/`, or nothing) followed by
    /// `host:port`.
    pub fn title(&self) -> String {
        let remote = if !self.name.is_empty() {
            format!("{}/", self.name)
        } else if !self.ssh_host.is_empty() {
            format!("{}/", self.ssh_host)
        } else {
            String::new()
        };
        format!("{}{}:{}", remote, self.host, self.port)
    }

    /// `true` when TLS is requested (legacy checkbox or the new flag).
    pub fn tls_enabled(&self) -> bool {
        self.ssl || self.tls.enabled
    }

    /// `true` when the SSH key needs a passphrase (the passphrase field is then shown): the
    /// contents contain `ENCRYPTED` (PEM, as in the original) or are an encrypted OpenSSH-format
    /// key (whose cipher name is only visible after decoding).
    pub fn ssh_key_is_encrypted(&self) -> bool {
        let Some(k) = self.ssh_key.as_deref() else {
            return false;
        };
        k.contains("ENCRYPTED")
            || russh::keys::PrivateKey::from_openssh(k).is_ok_and(|key| key.is_encrypted())
    }

    /// Seed nodes for cluster mode as `(host, port)`; falls back to `host:port`.
    pub fn cluster_seeds(&self) -> Result<Vec<(String, u16)>, String> {
        let mut out = Vec::new();
        for n in &self.cluster.nodes {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            out.push(parse_node(n, self.port_num().unwrap_or(6379))?);
        }
        if out.is_empty() {
            let c = self.with_defaults();
            out.push((c.host.clone(), c.port_num()?));
        }
        Ok(out)
    }

    /// Resolves the TLS PEM material. Contents win over file paths; unreadable files are errors.
    pub fn tls_material(&self) -> Result<TlsMaterial, String> {
        fn pick(content: &str, paths: [&str; 2], what: &str) -> Result<Option<Vec<u8>>, String> {
            if !content.trim().is_empty() {
                return Ok(Some(content.as_bytes().to_vec()));
            }
            for p in paths {
                if !p.trim().is_empty() {
                    return std::fs::read(p)
                        .map(Some)
                        .map_err(|e| format!("cannot read TLS {what} file {p}: {e}"));
                }
            }
            Ok(None)
        }
        Ok(TlsMaterial {
            ca: pick(&self.tls_ca, [&self.tls_ca_file, &self.tls.ca_path], "CA")?,
            cert: pick(&self.tls_cert, [&self.tls_cert_file, &self.tls.cert_path], "certificate")?,
            key: pick(&self.tls_key, [&self.tls_key_file, &self.tls.key_path], "key")?,
            insecure: self.tls.insecure,
            skip_hostname: self.tls.skip_hostname,
        })
    }
}

fn parse_port(s: &str, default: u16, what: &str) -> Result<u16, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(default);
    }
    s.parse::<u16>().map_err(|_| format!("Invalid {what} port: {s}"))
}

/// Parses `host:port`, `redis://host:port[/]`, `[::1]:6379` (port defaults to `default_port`).
pub fn parse_node(s: &str, default_port: u16) -> Result<(String, u16), String> {
    let s = s
        .trim()
        .trim_start_matches("rediss://")
        .trim_start_matches("redis://")
        .trim_end_matches('/');
    let (host, port) = match s.rfind(':') {
        Some(i) if !s[i + 1..].contains(']') => {
            let port = s[i + 1..]
                .parse::<u16>()
                .map_err(|_| format!("Invalid cluster node: {s}"))?;
            (&s[..i], port)
        }
        _ => (s, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(format!("Invalid cluster node: {s}"));
    }
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_key_matches_original() {
        let mut c = ConnectionConfig {
            host: "h".into(),
            port: "6380".into(),
            ..Default::default()
        };
        assert_eq!(c.connection_key(), "|h|6380");
        c.ssh_host = "bastion".into();
        assert_eq!(c.connection_key(), "bastion|h|6380");
        // sshHost is used even when the ssh flag is off (original behaviour)
        assert!(!c.ssh);
        assert_eq!(c.pattern_key(3), "bastion|h|6380|3");
    }

    #[test]
    fn defaults_and_title() {
        let c = ConnectionConfig::default().with_defaults();
        assert_eq!(c.host, "localhost");
        assert_eq!(c.port, "6379");
        assert_eq!(c.ssh_port, "22");
        assert_eq!(c.connection_key(), "|localhost|6379");
        assert_eq!(c.title(), "New Favorite/localhost:6379");
        let mut c2 = c.clone();
        c2.name.clear();
        assert_eq!(c2.title(), "localhost:6379");
        c2.ssh_host = "jump".into();
        assert_eq!(c2.title(), "jump/localhost:6379");
    }

    #[test]
    fn json_compat_with_original() {
        let j = r#"{"key":"favorite-1","name":"x","host":"a","port":6380,"ssh":true,
            "sshHost":"s","sshKey":false,"sshKeyFile":"/k","sshPort":"22","tlskeyFile":"/f"}"#;
        let c: ConnectionConfig = serde_json::from_str(j).unwrap();
        assert_eq!(c.port, "6380");
        assert_eq!(c.ssh_key, None);
        assert_eq!(c.ssh_key_file.as_deref(), Some("/k"));
        assert_eq!(c.tls_key_file, "/f");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["sshKey"], serde_json::Value::Bool(false));
        assert_eq!(v["sshHost"], "s");
    }

    #[test]
    fn nodes() {
        assert_eq!(parse_node("redis://a:7000/", 1).unwrap(), ("a".into(), 7000));
        assert_eq!(parse_node("a", 6379).unwrap(), ("a".into(), 6379));
        assert_eq!(parse_node("[::1]:7000", 1).unwrap(), ("::1".into(), 7000));
        assert!(parse_node("a:x", 1).is_err());
    }
}
