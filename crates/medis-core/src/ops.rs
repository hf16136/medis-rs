//! Typed, async key operations used by the UI (SPEC-2). Every function takes a
//! `&mut` [`RedisConn`] (cheap to clone, shared multiplexed connection).
//!
//! Paging helpers return exactly what the original views need:
//!
//! * hash / set: `HSCAN` / `SSCAN` pages ([`HashPage`], [`SetPage`]; `cursor == 0` = done),
//! * list / zset: `LRANGE` / `ZRANGE` windows ([`SeqPage`], `diff` = expected - received, the
//!   caller does `length -= diff`).
//!
//! Errors are [`CoreError`]s whose `message` is what the alert should show.

use std::collections::HashMap;

use redis::{Cmd, FromRedisValue, Value};
use redis::cluster_routing::{RoutingInfo, SingleNodeRoutingInfo};

use crate::conn::RedisConn;
use crate::error::{CoreError, Result, redis_error_message};
use crate::scan::cluster_primaries;
use crate::scripts;

/// First-call `COUNT` for HSCAN / SSCAN.
pub const SCAN_COUNT_FIRST: u32 = 500;
/// Subsequent `COUNT` for HSCAN / SSCAN.
pub const SCAN_COUNT_NEXT: u32 = 10_000;
/// First LRANGE/ZRANGE window (rows `0..=200` → 201 items).
pub const RANGE_FIRST: u64 = 200;
/// Subsequent LRANGE/ZRANGE window size (+1000).
pub const RANGE_NEXT: u64 = 1000;

/// Placeholder value of a new list item.
pub const NEW_ITEM: &str = "New Item";
/// Placeholder value of a new hash value / set or zset member.
pub const NEW_MEMBER: &str = "New Member";
/// Placeholder field of a new key's hash.
pub const NEW_KEY: &str = "New Key";
/// Placeholder value of a new key's hash.
pub const NEW_VALUE: &str = "New Value";

/// Types that can be created from the "add key" dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    /// `SET key ''`
    String,
    /// `HSET key 'New Key' 'New Value'`
    Hash,
    /// `LPUSH key 'New Item'`
    List,
    /// `SADD key 'New Member'`
    Set,
    /// `ZADD key 0 'New Member'`
    ZSet,
}

impl KeyKind {
    /// All kinds in dialog order.
    pub const ALL: [KeyKind; 5] = [Self::String, Self::Hash, Self::List, Self::Set, Self::ZSet];

    /// Label as shown in the dialog.
    pub fn label(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Hash => "hash",
            Self::List => "list",
            Self::Set => "set",
            Self::ZSet => "zset",
        }
    }
}

// ---- generic helpers -------------------------------------------------------------------------

/// Runs a command on any node (cluster: random node) – for node-local commands like INFO.
pub(crate) async fn query_any<T: FromRedisValue>(conn: &mut RedisConn, cmd: Cmd) -> Result<T> {
    match conn {
        RedisConn::Single(_) => Ok(cmd.query_async(conn).await?),
        RedisConn::Cluster(c) => {
            let v = c
                .route_command(cmd, RoutingInfo::SingleNode(SingleNodeRoutingInfo::Random))
                .await?;
            Ok(redis::from_redis_value(v.extract_error()?)?)
        }
    }
}

// ---- INFO / DB -------------------------------------------------------------------------------

