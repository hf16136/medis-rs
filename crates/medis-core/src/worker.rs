//! A per-connection background worker for UI use (egui or anything else).
//!
//! ```ignore
//! let (tx, mut rx) = Worker::spawn(cfg, Arc::new(move || ctx.request_repaint()));
//! tx.send(Req::new(next_req_id(), Op::Info))?;
//! // every frame:
//! while let Ok(evt) = rx.try_recv() { /* update state */ }
//! ```
//!
//! * [`Worker::spawn`] must be called from inside a tokio runtime context (e.g. after
//!   `rt.enter()` in `main`, or from within an async task).
//! * The worker connects first (emitting [`Evt::Status`], then [`Evt::Connected`] or
//!   [`Evt::ConnectFailed`]); requests sent in the meantime are queued.
//! * Every [`Req`] carries a [`ReqId`] chosen by the caller. Answers are [`Evt::Reply`] with the
//!   same id, except scans which stream [`Evt::ScanBatch`]es and finish with [`Evt::ScanDone`].
//! * Requests are processed in order (so `SelectDb` followed by `Scan` behaves), except
//!   [`Op::Scan`], [`Op::Info`] and [`Op::DbCount`], which run concurrently. A new scan, a
//!   `CancelScan` or a `SelectDb` aborts the running scan (it then emits nothing further).
//! * `notify` is called after every event is queued (use it for `ctx.request_repaint()`).
//! * Dropping the request sender (or sending [`Op::Disconnect`]) shuts the worker down: scan,
//!   pub/sub and monitor tasks are aborted, connections and the SSH tunnel are closed, and
//!   [`Evt::Closed`] is emitted.
//! * A command blocking on the server (BLPOP ...) blocks the worker's queue until the 30 s
//!   response timeout.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use redis::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::config::ConnectionConfig;
use crate::conn::{self, Connected, RedisConn};
use crate::error::{CoreError, Result};
use crate::ops::{self, HashPage, KeyKind, SeqPage, SetPage, ZItem};
use crate::scan::{self, KeyItem, ScanCursor, ScanRequest};

/// Caller-chosen request id echoed in the answer.
pub type ReqId = u64;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Returns a process-wide unique request id (convenience; ids may also be chosen by the caller).
pub fn next_req_id() -> ReqId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A request: an id plus the operation.
#[derive(Debug, Clone)]
pub struct Req {
    /// Echoed in [`Evt::Reply`] / [`Evt::ScanBatch`] / [`Evt::ScanDone`].
    pub id: ReqId,
    /// What to do.
    pub op: Op,
}

impl Req {
    /// Builds a request.
    pub fn new(id: ReqId, op: Op) -> Self {
        Self { id, op }
    }
}

/// Every operation the worker supports. Keys/fields/members are passed as text or bytes exactly
/// like the corresponding [`crate::ops`] function.
#[derive(Debug, Clone)]
pub enum Op {
    // ---- connection / server ----
    /// Close everything; the worker emits [`Evt::Closed`] and exits.
    Disconnect,
    /// `SELECT db` (cluster: only 0). Also aborts a running scan. → [`Reply::Unit`]
    SelectDb(u32),
    /// `INFO` parsed to a map. → [`Reply::Info`]
    Info,
    /// Number of databases (cached after the first call). → [`Reply::DbCount`]
    DbCount,

    // ---- key list ----
    /// Key SCAN (SPEC-1 §4.2), streaming [`Evt::ScanBatch`] and ending with [`Evt::ScanDone`].
    /// Pass `ScanCursor::default()` for a fresh scan, or the cursor of the last batch for
    /// "Scan more".
    Scan {
        /// Raw text of the pattern input.
        pattern: String,
        /// First run after a refresh.
        first_time: bool,
        /// The key list already has rows.
        have_keys: bool,
        /// Where to resume.
        cursor: ScanCursor,
    },
    /// Abort the running scan (no further events for it).
    CancelScan,
    /// `TYPE key`, then the length (STRLEN/LLEN/SCARD/ZCARD/HLEN). → [`Reply::KeyOpened`]
    OpenKey(String),
    /// `PTTL key`. → [`Reply::Int`]
    Pttl(String),
    /// `EXISTS key`. → [`Reply::Bool`]
    Exists(String),
    /// Create a key with a placeholder (error `The key already exists`). → [`Reply::Unit`]
    AddKey {
        /// Key name.
        key: String,
        /// Type.
        kind: KeyKind,
    },
    /// `RENAME old new`. → [`Reply::Unit`]
    Rename {
        /// Old name.
        old: String,
        /// New name.
        new: String,
    },
    /// `DEL key`. → [`Reply::Unit`]
    Delete(String),
    /// PEXPIRE (ms >= 0, error `Update Failed` on 0) / PERSIST (ms < 0). → [`Reply::Unit`]
    SetExpire {
        /// Key name.
        key: String,
        /// New PTTL in ms, or negative to persist.
        pttl_ms: i64,
    },
    /// Duplicate via the Lua script. → [`Reply::Unit`]
    Duplicate {
        /// Source key.
        source: String,
        /// Target key.
        target: String,
        /// Keep the TTL.
        keep_ttl: bool,
    },

