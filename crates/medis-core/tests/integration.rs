//! Integration tests against a real Redis. They run only when `MEDIS_TEST_REDIS` is set
//! (e.g. `redis://127.0.0.1:6379`); otherwise every test returns early.
//!
//! The tests use database 9 and unique key prefixes and flush nothing but their own keys.

use std::sync::Arc;
use std::time::Duration;

use medis_core::ops::{self, KeyKind};
use medis_core::scan::{self, ScanCursor, ScanRequest};
use medis_core::worker::{Evt, Op, Reply, Req, Worker, next_req_id};
use medis_core::{ConnectionConfig, RedisConn, connect};

fn test_url() -> Option<String> {
    std::env::var("MEDIS_TEST_REDIS").ok().filter(|s| !s.is_empty())
}

fn cfg_from(url: &str) -> ConnectionConfig {
    let rest = url.trim_start_matches("redis://");
    let (host, port) = rest.rsplit_once(':').unwrap_or((rest, "6379"));
    ConnectionConfig {
        host: host.to_string(),
        port: port.trim_end_matches('/').to_string(),
        ..Default::default()
    }
}

macro_rules! redis_or_skip {
    () => {{
        let Some(url) = test_url() else {
            eprintln!("MEDIS_TEST_REDIS not set; skipping");
            return;
        };
        let c = connect(&cfg_from(&url)).await.expect("connect");
        let mut conn = c.conn.clone();
        ops::select_db(&mut conn, 9).await.expect("select 9");
        (c, conn)
    }};
}

fn prefix(name: &str) -> String {
    format!("medistest:{}:{}:", name, uuid_like())
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{n:x}")
}

async fn cleanup(conn: &mut RedisConn, prefix: &str) {
    let mut cursor = ScanCursor::new();
    let req = ScanRequest {
        pattern: &format!("{prefix}*"),
        first_time: false,
        have_keys: true,
    };
    if let Ok(keys) = scan::scan_collect(conn, &req, &mut cursor).await {
        for k in keys {
            let _ = ops::delete(conn, &k.key).await;
        }
    }
}

#[tokio::test]
async fn connect_and_info() {
    let (c, mut conn) = redis_or_skip!();
    assert!(!c.version.is_empty());
    assert_eq!(c.connection_key, format!("|{}|{}", c.config.host, c.config.port));
    assert!(c.title.ends_with(&format!("{}:{}", c.config.host, c.config.port)));
    let info = ops::info(&mut conn).await.unwrap();
    assert!(info.contains_key("redis_version"));
    let n = ops::db_count(&mut conn, &c.client).await;
    assert!(n >= 10, "db count {n}");
}

#[tokio::test]
async fn connect_failure_message() {
    if test_url().is_none() {
        return;
    }
    let cfg = ConnectionConfig {
        host: "127.0.0.1".into(),
        port: "1".into(),
        ..Default::default()
    };
    let e = connect(&cfg).await.unwrap_err();
    assert!(e.starts_with("Redis Error: Connection failed. ("), "{e}");
}

#[tokio::test]
async fn wrong_password_message() {
    let Some(url) = test_url() else { return };
    let mut cfg = cfg_from(&url);
    cfg.password = "definitely-wrong".into();
    // Servers without requirepass answer AUTH with an error we report as connection failure /
    // access denied; either way connect must fail or (older servers) succeed – just don't panic.
    let _ = connect(&cfg).await;
}