/// Parses `INFO` output: lines split on `\r\n`, each on the first `:`; section headers and lines
/// without a value are skipped.
pub fn parse_info(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for line in text.split("\r\n").flat_map(|l| l.split('\n')) {
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && !v.is_empty()
        {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

/// Key count of database `db` from a parsed INFO map (`db<N>` → `keys=(\d+)`, else 0).
pub fn db_key_count(info: &HashMap<String, String>, db: u32) -> u64 {
    info.get(&format!("db{db}"))
        .and_then(|v| v.split(',').find_map(|p| p.trim().strip_prefix("keys=")))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Footer label of the DB selector: `"<db> (<count>)"` if count > 0, else `"<db>"`.
pub fn db_label(info: &HashMap<String, String>, db: u32) -> String {
    match db_key_count(info, db) {
        0 => db.to_string(),
        n => format!("{db} ({n})"),
    }
}

/// `INFO` parsed to a map. In cluster mode the INFO of a random node is used and `db0` is
/// replaced by `keys=<sum of DBSIZE over all primaries>`.
pub async fn info(conn: &mut RedisConn) -> Result<HashMap<String, String>> {
    let text: String = query_any(conn, redis::cmd("INFO")).await?;
    let mut map = parse_info(&text);
    if conn.is_cluster() {
        let mut total = 0u64;
        let primaries = cluster_primaries(conn).await?;
        if let RedisConn::Cluster(c) = conn {
            for (host, port) in primaries {
                let v = c
                    .route_command(
                        redis::cmd("DBSIZE"),
                        RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress { host, port }),
                    )
                    .await?;
                total += redis::from_redis_value::<u64>(v.extract_error()?)?;
            }
        }
        map.insert("db0".into(), format!("keys={total},expires=0"));
    }
    Ok(map)
}

/// `SELECT db`. Cluster connections only accept db 0.
pub async fn select_db(conn: &mut RedisConn, db: u32) -> Result<()> {
    if conn.is_cluster() {
        return if db == 0 {
            Ok(())
        } else {
            Err("Cluster mode only supports database 0".into())
        };
    }
    redis::cmd("SELECT").arg(db).exec_async(conn).await?;
    Ok(())
}

/// Number of databases (`databases = last valid index + 1`), computed once per connection.
///
/// 1. `CONFIG GET databases`;
/// 2. else probe on a *separate* connection from `client` (closed afterwards): `SELECT 15`,
///    16, … up to 30 returning the last success; if `SELECT 15` fails probe `SELECT 1`, 2, …;
///    if `SELECT 1` fails the result is 0 (→ 1 database).
///
/// Cluster: always 1.
pub async fn db_count(conn: &mut RedisConn, client: &redis::Client) -> u32 {
    if conn.is_cluster() {
        return 1;
    }
    let got: std::result::Result<Vec<String>, _> = redis::cmd("CONFIG")
        .arg("GET")
        .arg("databases")
        .query_async(conn)
        .await;
    if let Ok(v) = got
        && let Some(n) = v.get(1).and_then(|s| s.parse::<u32>().ok())
        && n > 0
    {
        return n;
    }
    let cfg = redis::AsyncConnectionConfig::new()
        .set_connection_timeout(Some(crate::conn::CONNECT_TIMEOUT))
        .set_response_timeout(Some(crate::conn::RESPONSE_TIMEOUT));
    let Ok(probe) = client
        .get_multiplexed_async_connection_with_config(&cfg)
        .await
    else {
        return 1;
    };
    let mut probe = Probe(probe);
    let last = guess_db_number(&mut probe, 15, None).await;
    let last = match last {
        Some(n) => n,
        None => guess_db_number(&mut probe, 1, Some(0)).await.unwrap_or(0),
    };
    last + 1
}

/// Something that can `SELECT n` and tell whether it worked (a trait so the probing logic is
/// testable without a server).
pub trait DbSelector: Send {
    /// Tries `SELECT i`.
    fn select(&mut self, i: u32) -> futures::future::BoxFuture<'_, bool>;
}

struct Probe(redis::aio::MultiplexedConnection);

impl DbSelector for Probe {
    fn select(&mut self, i: u32) -> futures::future::BoxFuture<'_, bool> {
        Box::pin(async move {
            redis::cmd("SELECT").arg(i).exec_async(&mut self.0).await.is_ok()
        })
    }
}

/// The `guessDatabaseNumber` recursion of the original, iteratively: tries `SELECT start`,
/// `start+1`, ... up to 30 and returns the last success (`Some(30)` on reaching the cap), or
/// `last_ok` when the first attempt fails.
pub async fn guess_db_number<S: DbSelector>(
    select: &mut S,
    start: u32,
    last_ok: Option<u32>,
) -> Option<u32> {
    let mut i = start;
    let mut last = last_ok;
    loop {
        if i > 30 {
            return Some(30);
        }
        if select.select(i).await {
            last = Some(i);
            i += 1;
        } else {
            return last;
        }
    }
}

// ---- key level -------------------------------------------------------------------------------

/// `TYPE key` (`none` when missing).
pub async fn key_type(conn: &mut RedisConn, key: &str) -> Result<String> {
    crate::scan::key_type(conn, key).await
}

/// `PTTL key` (`-1` persistent, `-2` missing).
pub async fn pttl(conn: &mut RedisConn, key: &str) -> Result<i64> {
    Ok(redis::cmd("PTTL").arg(key).query_async(conn).await?)
}

/// `EXISTS key`.
pub async fn exists(conn: &mut RedisConn, key: &str) -> Result<bool> {
    let n: i64 = redis::cmd("EXISTS").arg(key).query_async(conn).await?;
    Ok(n > 0)
}

/// Creates `key` with a placeholder of `kind`. Error `The key already exists` if present.
pub async fn add_key(conn: &mut RedisConn, key: &str, kind: KeyKind) -> Result<()> {
    if exists(conn, key).await? {
        return Err("The key already exists".into());
    }
    let c = match kind {
        KeyKind::String => {
            let mut c = redis::cmd("SET");
            c.arg(key).arg("");
            c
        }
        KeyKind::List => {
            let mut c = redis::cmd("LPUSH");
            c.arg(key).arg(NEW_ITEM);
            c
        }
        KeyKind::Hash => {
            let mut c = redis::cmd("HSET");
            c.arg(key).arg(NEW_KEY).arg(NEW_VALUE);
            c
        }
        KeyKind::Set => {
            let mut c = redis::cmd("SADD");
            c.arg(key).arg(NEW_MEMBER);
            c
        }
        KeyKind::ZSet => {
            let mut c = redis::cmd("ZADD");
            c.arg(key).arg(0).arg(NEW_MEMBER);
            c
        }
    };
    c.query_async::<Value>(conn).await?;
    Ok(())
}

/// `RENAME old new` (overwrites `new`; the UI confirms via [`exists`] first).
pub async fn rename(conn: &mut RedisConn, old: &str, new: &str) -> Result<()> {
    redis::cmd("RENAME").arg(old).arg(new).exec_async(conn).await?;
    Ok(())
}

/// `DEL key`.
pub async fn delete(conn: &mut RedisConn, key: &str) -> Result<()> {
    redis::cmd("DEL").arg(key).exec_async(conn).await?;
    Ok(())
}

/// Sets the expiration: `pttl_ms >= 0` → `PEXPIRE` (a reply `<= 0` → error `Update Failed`),
/// `< 0` → `PERSIST`.
pub async fn set_expire(conn: &mut RedisConn, key: &str, pttl_ms: i64) -> Result<()> {
    if pttl_ms >= 0 {
        let r: i64 = redis::cmd("PEXPIRE")
            .arg(key)
            .arg(pttl_ms)
            .query_async(conn)
            .await?;
        if r <= 0 {
            return Err("Update Failed".into());
        }
    } else {
        redis::cmd("PERSIST").arg(key).exec_async(conn).await?;
    }
    Ok(())
}

/// Duplicates `source` to `target` with the `duplicateKey` script (non-REPLACE: an existing
/// target gives a `BUSYKEY` error). `keep_ttl` copies the remaining TTL (0 when the source has
/// none). In cluster mode both keys must share a slot (script limitation).
pub async fn duplicate(
    conn: &mut RedisConn,
    source: &str,
    target: &str,
    keep_ttl: bool,
) -> Result<()> {
    let mut inv = scripts::duplicate_key().key(source);
    inv.key(target).arg(if keep_ttl { "TTL" } else { "NOTTL" });
    inv.invoke_async::<Value>(conn).await?;
    Ok(())
}

// ---- content: lengths ------------------------------------------------------------------------

/// Length of a key's value by `TYPE`: STRLEN / LLEN / SCARD / ZCARD / HLEN.
pub async fn content_len(conn: &mut RedisConn, key: &str, ktype: &str) -> Result<u64> {
    let name = match ktype {
        "string" => "STRLEN",
        "list" => "LLEN",
        "set" => "SCARD",
        "zset" => "ZCARD",
        "hash" => "HLEN",
        other => return Err(CoreError::msg(format!("Unsupported type: {other}"))),
    };
    Ok(redis::cmd(name).arg(key).query_async(conn).await?)
}

// ---- string ----------------------------------------------------------------------------------

/// `GET key` as raw bytes (`None` if missing).
pub async fn string_get(conn: &mut RedisConn, key: &str) -> Result<Option<Vec<u8>>> {
    Ok(redis::cmd("GET").arg(key).query_async(conn).await?)
}

/// Saves a string with the `setKeepTTL` script (binary-safe, keeps the remaining TTL).
pub async fn string_set(conn: &mut RedisConn, key: &str, value: &[u8]) -> Result<()> {
    let mut inv = scripts::set_keep_ttl().key(key);
    inv.arg(value);
    inv.invoke_async::<Value>(conn).await?;
    Ok(())
}

// ---- hash ------------------------------------------------------------------------------------

/// One `HSCAN` page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashPage {
    /// `(field, value)` pairs in scan order (field lossy UTF-8). May contain duplicates across
    /// pages (SCAN guarantee); the caller de-duplicates.
    pub items: Vec<(String, Vec<u8>)>,
    /// Next cursor; `0` = complete (do not restart from 0 afterwards).
    pub cursor: u64,
}

/// `HSCAN key cursor MATCH * COUNT n` with n = 500 for the first call (cursor 0), else 10000.
pub async fn hash_page(conn: &mut RedisConn, key: &str, cursor: u64) -> Result<HashPage> {
    let count = if cursor == 0 { SCAN_COUNT_FIRST } else { SCAN_COUNT_NEXT };
    let (cursor, flat): (u64, Vec<Vec<u8>>) = redis::cmd("HSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg("*")
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;
    let mut it = flat.into_iter();
    let mut items = Vec::new();
    while let (Some(f), Some(v)) = (it.next(), it.next()) {
        items.push((String::from_utf8_lossy(&f).into_owned(), v));
    }
    Ok(HashPage { items, cursor })
}

/// `HSET key field value`.
pub async fn hash_set(conn: &mut RedisConn, key: &str, field: &str, value: &[u8]) -> Result<()> {
    redis::cmd("HSET").arg(key).arg(field).arg(value).exec_async(conn).await?;
    Ok(())
}

/// `HSETNX key field 'New Member'`; error `The field already exists` when it was present.
pub async fn hash_add(conn: &mut RedisConn, key: &str, field: &str) -> Result<()> {
    let r: i64 = redis::cmd("HSETNX")
        .arg(key)
        .arg(field)
        .arg(NEW_MEMBER)
        .query_async(conn)
        .await?;
    if r == 0 {
        return Err("The field already exists".into());
    }
    Ok(())
}

/// `HEXISTS key field`.
pub async fn hash_exists(conn: &mut RedisConn, key: &str, field: &str) -> Result<bool> {
    let r: i64 = redis::cmd("HEXISTS").arg(key).arg(field).query_async(conn).await?;
    Ok(r > 0)
}

/// Renames a hash field: `MULTI; HDEL key src; HSET key target value; EXEC`.
pub async fn hash_rename(
    conn: &mut RedisConn,
    key: &str,
    src: &str,
    target: &str,
    value: &[u8],
) -> Result<()> {
    let mut p = redis::pipe();
    p.atomic()
        .cmd("HDEL").arg(key).arg(src)
        .cmd("HSET").arg(key).arg(target).arg(value);
    p.query_async::<Value>(conn).await?;
    Ok(())
}

/// `HDEL key field`.
pub async fn hash_delete(conn: &mut RedisConn, key: &str, field: &str) -> Result<()> {
    redis::cmd("HDEL").arg(key).arg(field).exec_async(conn).await?;
    Ok(())
}

// ---- list ------------------------------------------------------------------------------------

/// A window of a list/zset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeqPage<T> {
    /// Items in display order (already reversed for `desc`).
    pub items: Vec<T>,
    /// `expected - received`; the caller does `length -= diff` and continues loading only when
    /// `diff == 0`.
    pub diff: i64,
}

/// Computes `(from, to)` (row indexes, inclusive) of the next window, or `None` when finished.
///
/// `from = loaded`, `to = min(from == 0 ? 200 : from + 1000, length - 1)`.
pub fn next_window(loaded: u64, length: u64) -> Option<(u64, u64)> {
    let from = loaded;
    let to = (if from == 0 { RANGE_FIRST } else { from + RANGE_NEXT }).min(length.checked_sub(1)?);
    (to >= from).then_some((from, to))
}

/// Loads the next list window: `LRANGE key from to` (negative indexes + reversal for `desc`).
pub async fn list_page(
    conn: &mut RedisConn,
    key: &str,
    loaded: u64,
    length: u64,
    desc: bool,
) -> Result<SeqPage<Vec<u8>>> {
    let Some((from, to)) = next_window(loaded, length) else {
        return Ok(SeqPage::default());
    };
    let (a, b) = if desc {
        (-1 - to as i64, -1 - from as i64)
    } else {
        (from as i64, to as i64)
    };
    let mut items: Vec<Vec<u8>> = redis::cmd("LRANGE")
        .arg(key)
        .arg(a)
        .arg(b)
        .query_async(conn)
        .await?;
    if desc {
        items.reverse();
    }
    let diff = (to - from + 1) as i64 - items.len() as i64;
    Ok(SeqPage { items, diff })
}

/// The Redis index of display row `row`: `desc ? -1 - row : row`.
pub fn list_redis_index(row: u64, desc: bool) -> i64 {
    if desc { -1 - row as i64 } else { row as i64 }
}

/// `LSET key index value` (`index` = [`list_redis_index`]).
pub async fn list_set(conn: &mut RedisConn, key: &str, index: i64, value: &[u8]) -> Result<()> {
    redis::cmd("LSET").arg(key).arg(index).arg(value).exec_async(conn).await?;
    Ok(())
}

/// `LPUSH` (head) or `RPUSH` (tail) of `'New Item'`.
pub async fn list_push(conn: &mut RedisConn, key: &str, head: bool) -> Result<()> {
    redis::cmd(if head { "LPUSH" } else { "RPUSH" })
        .arg(key)
        .arg(NEW_ITEM)
        .query_async::<Value>(conn)
        .await?;
    Ok(())
}

/// Removes a list element by index with the `lremindex` script.
pub async fn list_remove(conn: &mut RedisConn, key: &str, index: i64) -> Result<()> {
    let mut inv = scripts::lrem_index().key(key);
    inv.arg(index);
    inv.invoke_async::<Value>(conn).await?;
    Ok(())
}

// ---- set -------------------------------------------------------------------------------------

/// One `SSCAN` page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetPage {
    /// Members in scan order (may repeat across pages).
    pub items: Vec<Vec<u8>>,
    /// Next cursor; `0` = complete (then `length = loaded count`).
    pub cursor: u64,
}