    // ---- string ----
    /// `GET key`. → [`Reply::Bytes`]
    StringGet(String),
    /// setKeepTTL script. → [`Reply::Unit`]
    StringSet {
        /// Key.
        key: String,
        /// New value.
        value: Vec<u8>,
    },

    // ---- hash ----
    /// HSCAN page (`cursor` 0 for the first). → [`Reply::Hash`]
    HashPage {
        /// Key.
        key: String,
        /// Cursor.
        cursor: u64,
    },
    /// `HSET`. → [`Reply::Unit`]
    HashSet {
        /// Key.
        key: String,
        /// Field.
        field: String,
        /// Value.
        value: Vec<u8>,
    },
    /// HSETNX `'New Member'` (error `The field already exists`). → [`Reply::Unit`]
    HashAdd {
        /// Key.
        key: String,
        /// Field.
        field: String,
    },
    /// `HEXISTS`. → [`Reply::Bool`]
    HashExists {
        /// Key.
        key: String,
        /// Field.
        field: String,
    },
    /// MULTI HDEL src, HSET target value EXEC. → [`Reply::Unit`]
    HashRename {
        /// Key.
        key: String,
        /// Old field.
        src: String,
        /// New field.
        target: String,
        /// Value to store under the new field.
        value: Vec<u8>,
    },
    /// `HDEL`. → [`Reply::Unit`]
    HashDelete {
        /// Key.
        key: String,
        /// Field.
        field: String,
    },

    // ---- list ----
    /// Next LRANGE window. `loaded` = rows already loaded, `length` = current length.
    /// → [`Reply::List`]
    ListPage {
        /// Key.
        key: String,
        /// Rows loaded so far.
        loaded: u64,
        /// Current length.
        length: u64,
        /// Descending order.
        desc: bool,
    },
    /// `LSET key index value` (`index` = `ops::list_redis_index`). → [`Reply::Unit`]
    ListSet {
        /// Key.
        key: String,
        /// Redis index.
        index: i64,
        /// Value.
        value: Vec<u8>,
    },
    /// LPUSH/RPUSH `'New Item'`. → [`Reply::Unit`]
    ListPush {
        /// Key.
        key: String,
        /// Head (LPUSH) or tail (RPUSH).
        head: bool,
    },
    /// lremindex script. → [`Reply::Unit`]
    ListRemove {
        /// Key.
        key: String,
        /// Redis index.
        index: i64,
    },

    // ---- set ----
    /// SSCAN page. → [`Reply::Set`]
    SetPage {
        /// Key.
        key: String,
        /// Cursor.
        cursor: u64,
    },
    /// SISMEMBER + SADD (error `Member already exists`). → [`Reply::Unit`]
    SetAdd {
        /// Key.
        key: String,
        /// Member.
        member: Vec<u8>,
    },
    /// Replace a member (error `The value already exists in the set`). → [`Reply::Unit`]
    SetReplace {
        /// Key.
        key: String,
        /// Old member.
        old: Vec<u8>,
        /// New member.
        new: Vec<u8>,
    },
    /// `SREM`. → [`Reply::Unit`]
    SetRemove {
        /// Key.
        key: String,
        /// Member.
        member: Vec<u8>,
    },

