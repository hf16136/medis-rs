//! Auth error mapping; needs a password-protected server in `MEDIS_TEST_REDIS_AUTH`
//! (`host:port`, password `sekret`). Skipped when unset.

use medis_core::{ConnectionConfig, connect};

#[tokio::test]
async fn noauth_and_wrongpass_map_to_access_denied() {
    let Ok(hp) = std::env::var("MEDIS_TEST_REDIS_AUTH") else { return };
    let (host, port) = hp.rsplit_once(':').unwrap();
    let mut cfg = ConnectionConfig { host: host.into(), port: port.into(), ..Default::default() };
    let denied = "Redis Error: Access denied. Please double-check your password.";
    assert_eq!(connect(&cfg).await.unwrap_err(), denied);
    cfg.password = "wrong".into();
    assert_eq!(connect(&cfg).await.unwrap_err(), denied);
    cfg.password = "sekret".into();
    assert!(connect(&cfg).await.is_ok());
}
