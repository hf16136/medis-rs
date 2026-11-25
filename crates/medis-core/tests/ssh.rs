//! SSH tunnel test. Needs `MEDIS_TEST_REDIS` (target, as seen from the sshd host),
//! `MEDIS_TEST_SSH=host:port`, `MEDIS_TEST_SSH_USER`, `MEDIS_TEST_SSH_KEY` (unencrypted key
//! path) and optionally `MEDIS_TEST_SSH_KEY_ENC` (key path encrypted with passphrase `pp`).
//! Skipped otherwise.

use medis_core::{ConnectionConfig, connect, ops};

#[tokio::test]
async fn tunnel_with_keys() {
    let (Ok(redis), Ok(ssh), Ok(user), Ok(key)) = (
        std::env::var("MEDIS_TEST_REDIS"),
        std::env::var("MEDIS_TEST_SSH"),
        std::env::var("MEDIS_TEST_SSH_USER"),
        std::env::var("MEDIS_TEST_SSH_KEY"),
    ) else {
        return;
    };
    let rest = redis.trim_start_matches("redis://");
    let (rh, rp) = rest.rsplit_once(':').unwrap();
    let (sh, sp) = ssh.rsplit_once(':').unwrap();
    let mut cfg = ConnectionConfig {
        host: rh.into(),
        port: rp.into(),
        ssh: true,
        ssh_host: sh.into(),
        ssh_port: sp.into(),
        ssh_user: user,
        ssh_key_file: Some(key.clone()),
        ..Default::default()
    };
    let statuses = std::sync::Mutex::new(Vec::new());
    let c = medis_core::connect_with(&cfg, |s| statuses.lock().unwrap().push(s.to_string()))
        .await
        .unwrap();
    assert_eq!(*statuses.lock().unwrap(), vec!["SSH connecting...", "Redis connecting..."]);
    let mut conn = c.conn.clone();
    assert!(ops::info(&mut conn).await.unwrap().contains_key("redis_version"));
    assert_eq!(c.connection_key, format!("{sh}|{rh}|{rp}"));
    let port = c.tunnel.as_ref().unwrap().local_port();
    drop(c); // tunnel dropped: the local port must stop accepting
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());

    // inline key contents
    cfg.ssh_key_file = None;
    cfg.ssh_key = Some(std::fs::read_to_string(&key).unwrap());
    assert!(connect(&cfg).await.is_ok());

    // encrypted key
    if let Ok(enc) = std::env::var("MEDIS_TEST_SSH_KEY_ENC") {
        cfg.ssh_key = Some(std::fs::read_to_string(&enc).unwrap());
        assert!(cfg.ssh_key_is_encrypted());
        let e = connect(&cfg).await.unwrap_err();
        assert!(e.starts_with("SSH Error: "), "{e}");
        cfg.ssh_key_passphrase = "pp".into();
        assert!(connect(&cfg).await.is_ok());
    }

    // wrong user / password auth is refused
    cfg.ssh_key = None;
    cfg.ssh_password = "nope".into();
    let e = connect(&cfg).await.unwrap_err();
    assert!(e.starts_with("SSH Error: "), "{e}");

    // SSH + cluster is an explicit error
    cfg.cluster.enabled = true;
    let e = connect(&cfg).await.unwrap_err();
    assert!(e.contains("not supported"), "{e}");
}