    // ---- zset ----
    /// Next ZRANGE/ZREVRANGE WITHSCORES window. → [`Reply::ZSet`]
    ZSetPage {
        /// Key.
        key: String,
        /// Rows loaded so far.
        loaded: u64,
        /// Current length.
        length: u64,
        /// Descending order.
        desc: bool,
    },
    /// `ZADD key score member` (score edit). → [`Reply::Unit`]
    ZSetSetScore {
        /// Key.
        key: String,
        /// Member.
        member: Vec<u8>,
        /// Score text.
        score: String,
    },
    /// ZSCORE + ZADD (error `Member already exists`). → [`Reply::Unit`]
    ZSetAdd {
        /// Key.
        key: String,
        /// Member.
        member: Vec<u8>,
        /// Score text.
        score: String,
    },
    /// MULTI ZREM old, ZADD score new EXEC. → [`Reply::Unit`]
    ZSetReplace {
        /// Key.
        key: String,
        /// Old member.
        old: Vec<u8>,
        /// New member.
        new: Vec<u8>,
        /// Score text.
        score: String,
    },
    /// `ZREM`. → [`Reply::Unit`]
    ZSetRemove {
        /// Key.
        key: String,
        /// Member.
        member: Vec<u8>,
    },

    // ---- terminal ----
    /// Raw command. A server error comes back as `Err`. A successful `SELECT n` also emits
    /// [`Evt::DbSelected`]. → [`Reply::Value`]
    Exec(Vec<Vec<u8>>),
    /// Start (replace) a pub/sub stream on a dedicated connection: SUBSCRIBE channels and
    /// PSUBSCRIBE patterns; messages arrive as [`Evt::PubSub`]. → [`Reply::Unit`] once subscribed.
    PubSubStart {
        /// Exact channels.
        channels: Vec<String>,
        /// Glob patterns.
        patterns: Vec<String>,
    },
    /// Stop the pub/sub stream. → [`Reply::Unit`]
    PubSubStop,
    /// Start a MONITOR stream on a dedicated connection ([`Evt::Monitor`] lines). In cluster mode
    /// the first seed node is monitored. → [`Reply::Unit`]
    MonitorStart,
    /// Stop the monitor stream. → [`Reply::Unit`]
    MonitorStop,
}

/// Successful answers (see the doc of each [`Op`] variant).
#[derive(Debug, Clone)]
pub enum Reply {
    /// Nothing to return.
    Unit,
    /// A boolean.
    Bool(bool),
    /// An integer (PTTL).
    Int(i64),
    /// Optional bytes (GET).
    Bytes(Option<Vec<u8>>),
    /// Parsed INFO.
    Info(HashMap<String, String>),
    /// Number of databases.
    DbCount(u32),
    /// Result of [`Op::OpenKey`].
    KeyOpened {
        /// `string`/`list`/`hash`/`set`/`zset`/`none`/other.
        ktype: String,
        /// Length for the five supported types.
        len: Option<u64>,
    },
    /// HSCAN page.
    Hash(HashPage),
    /// LRANGE window.
    List(SeqPage<Vec<u8>>),
    /// SSCAN page.
    Set(SetPage),
    /// ZRANGE window.
    ZSet(SeqPage<ZItem>),
    /// Raw reply of [`Op::Exec`].
    Value(Value),
}

/// Which stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Pub/sub.
    PubSub,
    /// MONITOR.
    Monitor,
}

/// One pub/sub message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSubMsg {
    /// Channel it was published to.
    pub channel: String,
    /// The matching pattern (for PSUBSCRIBE).
    pub pattern: Option<String>,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

/// Summary of a successful connect.
#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    /// Instance title (`<name>/host:port`).
    pub title: String,
    /// `"<sshHost>|<host>|<port>"` (use with [`ConnectionConfig::pattern_key`] semantics).
    pub connection_key: String,
    /// `redis_version`.
    pub version: String,
    /// Cluster mode (db selector hidden).
    pub cluster: bool,
    /// The config with defaults applied.
    pub config: ConnectionConfig,
}