#[tokio::test]
async fn scan_with_pattern_and_plain_key() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("scan");
    for i in 0..30 {
        redis::cmd("SET").arg(format!("{p}k{i}")).arg("v").exec_async(&mut conn).await.unwrap();
    }
    redis::cmd("RPUSH").arg(format!("{p}list")).arg("a").exec_async(&mut conn).await.unwrap();

    let mut cursor = ScanCursor::new();
    let req = ScanRequest { pattern: &format!("{p}*"), first_time: true, have_keys: false };
    let keys = scan::scan_collect(&mut conn, &req, &mut cursor).await.unwrap();
    assert_eq!(keys.len(), 31, "{keys:?}");
    assert!(keys.iter().any(|k| k.key == format!("{p}list") && k.ktype == "list"));
    assert!(keys.iter().any(|k| k.ktype == "string"));
    assert!(!cursor.has_more() || keys.len() >= 31);

    // plain key: exact match first, filtered from the scan, scan stops (first run)
    let mut cursor = ScanCursor::new();
    let name = format!("{p}k1");
    let req = ScanRequest { pattern: &name, first_time: true, have_keys: false };
    let keys = scan::scan_collect(&mut conn, &req, &mut cursor).await.unwrap();
    assert_eq!(keys[0].key, name);
    assert_eq!(keys.iter().filter(|k| k.key == name).count(), 1);

    // missing plain key: nothing exact
    let mut cursor = ScanCursor::new();
    let missing = format!("{p}nope");
    let req = ScanRequest { pattern: &missing, first_time: true, have_keys: false };
    let keys = scan::scan_collect(&mut conn, &req, &mut cursor).await.unwrap();
    assert!(keys.is_empty());

    let ttls = scan::key_ttls(&mut conn, std::slice::from_ref(&name)).await.unwrap();
    assert_eq!(ttls, vec![-1]);
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn string_crud_keeps_ttl_and_binary() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("str");
    let k = format!("{p}s");
    ops::add_key(&mut conn, &k, KeyKind::String).await.unwrap();
    assert_eq!(ops::add_key(&mut conn, &k, KeyKind::String).await.unwrap_err().message, "The key already exists");
    assert_eq!(ops::key_type(&mut conn, &k).await.unwrap(), "string");
    assert_eq!(ops::string_get(&mut conn, &k).await.unwrap(), Some(vec![]));

    let bin = vec![0u8, 1, 2, 255, 254, b'\n'];
    ops::string_set(&mut conn, &k, &bin).await.unwrap();
    assert_eq!(ops::string_get(&mut conn, &k).await.unwrap(), Some(bin.clone()));
    assert_eq!(ops::content_len(&mut conn, &k, "string").await.unwrap(), 6);
    assert_eq!(ops::pttl(&mut conn, &k).await.unwrap(), -1);

    // expire, then save again: TTL kept
    ops::set_expire(&mut conn, &k, 100_000).await.unwrap();
    ops::string_set(&mut conn, &k, b"new").await.unwrap();
    let t = ops::pttl(&mut conn, &k).await.unwrap();
    assert!(t > 0 && t <= 100_000, "ttl {t}");
    assert_eq!(ops::string_get(&mut conn, &k).await.unwrap(), Some(b"new".to_vec()));

    // persist
    ops::set_expire(&mut conn, &k, -1).await.unwrap();
    assert_eq!(ops::pttl(&mut conn, &k).await.unwrap(), -1);
    // PEXPIRE on a missing key -> Update Failed
    let e = ops::set_expire(&mut conn, &format!("{p}missing"), 1000).await.unwrap_err();
    assert_eq!(e.message, "Update Failed");
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn hash_crud() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("hash");
    let k = format!("{p}h");
    ops::add_key(&mut conn, &k, KeyKind::Hash).await.unwrap();
    assert_eq!(ops::content_len(&mut conn, &k, "hash").await.unwrap(), 1);
    ops::hash_add(&mut conn, &k, "f1").await.unwrap();
    assert_eq!(ops::hash_add(&mut conn, &k, "f1").await.unwrap_err().message, "The field already exists");
    ops::hash_set(&mut conn, &k, "f1", b"\x00bin").await.unwrap();
    assert!(ops::hash_exists(&mut conn, &k, "f1").await.unwrap());
    ops::hash_rename(&mut conn, &k, "f1", "f2", b"\x00bin").await.unwrap();
    assert!(!ops::hash_exists(&mut conn, &k, "f1").await.unwrap());

    let page = ops::hash_page(&mut conn, &k, 0).await.unwrap();
    assert_eq!(page.cursor, 0);
    let m: std::collections::HashMap<_, _> = page.items.into_iter().collect();
    assert_eq!(m.len(), 2);
    assert_eq!(m["f2"], b"\x00bin");
    assert_eq!(m["New Key"], b"New Value");

    ops::hash_delete(&mut conn, &k, "f2").await.unwrap();
    assert_eq!(ops::content_len(&mut conn, &k, "hash").await.unwrap(), 1);
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn list_crud_and_paging() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("list");
    let k = format!("{p}l");
    ops::add_key(&mut conn, &k, KeyKind::List).await.unwrap();
    redis::cmd("DEL").arg(&k).exec_async(&mut conn).await.unwrap();
    let mut c = redis::cmd("RPUSH");
    c.arg(&k);
    for i in 0..1500 {
        c.arg(format!("v{i}"));
    }
    c.exec_async(&mut conn).await.unwrap();
    let len = ops::content_len(&mut conn, &k, "list").await.unwrap();
    assert_eq!(len, 1500);

    // asc paging: 201 then 1001 items
    let p1 = ops::list_page(&mut conn, &k, 0, len, false).await.unwrap();
    assert_eq!(p1.items.len(), 201);
    assert_eq!(p1.diff, 0);
    assert_eq!(p1.items[0], b"v0");
    let p2 = ops::list_page(&mut conn, &k, 201, len, false).await.unwrap();
    assert_eq!(p2.items.len(), 1001);
    assert_eq!(p2.items[0], b"v201");
    let p3 = ops::list_page(&mut conn, &k, 1202, len, false).await.unwrap();
    assert_eq!(p3.items.len(), 298);
    assert!(ops::list_page(&mut conn, &k, 1500, len, false).await.unwrap().items.is_empty());

    // desc paging
    let d1 = ops::list_page(&mut conn, &k, 0, len, true).await.unwrap();
    assert_eq!(d1.items.len(), 201);
    assert_eq!(d1.items[0], b"v1499");
    assert_eq!(d1.items[200], b"v1299");

    // LSET by display row (desc row 0 = last element)
    ops::list_set(&mut conn, &k, ops::list_redis_index(0, true), b"LAST").await.unwrap();
    let tail: Vec<Vec<u8>> = redis::cmd("LRANGE").arg(&k).arg(-1).arg(-1).query_async(&mut conn).await.unwrap();
    assert_eq!(tail[0], b"LAST");

    // remove by index via the lremindex script, then head/tail pushes
    ops::list_remove(&mut conn, &k, 1).await.unwrap();
    assert_eq!(ops::content_len(&mut conn, &k, "list").await.unwrap(), 1499);
    let head: Vec<Vec<u8>> = redis::cmd("LRANGE").arg(&k).arg(0).arg(1).query_async(&mut conn).await.unwrap();
    assert_eq!(head, vec![b"v0".to_vec(), b"v2".to_vec()]);
    ops::list_push(&mut conn, &k, true).await.unwrap();
    let first: Vec<Vec<u8>> = redis::cmd("LRANGE").arg(&k).arg(0).arg(0).query_async(&mut conn).await.unwrap();
    assert_eq!(first[0], b"New Item");
    // stale length -> diff > 0
    let shrink = ops::list_page(&mut conn, &k, 1495, 1600, false).await.unwrap();
    assert!(shrink.diff > 0);
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn set_crud() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("set");
    let k = format!("{p}s");
    ops::add_key(&mut conn, &k, KeyKind::Set).await.unwrap();
    ops::set_add(&mut conn, &k, b"m1").await.unwrap();
    assert_eq!(ops::set_add(&mut conn, &k, b"m1").await.unwrap_err().message, "Member already exists");
    assert_eq!(
        ops::set_replace(&mut conn, &k, b"m1", b"New Member").await.unwrap_err().message,
        "The value already exists in the set"
    );
    ops::set_replace(&mut conn, &k, b"m1", b"m2").await.unwrap();
    let page = ops::set_page(&mut conn, &k, 0).await.unwrap();
    assert_eq!(page.cursor, 0);
    let mut items = page.items;
    items.sort();
    assert_eq!(items, vec![b"New Member".to_vec(), b"m2".to_vec()]);
    ops::set_remove(&mut conn, &k, b"m2").await.unwrap();
    assert_eq!(ops::content_len(&mut conn, &k, "set").await.unwrap(), 1);
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn zset_crud() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("zset");
    let k = format!("{p}z");
    ops::add_key(&mut conn, &k, KeyKind::ZSet).await.unwrap();
    ops::zset_add(&mut conn, &k, b"a", "2.5").await.unwrap();
    ops::zset_add(&mut conn, &k, b"b", "10").await.unwrap();
    assert_eq!(ops::zset_add(&mut conn, &k, b"a", "1").await.unwrap_err().message, "Member already exists");
    let len = ops::content_len(&mut conn, &k, "zset").await.unwrap();
    assert_eq!(len, 3);
    let asc = ops::zset_page(&mut conn, &k, 0, len, false).await.unwrap();
    let names: Vec<_> = asc.items.iter().map(|(m, _)| m.clone()).collect();
    assert_eq!(names, vec![b"New Member".to_vec(), b"a".to_vec(), b"b".to_vec()]);
    assert_eq!(asc.items[1].1, "2.5");
    let desc = ops::zset_page(&mut conn, &k, 0, len, true).await.unwrap();
    assert_eq!(desc.items[0].0, b"b");

    ops::zset_set_score(&mut conn, &k, b"a", "99").await.unwrap();
    ops::zset_replace(&mut conn, &k, b"a", b"a2", "99").await.unwrap();
    let asc = ops::zset_page(&mut conn, &k, 0, len, false).await.unwrap();
    assert_eq!(asc.items.last().unwrap(), &(b"a2".to_vec(), "99".to_string()));
    ops::zset_remove(&mut conn, &k, b"a2").await.unwrap();
    assert_eq!(ops::content_len(&mut conn, &k, "zset").await.unwrap(), 2);
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn rename_delete_duplicate() {
    let (_c, mut conn) = redis_or_skip!();
    let p = prefix("key");
    let (a, b, c, d) = (format!("{p}a"), format!("{p}b"), format!("{p}c"), format!("{p}d"));
    ops::add_key(&mut conn, &a, KeyKind::Hash).await.unwrap();
    ops::rename(&mut conn, &a, &b).await.unwrap();
    assert!(!ops::exists(&mut conn, &a).await.unwrap());
    assert!(ops::exists(&mut conn, &b).await.unwrap());
    assert!(ops::rename(&mut conn, &a, &b).await.is_err()); // missing source

    // duplicate without TTL
    ops::duplicate(&mut conn, &b, &c, false).await.unwrap();
    assert_eq!(ops::key_type(&mut conn, &c).await.unwrap(), "hash");
    assert_eq!(ops::pttl(&mut conn, &c).await.unwrap(), -1);
    // existing target -> BUSYKEY
    let e = ops::duplicate(&mut conn, &b, &c, false).await.unwrap_err();
    assert!(e.message.contains("BUSYKEY"), "{}", e.message);

    // keep TTL on a source with a TTL
    ops::set_expire(&mut conn, &b, 50_000).await.unwrap();
    ops::duplicate(&mut conn, &b, &d, true).await.unwrap();
    let t = ops::pttl(&mut conn, &d).await.unwrap();
    assert!(t > 0 && t <= 50_000, "{t}");
    // keep TTL on a persistent source (the ported fix): works, no TTL
    ops::set_expire(&mut conn, &b, -1).await.unwrap();
    ops::delete(&mut conn, &d).await.unwrap();
    ops::duplicate(&mut conn, &b, &d, true).await.unwrap();
    assert_eq!(ops::pttl(&mut conn, &d).await.unwrap(), -1);

    ops::delete(&mut conn, &b).await.unwrap();
    assert!(!ops::exists(&mut conn, &b).await.unwrap());
    assert_eq!(ops::key_type(&mut conn, &b).await.unwrap(), "none");
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn db_count_and_select() {
    let (c, mut conn) = redis_or_skip!();
    let n = ops::db_count(&mut conn, &c.client).await;
    assert!(n >= 1);
    ops::select_db(&mut conn, 9).await.unwrap();
    let p = prefix("db");
    let k = format!("{p}k");
    redis::cmd("SET").arg(&k).arg("1").exec_async(&mut conn).await.unwrap();
    let info = ops::info(&mut conn).await.unwrap();
    assert!(ops::db_key_count(&info, 9) >= 1);
    // wrong db number
    assert!(ops::select_db(&mut conn, 100_000).await.is_err());
    cleanup(&mut conn, &p).await;
}

#[tokio::test]
async fn raw_exec() {
    let (_c, mut conn) = redis_or_skip!();
    let v = ops::exec(&mut conn, &[b"PING".to_vec()]).await.unwrap();
    assert!(matches!(v, redis::Value::SimpleString(ref s) if s == "PONG"), "{v:?}");
    let e = ops::exec(&mut conn, &[b"NOSUCHCMD".to_vec()]).await.unwrap_err();
    assert!(e.message.to_lowercase().contains("unknown command"), "{}", e.message);
}

async fn recv(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Evt>) -> Evt {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("worker closed")
}

async fn wait_reply(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Evt>, want: u64) -> Result<Reply, medis_core::CoreError> {
    loop {
        if let Evt::Reply { id, result } = recv(rx).await
            && id == want
        {
            return result;
        }
    }
}

#[tokio::test]
async fn worker_roundtrip() {
    let Some(url) = test_url() else { return };
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n2 = notified.clone();
    let (tx, mut rx) = Worker::spawn(
        cfg_from(&url),
        Arc::new(move || {
            n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );

    // requests sent before the connection completes are queued
    let sel = next_req_id();
    tx.send(Req::new(sel, Op::SelectDb(9))).unwrap();

    let mut saw_status = false;
    loop {
        match recv(&mut rx).await {
            Evt::Status(s) => {
                assert!(s == "Redis connecting..." || s == "SSH connecting...");
                saw_status = true;
            }
            Evt::Connected(info) => {
                assert!(!info.version.is_empty());
                assert!(!info.cluster);
                break;
            }
            Evt::ConnectFailed(m) => panic!("connect failed: {m}"),
            _ => {}
        }
    }
    assert!(saw_status);
    wait_reply(&mut rx, sel).await.unwrap();

    let p = prefix("worker");
    let k = format!("{p}key");
    let id = next_req_id();
    tx.send(Req::new(id, Op::AddKey { key: k.clone(), kind: KeyKind::List })).unwrap();
    wait_reply(&mut rx, id).await.unwrap();
    let id = next_req_id();
    tx.send(Req::new(id, Op::AddKey { key: k.clone(), kind: KeyKind::List })).unwrap();
    assert_eq!(wait_reply(&mut rx, id).await.unwrap_err().message, "The key already exists");

    let id = next_req_id();
    tx.send(Req::new(id, Op::OpenKey(k.clone()))).unwrap();
    match wait_reply(&mut rx, id).await.unwrap() {
        Reply::KeyOpened { ktype, len } => {
            assert_eq!(ktype, "list");
            assert_eq!(len, Some(1));
        }
        other => panic!("{other:?}"),
    }

    // scan streaming
    let scan_id = next_req_id();
    tx.send(Req::new(
        scan_id,
        Op::Scan { pattern: format!("{p}*"), first_time: true, have_keys: false, cursor: ScanCursor::new() },
    ))
    .unwrap();
    let mut keys = Vec::new();
    loop {
        match recv(&mut rx).await {
            Evt::ScanBatch { id, keys: k, .. } if id == scan_id => keys.extend(k),
            Evt::ScanDone { id, result } if id == scan_id => {
                result.unwrap();
                break;
            }
            _ => {}
        }
    }
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].ktype, "list");

    // terminal: SELECT emits DbSelected
    let id = next_req_id();
    tx.send(Req::new(id, Op::Exec(vec![b"SELECT".to_vec(), b"9".to_vec()]))).unwrap();
    let mut db_sel = false;
    loop {
        match recv(&mut rx).await {
            Evt::DbSelected(9) => db_sel = true,
            Evt::Reply { id: i, result } if i == id => {
                result.unwrap();
                break;
            }
            _ => {}
        }
    }
    assert!(db_sel);

    // pub/sub
    let sub_id = next_req_id();
    let chan = format!("{p}chan");
    tx.send(Req::new(sub_id, Op::PubSubStart { channels: vec![chan.clone()], patterns: vec![] })).unwrap();
    wait_reply(&mut rx, sub_id).await.unwrap();
    let id = next_req_id();
    tx.send(Req::new(id, Op::Exec(vec![b"PUBLISH".to_vec(), chan.clone().into_bytes(), b"hello".to_vec()]))).unwrap();
    loop {
        if let Evt::PubSub(m) = recv(&mut rx).await {
            assert_eq!(m.channel, chan);
            assert_eq!(m.payload, b"hello");
            break;
        }
    }

    // monitor
    let mon = next_req_id();
    tx.send(Req::new(mon, Op::MonitorStart)).unwrap();
    wait_reply(&mut rx, mon).await.unwrap();
    let id = next_req_id();
    tx.send(Req::new(id, Op::Exec(vec![b"PING".to_vec()]))).unwrap();
    loop {
        if let Evt::Monitor(line) = recv(&mut rx).await
            && line.to_lowercase().contains("ping")
        {
            break;
        }
    }

    let id = next_req_id();
    tx.send(Req::new(id, Op::Delete(k.clone()))).unwrap();
    wait_reply(&mut rx, id).await.unwrap();

    let id = next_req_id();
    tx.send(Req::new(id, Op::DbCount)).unwrap();
    assert!(matches!(wait_reply(&mut rx, id).await.unwrap(), Reply::DbCount(n) if n >= 1));

    tx.send(Req::new(0, Op::Disconnect)).unwrap();
    loop {
        if matches!(recv(&mut rx).await, Evt::Closed) {
            break;
        }
    }
    assert!(notified.load(std::sync::atomic::Ordering::SeqCst) > 5);
}

#[tokio::test]
async fn worker_connect_failure() {
    if test_url().is_none() {
        return;
    }
    let cfg = ConnectionConfig { host: "127.0.0.1".into(), port: "1".into(), ..Default::default() };
    let (_tx, mut rx) = Worker::spawn(cfg, Arc::new(|| {}));
    loop {
        match recv(&mut rx).await {
            Evt::ConnectFailed(m) => {
                assert!(m.starts_with("Redis Error: Connection failed."), "{m}");
                break;
            }
            Evt::Connected(_) => panic!("unexpected connect"),
            _ => {}
        }
    }
    assert!(matches!(recv(&mut rx).await, Evt::Closed));
}