/// `SSCAN key cursor COUNT n` (500 first, then 10000).
pub async fn set_page(conn: &mut RedisConn, key: &str, cursor: u64) -> Result<SetPage> {
    let count = if cursor == 0 { SCAN_COUNT_FIRST } else { SCAN_COUNT_NEXT };
    let (cursor, items): (u64, Vec<Vec<u8>>) = redis::cmd("SSCAN")
        .arg(key)
        .arg(cursor)
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;
    Ok(SetPage { items, cursor })
}

/// `SISMEMBER key member`.
pub async fn set_is_member(conn: &mut RedisConn, key: &str, member: &[u8]) -> Result<bool> {
    let r: i64 = redis::cmd("SISMEMBER").arg(key).arg(member).query_async(conn).await?;
    Ok(r > 0)
}

/// Adds a member; error `Member already exists` if present.
pub async fn set_add(conn: &mut RedisConn, key: &str, member: &[u8]) -> Result<()> {
    if set_is_member(conn, key, member).await? {
        return Err("Member already exists".into());
    }
    redis::cmd("SADD").arg(key).arg(member).exec_async(conn).await?;
    Ok(())
}

/// Replaces a member (`SISMEMBER new` → error `The value already exists in the set`, else
/// `MULTI; SREM old; SADD new; EXEC`).
pub async fn set_replace(conn: &mut RedisConn, key: &str, old: &[u8], new: &[u8]) -> Result<()> {
    if set_is_member(conn, key, new).await? {
        return Err("The value already exists in the set".into());
    }
    let mut p = redis::pipe();
    p.atomic().cmd("SREM").arg(key).arg(old).cmd("SADD").arg(key).arg(new);
    p.query_async::<Value>(conn).await?;
    Ok(())
}

