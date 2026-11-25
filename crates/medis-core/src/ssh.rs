//! SSH local-forward tunnel built on russh.
//!
//! [`open`] connects to the SSH server, authenticates (private key – inline contents or file
//! path, with optional passphrase – otherwise password), binds `127.0.0.1:0` and forwards every
//! accepted local connection to `remote_host:remote_port` through the SSH server
//! (`direct-tcpip`). The returned [`Tunnel`] owns the SSH session and the accept loop; dropping
//! it stops the forward and closes the session (and the in-flight forwarded connections).
//!
//! Host keys are not verified (the original uses ssh2 without a `hostVerifier`).
//! Error strings returned here are the bare messages; [`crate::conn`] prefixes `SSH Error: `.

use std::sync::Arc;
use std::time::Duration;

use russh::client;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use tokio::net::TcpListener;
use tokio::task::{AbortHandle, JoinSet};

/// SSH connection parameters (taken from [`crate::ConnectionConfig`]).
#[derive(Debug, Clone, Default)]
pub struct SshParams {
    /// SSH server host.
    pub host: String,
    /// SSH server port.
    pub port: u16,
    /// Login user.
    pub user: String,
    /// Password (used when no key is given).
    pub password: String,
    /// Private key contents (PEM/OpenSSH). Wins over `key_file`.
    pub key: Option<String>,
    /// Private key file path.
    pub key_file: Option<String>,
    /// Key passphrase (empty = none).
    pub passphrase: String,
}

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// A running local-forward tunnel. Dropping it tears everything down.
#[derive(Debug)]
pub struct Tunnel {
    local_port: u16,
    accept_task: AbortHandle,
}

impl Tunnel {
    /// The local port on `127.0.0.1` that forwards to the remote target.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // The accept task owns the SSH session handle and the JoinSet of forwarding tasks;
        // aborting it drops all of them.
        self.accept_task.abort();
    }
}

fn load_key(p: &SshParams) -> Result<Option<PrivateKey>, String> {
    let pass = (!p.passphrase.is_empty()).then_some(p.passphrase.as_str());
    if let Some(k) = p.key.as_deref().filter(|k| !k.trim().is_empty()) {
        return russh::keys::decode_secret_key(k, pass)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    if let Some(f) = p.key_file.as_deref().filter(|f| !f.trim().is_empty()) {
        return russh::keys::load_secret_key(f, pass)
            .map(Some)
            .map_err(|e| format!("{f}: {e}"));
    }
    Ok(None)
}

/// Opens the tunnel to `remote_host:remote_port`. Errors are plain messages.
pub async fn open(p: &SshParams, remote_host: &str, remote_port: u16) -> Result<Tunnel, String> {
    let key = load_key(p)?;

    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        nodelay: true,
        ..Default::default()
    });
    let connect = client::connect(config, (p.host.as_str(), p.port), Client);
    let mut session = tokio::time::timeout(Duration::from_secs(10), connect)
        .await
        .map_err(|_| "Timed out while waiting for handshake".to_string())?
        .map_err(|e| e.to_string())?;

    let ok = match key {
        Some(key) => {
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|e| e.to_string())?
                .flatten();
            session
                .authenticate_publickey(&p.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| e.to_string())?
                .success()
        }
        None => session
            .authenticate_password(&p.user, &p.password)
            .await
            .map_err(|e| e.to_string())?
            .success(),
    };
    if !ok {
        return Err("All configured authentication methods failed".to_string());
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let session = Arc::new(session);
    let rhost = remote_host.to_string();

    let task = tokio::spawn(async move {
        let mut forwards = JoinSet::new();
        loop {
            let (mut sock, peer) = tokio::select! {
                r = listener.accept() => match r {
                    Ok(v) => v,
                    Err(_) => break,
                },
                // reap finished forwards
                Some(_) = forwards.join_next(), if !forwards.is_empty() => continue,
            };
            let session = session.clone();
            let rhost = rhost.clone();
            forwards.spawn(async move {
                let ch = session
                    .channel_open_direct_tcpip(
                        rhost,
                        u32::from(remote_port),
                        peer.ip().to_string(),
                        u32::from(peer.port()),
                    )
                    .await;
                if let Ok(ch) = ch {
                    let mut stream = ch.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
                }
            });
        }
    });

    Ok(Tunnel {
        local_port,
        accept_task: task.abort_handle(),
    })
}
