//! Cluster smoke test; needs `MEDIS_TEST_CLUSTER=host:port` (a seed node). Skipped when unset.

use medis_core::ops::{self, KeyKind};
use medis_core::scan::{self, ScanCursor, ScanRequest};
use medis_core::{ConnectionConfig, connect};

#[tokio::test]
async fn cluster_scan_and_ops() {
    let Ok(seed) = std::env::var("MEDIS_TEST_CLUSTER") else { return };
    let (host, port) = seed.rsplit_once(':').unwrap();
    let mut cfg = ConnectionConfig { host: host.into(), port: port.into(), ..Default::default() };
    cfg.cluster.enabled = true;
    let c = connect(&cfg).await.unwrap();
    let mut conn = c.conn.clone();
    assert!(conn.is_cluster());
    assert_eq!(ops::db_count(&mut conn, &c.client).await, 1);
    for i in 0..300 {
        redis::cmd("SET").arg(format!("medisc:{i}")).arg("v").exec_async(&mut conn).await.unwrap();
    }
    let mut cur = ScanCursor::new();
    let req = ScanRequest { pattern: "medisc:*", first_time: true, have_keys: false };
    let mut all = Vec::new();
    loop {
        let batch = scan::scan_collect(&mut conn, &req, &mut cur).await.unwrap();
        all.extend(batch);
        if !cur.has_more() { break; }
    }
    all.sort_by(|a, b| a.key.cmp(&b.key));
    all.dedup_by(|a, b| a.key == b.key);
    assert_eq!(all.len(), 300);
    assert!(all.iter().all(|k| k.ktype == "string"));
    let info = ops::info(&mut conn).await.unwrap();
    assert!(ops::db_key_count(&info, 0) >= 300);
    ops::add_key(&mut conn, "medisc:{t}h", KeyKind::Hash).await.unwrap();
    ops::duplicate(&mut conn, "medisc:{t}h", "medisc:{t}h2", true).await.unwrap();
    for i in 0..300 { ops::delete(&mut conn, &format!("medisc:{i}")).await.unwrap(); }
    ops::delete(&mut conn, "medisc:{t}h").await.unwrap();
    ops::delete(&mut conn, "medisc:{t}h2").await.unwrap();
}