/// `SREM key member`.
pub async fn set_remove(conn: &mut RedisConn, key: &str, member: &[u8]) -> Result<()> {
    redis::cmd("SREM").arg(key).arg(member).exec_async(conn).await?;
    Ok(())
}

// ---- zset ------------------------------------------------------------------------------------

/// `(member, score_text)`.
pub type ZItem = (Vec<u8>, String);

/// Loads the next zset window: `ZRANGE` (or `ZREVRANGE` for `desc`) `key from to WITHSCORES`.
pub async fn zset_page(
    conn: &mut RedisConn,
    key: &str,
    loaded: u64,
    length: u64,
    desc: bool,
) -> Result<SeqPage<ZItem>> {
    let Some((from, to)) = next_window(loaded, length) else {
        return Ok(SeqPage::default());
    };
    let flat: Vec<Vec<u8>> = redis::cmd(if desc { "ZREVRANGE" } else { "ZRANGE" })
        .arg(key)
        .arg(from)
        .arg(to)
        .arg("WITHSCORES")
        .query_async(conn)
        .await?;
    let mut it = flat.into_iter();
    let mut items = Vec::new();
    while let (Some(m), Some(s)) = (it.next(), it.next()) {
        items.push((m, String::from_utf8_lossy(&s).into_owned()));
    }
    let diff = (to - from + 1) as i64 - items.len() as i64;
    Ok(SeqPage { items, diff })
}