/// Events from the worker (drain with `try_recv`).
#[derive(Debug, Clone)]
pub enum Evt {
    /// `SSH connecting...` / `Redis connecting...`
    Status(String),
    /// Connected; requests are now being served.
    Connected(Box<ConnectedInfo>),
    /// Connect failed with the verbatim alert text; [`Evt::Closed`] follows.
    ConnectFailed(String),
    /// Answer to the request `id`.
    Reply {
        /// Request id.
        id: ReqId,
        /// Outcome; `Err(e).message` is the alert text.
        result: std::result::Result<Reply, CoreError>,
    },
    /// A batch of scanned keys for scan request `id` (append to the list; `cursor` is the
    /// resume position, `cursor.has_more()` tells whether a "Scan more" row is needed).
    ScanBatch {
        /// Scan request id.
        id: ReqId,
        /// New keys with their types.
        keys: Vec<KeyItem>,
        /// Position after this batch.
        cursor: ScanCursor,
    },
    /// The scan request `id` finished (`Ok(cursor)`) or failed.
    ScanDone {
        /// Scan request id.
        id: ReqId,
        /// Final cursor or error.
        result: std::result::Result<ScanCursor, CoreError>,
    },
    /// The connection's current db changed because of a terminal `SELECT n`.
    DbSelected(u32),
    /// A pub/sub message.
    PubSub(PubSubMsg),
    /// A MONITOR line.
    Monitor(String),
    /// A pub/sub or monitor stream ended (error text if it failed).
    StreamEnded {
        /// Which stream.
        kind: StreamKind,
        /// Failure, if any.
        error: Option<String>,
    },
    /// An operation failed because the connection dropped. Emitted once; show
    /// `Redis Error: Connection failed. (<message>)` and return to the connect form.
    ConnectionLost(String),
    /// The worker has shut down; no further events.
    Closed,
}

/// Namespace for [`Worker::spawn`].
pub struct Worker;

#[derive(Clone)]
struct Out {
    tx: UnboundedSender<Evt>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Out {
    fn send(&self, e: Evt) {
        let _ = self.tx.send(e);
        (self.notify)();
    }
}

impl Worker {
    /// Spawns the worker task for `cfg` on the current tokio runtime and starts connecting.
    ///
    /// Returns the request sender and the event receiver. `notify` is invoked after each event is
    /// queued (typically `ctx.request_repaint()`).
    pub fn spawn(
        cfg: ConnectionConfig,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> (UnboundedSender<Req>, UnboundedReceiver<Evt>) {
        let (req_tx, req_rx) = unbounded_channel();
        let (evt_tx, evt_rx) = unbounded_channel();
        let out = Out { tx: evt_tx, notify };
        tokio::spawn(run(cfg, req_rx, out));
        (req_tx, evt_rx)
    }
}

struct State {
    conn: RedisConn,
    client: redis::Client,
    out: Out,
    db_count: Option<u32>,
    scan: Option<AbortHandle>,
    pubsub: Option<AbortHandle>,
    monitor: Option<AbortHandle>,
    lost_reported: bool,
}

async fn run(cfg: ConnectionConfig, mut rx: UnboundedReceiver<Req>, out: Out) {
    let mut pending: VecDeque<Req> = VecDeque::new();
    let status_out = out.clone();
    let connect = conn::connect_with(&cfg, move |s| status_out.send(Evt::Status(s.to_string())));
    tokio::pin!(connect);

    let connected: Connected = loop {
        tokio::select! {
            r = &mut connect => match r {
                Ok(c) => break c,
                Err(msg) => {
                    out.send(Evt::ConnectFailed(msg));
                    out.send(Evt::Closed);
                    return;
                }
            },
            req = rx.recv() => match req {
                None => { out.send(Evt::Closed); return; }
                Some(Req { op: Op::Disconnect, .. }) => { out.send(Evt::Closed); return; }
                Some(r) => pending.push_back(r),
            },
        }
    };

    out.send(Evt::Connected(Box::new(ConnectedInfo {
        title: connected.title.clone(),
        connection_key: connected.connection_key.clone(),
        version: connected.version.clone(),
        cluster: connected.conn.is_cluster(),
        config: connected.config.clone(),
    })));

    let Connected { conn, client, tunnel, .. } = connected;
    let mut st = State {
        conn,
        client,
        out: out.clone(),
        db_count: None,
        scan: None,
        pubsub: None,
        monitor: None,
        lost_reported: false,
    };

    'outer: loop {
        let req = match pending.pop_front() {
            Some(r) => r,
            None => match rx.recv().await {
                Some(r) => r,
                None => break,
            },
        };
        if matches!(req.op, Op::Disconnect) {
            break 'outer;
        }
        handle(&mut st, req).await;
    }

