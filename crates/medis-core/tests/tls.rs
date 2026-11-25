//! TLS smoke test. Needs `MEDIS_TEST_TLS=host:port` and `MEDIS_TEST_TLS_CA=/path/ca.crt`
//! (server cert valid for `localhost`/127.0.0.1, no client-cert requirement). Skipped otherwise.

use medis_core::{ConnectionConfig, connect};

#[tokio::test]
async fn tls_modes() {
    let (Ok(hp), Ok(ca)) = (std::env::var("MEDIS_TEST_TLS"), std::env::var("MEDIS_TEST_TLS_CA")) else {
        return;
    };
    let (host, port) = hp.rsplit_once(':').unwrap();
    let mut cfg = ConnectionConfig { host: host.into(), port: port.into(), ..Default::default() };
    cfg.tls.enabled = true;

    // insecure (default; the original's rejectUnauthorized:false)
    let r = connect(&cfg).await;
    assert!(r.is_ok(), "{:?}", r.err());

    // verified with the CA given by path
    cfg.tls.insecure = false;
    cfg.tls.ca_path = ca.clone();
    assert!(connect(&cfg).await.is_ok());

    // verified, but CA unknown -> fails
    cfg.tls.ca_path.clear();
    assert!(connect(&cfg).await.is_err());

    // Hostname mismatch: needs a second server whose certificate is only valid for another
    // name, given as MEDIS_TEST_TLS_BADHOST=host:port. Optional.
    let Ok(bad) = std::env::var("MEDIS_TEST_TLS_BADHOST") else { return };
    let (bhost, bport) = bad.rsplit_once(':').unwrap();
    let mut cfg = ConnectionConfig { host: bhost.into(), port: bport.into(), ..Default::default() };
    cfg.tls.enabled = true;
    cfg.tls.insecure = false;
    cfg.tls_ca = std::fs::read_to_string(&ca).unwrap(); // inline contents
    assert!(connect(&cfg).await.is_err());
    cfg.tls.skip_hostname = true;
    let c = connect(&cfg).await;
    assert!(c.is_ok(), "{:?}", c.err());
}