/// `ZADD key score member` (score edit; no existence check).
pub async fn zset_set_score(conn: &mut RedisConn, key: &str, member: &[u8], score: &str) -> Result<()> {
    redis::cmd("ZADD").arg(key).arg(score).arg(member).exec_async(conn).await?;
    Ok(())
}

/// Adds a member; error `Member already exists` if `ZSCORE` is non-null.
pub async fn zset_add(conn: &mut RedisConn, key: &str, member: &[u8], score: &str) -> Result<()> {
    let existing: Option<String> = redis::cmd("ZSCORE").arg(key).arg(member).query_async(conn).await?;
    if existing.is_some() {
        return Err("Member already exists".into());
    }
    zset_set_score(conn, key, member, score).await
}

/// Renames a member: `MULTI; ZREM key old; ZADD key score new; EXEC`.
pub async fn zset_replace(
    conn: &mut RedisConn,
    key: &str,
    old: &[u8],
    new: &[u8],
    score: &str,
) -> Result<()> {
    let mut p = redis::pipe();
    p.atomic()
        .cmd("ZREM").arg(key).arg(old)
        .cmd("ZADD").arg(key).arg(score).arg(new);
    p.query_async::<Value>(conn).await?;
    Ok(())
}

/// `ZREM key member`.
pub async fn zset_remove(conn: &mut RedisConn, key: &str, member: &[u8]) -> Result<()> {
    redis::cmd("ZREM").arg(key).arg(member).exec_async(conn).await?;
    Ok(())
}