    for h in [st.scan.take(), st.pubsub.take(), st.monitor.take()].into_iter().flatten() {
        h.abort();
    }
    drop(st);
    drop(tunnel);
    out.send(Evt::Closed);
}

fn abort(h: &mut Option<AbortHandle>) {
    if let Some(h) = h.take() {
        h.abort();
    }
}

fn reply(st: &mut State, id: ReqId, result: Result<Reply>) {
    if let Err(e) = &result
        && e.connection_lost
        && !st.lost_reported
    {
        st.lost_reported = true;
        st.out.send(Evt::ConnectionLost(e.message.clone()));
    }
    st.out.send(Evt::Reply { id, result });
}

async fn handle(st: &mut State, req: Req) {
    let Req { id, op } = req;
    match op {
        Op::Disconnect => {}
        Op::CancelScan => abort(&mut st.scan),
        Op::Scan { pattern, first_time, have_keys, cursor } => {
            abort(&mut st.scan);
            let mut conn = st.conn.clone();
            let out = st.out.clone();
            let h = tokio::spawn(async move {
                let mut cursor = cursor;
                let sr = ScanRequest { pattern: &pattern, first_time, have_keys };
                let res = scan::scan_with(&mut conn, &sr, &mut cursor, |keys, cur| {
                    out.send(Evt::ScanBatch { id, keys: keys.to_vec(), cursor: cur.clone() });
                    true
                })
                .await;
                match res {
                    Ok(_) => out.send(Evt::ScanDone { id, result: Ok(cursor) }),
                    Err(e) => {
                        if e.connection_lost {
                            out.send(Evt::ConnectionLost(e.message.clone()));
                        }
                        out.send(Evt::ScanDone { id, result: Err(e) });
                    }
                }
            });
            st.scan = Some(h.abort_handle());
        }
        Op::Info => {
            let mut conn = st.conn.clone();
            let out = st.out.clone();
            tokio::spawn(async move {
                let r = ops::info(&mut conn).await.map(Reply::Info);
                out.send(Evt::Reply { id, result: r });
            });
        }
        Op::DbCount => {
            if let Some(n) = st.db_count {
                reply(st, id, Ok(Reply::DbCount(n)));
            } else {
                // Not spawned: result is cached in `st` and the probe is a one-time cost.
                let n = ops::db_count(&mut st.conn, &st.client).await;
                st.db_count = Some(n);
                reply(st, id, Ok(Reply::DbCount(n)));
            }
        }
        Op::SelectDb(db) => {
            abort(&mut st.scan);
            let r = ops::select_db(&mut st.conn, db).await.map(|_| Reply::Unit);
            reply(st, id, r);
        }
        Op::Exec(args) => {
            let r = ops::exec(&mut st.conn, &args).await;
            if let (Ok(v), Some(db)) = (&r, ops::parse_select(&args))
                && matches!(v, Value::Okay | Value::SimpleString(_))
            {
                abort(&mut st.scan);
                st.out.send(Evt::DbSelected(db));
            }
            reply(st, id, r.map(Reply::Value));
        }
        Op::PubSubStart { channels, patterns } => {
            abort(&mut st.pubsub);
            let r = start_pubsub(st, channels, patterns).await.map(|_| Reply::Unit);
            reply(st, id, r);
        }
        Op::PubSubStop => {
            abort(&mut st.pubsub);
            reply(st, id, Ok(Reply::Unit));
        }
        Op::MonitorStart => {
            abort(&mut st.monitor);
            let r = start_monitor(st).await.map(|_| Reply::Unit);
            reply(st, id, r);
        }
        Op::MonitorStop => {
            abort(&mut st.monitor);
            reply(st, id, Ok(Reply::Unit));
        }
        other => {
            let r = simple(&mut st.conn, other).await;
            reply(st, id, r);
        }
    }
}

async fn start_pubsub(st: &mut State, channels: Vec<String>, patterns: Vec<String>) -> Result<()> {
    if channels.is_empty() && patterns.is_empty() {
        return Err("No channel to subscribe".into());
    }
    let mut ps = st.client.get_async_pubsub().await?;
    for c in &channels {
        ps.subscribe(c).await?;
    }
    for p in &patterns {
        ps.psubscribe(p).await?;
    }
    let out = st.out.clone();
    let h = tokio::spawn(async move {
        let mut stream = ps.into_on_message();
        while let Some(m) = stream.next().await {
            out.send(Evt::PubSub(PubSubMsg {
                channel: m.get_channel_name().to_string(),
                pattern: m.get_pattern::<String>().ok().filter(|_| m.from_pattern()),
                payload: m.get_payload_bytes().to_vec(),
            }));
        }
        out.send(Evt::StreamEnded { kind: StreamKind::PubSub, error: None });
    });
    st.pubsub = Some(h.abort_handle());
    Ok(())
}

async fn start_monitor(st: &mut State) -> Result<()> {
    let m = st.client.get_async_monitor().await?;
    let out = st.out.clone();
    let h = tokio::spawn(async move {
        let mut stream = m.into_on_message::<String>();
        while let Some(line) = stream.next().await {
            out.send(Evt::Monitor(line));
        }
        out.send(Evt::StreamEnded { kind: StreamKind::Monitor, error: None });
    });
    st.monitor = Some(h.abort_handle());
    Ok(())
}

/// The request/response operations that map 1:1 onto [`crate::ops`].
async fn simple(c: &mut RedisConn, op: Op) -> Result<Reply> {
    use Op::*;
    Ok(match op {
        OpenKey(key) => {
            let ktype = ops::key_type(c, &key).await?;
            let len = match ktype.as_str() {
                "string" | "list" | "set" | "zset" | "hash" => Some(ops::content_len(c, &key, &ktype).await?),
                _ => None,
            };
            Reply::KeyOpened { ktype, len }
        }
        Pttl(k) => Reply::Int(ops::pttl(c, &k).await?),
        Exists(k) => Reply::Bool(ops::exists(c, &k).await?),
        AddKey { key, kind } => unit(ops::add_key(c, &key, kind).await)?,
        Rename { old, new } => unit(ops::rename(c, &old, &new).await)?,
        Delete(k) => unit(ops::delete(c, &k).await)?,
        SetExpire { key, pttl_ms } => unit(ops::set_expire(c, &key, pttl_ms).await)?,
        Duplicate { source, target, keep_ttl } => unit(ops::duplicate(c, &source, &target, keep_ttl).await)?,
        StringGet(k) => Reply::Bytes(ops::string_get(c, &k).await?),
        StringSet { key, value } => unit(ops::string_set(c, &key, &value).await)?,
        HashPage { key, cursor } => Reply::Hash(ops::hash_page(c, &key, cursor).await?),
        HashSet { key, field, value } => unit(ops::hash_set(c, &key, &field, &value).await)?,
        HashAdd { key, field } => unit(ops::hash_add(c, &key, &field).await)?,
        HashExists { key, field } => Reply::Bool(ops::hash_exists(c, &key, &field).await?),
        HashRename { key, src, target, value } => {
            unit(ops::hash_rename(c, &key, &src, &target, &value).await)?
        }
        HashDelete { key, field } => unit(ops::hash_delete(c, &key, &field).await)?,
        ListPage { key, loaded, length, desc } => {
            Reply::List(ops::list_page(c, &key, loaded, length, desc).await?)
        }
        ListSet { key, index, value } => unit(ops::list_set(c, &key, index, &value).await)?,
        ListPush { key, head } => unit(ops::list_push(c, &key, head).await)?,
        ListRemove { key, index } => unit(ops::list_remove(c, &key, index).await)?,
        SetPage { key, cursor } => Reply::Set(ops::set_page(c, &key, cursor).await?),
        SetAdd { key, member } => unit(ops::set_add(c, &key, &member).await)?,
        SetReplace { key, old, new } => unit(ops::set_replace(c, &key, &old, &new).await)?,
        SetRemove { key, member } => unit(ops::set_remove(c, &key, &member).await)?,
        ZSetPage { key, loaded, length, desc } => {
            Reply::ZSet(ops::zset_page(c, &key, loaded, length, desc).await?)
        }
        ZSetSetScore { key, member, score } => unit(ops::zset_set_score(c, &key, &member, &score).await)?,
        ZSetAdd { key, member, score } => unit(ops::zset_add(c, &key, &member, &score).await)?,
        ZSetReplace { key, old, new, score } => {
            unit(ops::zset_replace(c, &key, &old, &new, &score).await)?
        }
        ZSetRemove { key, member } => unit(ops::zset_remove(c, &key, &member).await)?,
        // handled before reaching here
        Disconnect | CancelScan | Scan { .. } | Info | DbCount | SelectDb(_) | Exec(_)
        | PubSubStart { .. } | PubSubStop | MonitorStart | MonitorStop => Reply::Unit,
    })
}

fn unit(r: Result<()>) -> Result<Reply> {
    r.map(|_| Reply::Unit)
}