/// Index at which a new score belongs in the loaded rows (lodash `sortedIndexBy` lower bound of
/// `score * (desc ? -1 : 1)`). Unparseable scores sort as 0.
pub fn zset_insert_index(loaded_scores: &[&str], score: f64, desc: bool) -> usize {
    let sign = if desc { -1.0 } else { 1.0 };
    let target = score * sign;
    loaded_scores
        .iter()
        .position(|s| s.parse::<f64>().unwrap_or(0.0) * sign >= target)
        .unwrap_or(loaded_scores.len())
}

// ---- raw commands ----------------------------------------------------------------------------

/// Executes a raw command (terminal). Server errors become `Err` with the `CODE detail` text.
pub async fn exec(conn: &mut RedisConn, args: &[Vec<u8>]) -> Result<Value> {
    use redis::aio::ConnectionLike;
    let Some((name, rest)) = args.split_first() else {
        return Err("empty command".into());
    };
    let mut c = Cmd::new();
    c.arg(name.as_slice());
    for a in rest {
        c.arg(a.as_slice());
    }
    match conn.req_packed_command(&c).await {
        Ok(Value::ServerError(e)) => Err(redis_error_message(&redis::RedisError::from(e)).into()),
        Ok(v) => Ok(v),
        Err(e) => Err(e.into()),
    }
}

/// If `args` is a successful-looking `SELECT n`, returns `n`.
pub fn parse_select(args: &[Vec<u8>]) -> Option<u32> {
    match args {
        [c, n] if c.eq_ignore_ascii_case(b"select") => std::str::from_utf8(n).ok()?.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_parsing() {
        let text = "# Server\r\nredis_version:7.2.4\r\nuptime:5\r\n\r\n# Keyspace\r\ndb0:keys=12,expires=1,avg_ttl=0\r\ndb3:keys=1,expires=0\r\nweird:a:b\r\nempty:\r\n";
        let m = parse_info(text);
        assert_eq!(m["redis_version"], "7.2.4");
        assert_eq!(m["weird"], "a:b");
        assert!(!m.contains_key("empty"));
        assert_eq!(db_key_count(&m, 0), 12);
        assert_eq!(db_key_count(&m, 3), 1);
        assert_eq!(db_key_count(&m, 1), 0);
        assert_eq!(db_label(&m, 0), "0 (12)");
        assert_eq!(db_label(&m, 2), "2");
    }

    #[test]
    fn windows() {
        assert_eq!(next_window(0, 0), None);
        assert_eq!(next_window(0, 1), Some((0, 0)));
        assert_eq!(next_window(0, 1000), Some((0, 200)));
        assert_eq!(next_window(201, 1000), Some((201, 999)));
        assert_eq!(next_window(201, 5000), Some((201, 1201)));
        assert_eq!(next_window(1000, 1000), None);
        assert_eq!(list_redis_index(0, true), -1);
        assert_eq!(list_redis_index(3, false), 3);
    }

    #[test]
    fn insert_index() {
        assert_eq!(zset_insert_index(&["1", "2", "5"], 3.0, false), 2);
        assert_eq!(zset_insert_index(&["1", "2", "5"], 2.0, false), 1);
        assert_eq!(zset_insert_index(&["1", "2", "5"], 9.0, false), 3);
        assert_eq!(zset_insert_index(&["5", "2", "1"], 3.0, true), 1);
    }

    #[test]
    fn select_parse() {
        assert_eq!(parse_select(&[b"select".to_vec(), b"3".to_vec()]), Some(3));
        assert_eq!(parse_select(&[b"SELECT".to_vec(), b"x".to_vec()]), None);
        assert_eq!(parse_select(&[b"get".to_vec(), b"3".to_vec()]), None);
    }

    struct Lt(u32);
    impl DbSelector for Lt {
        fn select(&mut self, i: u32) -> futures::future::BoxFuture<'_, bool> {
            let ok = i < self.0;
            Box::pin(async move { ok })
        }
    }

    #[tokio::test]
    async fn db_guess() {
        // 20 databases: indexes 0..=19 valid
        let mut sel = Lt(20);
        assert_eq!(guess_db_number(&mut sel, 15, None).await, Some(19));
        // 16 databases
        let mut sel = Lt(16);
        assert_eq!(guess_db_number(&mut sel, 15, None).await, Some(15));
        // 4 databases: 15 fails -> None; then probe from 1
        let mut sel = Lt(4);
        assert_eq!(guess_db_number(&mut sel, 15, None).await, None);
        assert_eq!(guess_db_number(&mut sel, 1, Some(0)).await, Some(3));
        // 1 database
        let mut sel = Lt(1);
        assert_eq!(guess_db_number(&mut sel, 1, Some(0)).await, Some(0));
        // cap
        let mut sel = Lt(u32::MAX);
        assert_eq!(guess_db_number(&mut sel, 15, None).await, Some(30));
    }
}
