# medis-core public API (as built by lane 1)

All items are under `medis_core::…`; the crate also re-exports `redis`. Call `install_crypto_provider()` once at startup.

## config.rs — ConnectionConfig
- Serialises to the original localStorage camelCase names (`sshHost`, `tlskeyFile`, ...). Missing fields take defaults.
- **Fields:**
  - `key, name("New Favorite"), host, port, password, username`
  - TLS: `ssl: bool, tls_key, tls_cert, tls_ca` (PEM contents), `tls_key_file, tls_cert_file, tls_ca_file` (paths)
  - SSH: `ssh: bool, ssh_host, ssh_user, ssh_password, ssh_key: Option<String>, ssh_key_file: Option<String>, ssh_key_passphrase, ssh_port`
  - `tls: TlsConfig{enabled, ca_path, cert_path, key_path, insecure(default true), skip_hostname}`
  - `cluster: ClusterConfig{enabled, nodes: Vec<String>}`
- **Methods:**
  - Constructors: `new_favorite()`, `quick()`.
  - `with_defaults()` fills in localhost, 6379 and 22.
  - `connection_key()` returns `"<sshHost>|<host>|<port>"`; `pattern_key(db)` returns `"<connectionKey>|<db>"`.
  - Also: `title()`, `tls_enabled()`, `ssh_key_is_encrypted()`.

## storage.rs — Storage
- `Storage::new(dir)` or `open_default()` (uses `config_dir()/medis`).
- Favorites: `load_favorites`, `create_favorite`, `remove_favorite(key)`, `update_favorite(&cfg)`, `reorder_favorite(from,to)`.
- Patterns: `load_patterns`, `patterns_for(pattern_key)`, `create_pattern`, `remove_pattern(pk, idx)`, `update_pattern(pk, idx, name, value)`.
- Sizes: `load_sizes` and `save_sizes`, with `Sizes{content_bar_width 200, index_bar_width 60, score_bar_width 60}`.
- `stamp()`: poll it to detect writes made by other windows.

## conn.rs
- `connect(&cfg)` and `connect_with(&cfg, |status|..)` return `Connected { conn: RedisConn, config, title, connection_key, version, client, tunnel }`.
- `RedisConn::{Single, Cluster}` is Clone and implements ConnectionLike.
- Verbatim error strings are produced here, so the UI shows them as returned.

## scan.rs
- `KeyItem{key, ktype}`.
- `ScanCursor` provides `has_more()` (show the "Scan more" row), `display()` and `is_finished()`.

## ops.rs (async; the UI normally goes through the Worker)
- Helpers: `parse_info`, `db_label`, `list_redis_index(row, desc)`, `zset_insert_index(scores, score, desc)`, `parse_select`.
- Types: `HashPage{items:(String,Vec<u8>), cursor}`, `SeqPage{items, diff}`, `SetPage`, `ZItem=(Vec<u8>, String)`.
- Paging and placeholder constants (`New Item`, `New Member`, and the 500/10000 and 200/1000 windows) live in ops.rs.

## editor.rs
- raw/json/messagepack detection and encoding (SPEC-2 §5.7). The UI must use it and not reimplement it.

## worker.rs — the only thing the UI talks to
- `Worker::spawn(cfg, notify: Arc<dyn Fn()+Send+Sync>) -> (UnboundedSender<Req>, UnboundedReceiver<Evt>)`.
  - Call it inside a tokio runtime context (`rt.enter()`).
  - `notify` should call `ctx.request_repaint()`.
- `Req{id: ReqId(u64), op: Op}`; get ids from `next_req_id()`.
- **Ordering:** ops run in order, except Scan, Info and DbCount, which run concurrently. A new Scan, CancelScan or SelectDb aborts the running scan.
- **Op:**
  - Connection and server: `Disconnect`, `SelectDb(u32)`, `Info`, `DbCount`
  - Scan: `Scan{pattern, first_time, have_keys, cursor}`, `CancelScan`
  - Keys: `OpenKey(key)`, `Pttl`, `Exists`, `AddKey{key,kind}`, `Rename{old,new}`, `Delete`, `SetExpire{key,pttl_ms}` (negative = persist), `Duplicate{source,target,keep_ttl}`
  - String: `StringGet`, `StringSet`
  - Hash: `HashPage`, `HashSet`, `HashAdd`, `HashExists`, `HashRename`, `HashDelete`
  - List: `ListPage`, `ListSet`, `ListPush`, `ListRemove`
  - Set: `SetPage`, `SetAdd`, `SetReplace`, `SetRemove`
  - ZSet: `ZSetPage`, `ZSetSetScore`, `ZSetAdd`, `ZSetReplace`, `ZSetRemove`
  - Terminal: `Exec(Vec<Vec<u8>>)`, `PubSubStart{channels,patterns}`, `PubSubStop`, `MonitorStart`, `MonitorStop`
- **Evt:**
  - Connect lifecycle: `Status(String)`, `Connected(Box<ConnectedInfo{title, connection_key, version, cluster, config}>)`, `ConnectFailed(String)` followed by `Closed`
  - Replies: `Reply{id, result: Result<Reply, CoreError>}`, `ScanBatch{id, keys, cursor}`, `ScanDone{id, result}`
  - Terminal: `DbSelected(u32)`, `PubSub(PubSubMsg{channel, pattern, payload})`, `Monitor(String)`, `StreamEnded{kind, error}`
  - Connection state: `ConnectionLost(String)` (emitted once), `Closed`
- **Reply:** `Unit`, `Bool`, `Int`, `Bytes(Option<Vec<u8>>)`, `Info(HashMap)`, `DbCount(u32)`, `KeyOpened{ktype, len}`, `Hash(HashPage)`, `List(SeqPage<Vec<u8>>)`, `Set(SetPage)`, `ZSet(SeqPage<ZItem>)`, `Value(redis::Value)`.
- `CoreError{message, connection_lost}`.
- The exact field names of each Op variant are in worker.rs; read that file.

## Cluster limits
- db 0 only; `DbCount` returns 1.
- Pub/sub and MONITOR use the first seed node.
- Blocking terminal commands hold the queue until the 30 s timeout.

## format.rs / cmdline.rs (lane 2 contract; the UI may code against these signatures)
```rust
// cmdline.rs
pub fn split_args(line: &str) -> Result<Vec<Vec<u8>>, SplitError>; // sdssplitargs semantics; SplitError: Display ("Invalid argument(s)")
pub const COMMANDS: &[&str];                                       // known redis command names, uppercase
pub fn complete(line: &str) -> Option<String>;                     // first-token completion with SPEC-3 case rule; returns full new line
pub fn needs_confirm(args: &[Vec<u8>]) -> Option<String>;          // FLUSHALL/FLUSHDB prompt text (SPEC-3), else None
pub enum Special { Monitor, Subscribe{channels: Vec<String>}, PSubscribe{patterns: Vec<String>}, Select(u32), None }
pub fn classify(args: &[Vec<u8>]) -> Special;

// format.rs
pub enum Tone { Plain, String, Integer, Nil, Error, Status, Index, Dim }
pub struct Span { pub text: String, pub tone: Tone }
pub type Line = Vec<Span>;
pub fn render_reply(v: &redis::Value) -> Vec<Line>;          // SPEC-3 rendering (0-based indices, no redis-cli quoting decorations)
pub fn render_error(msg: &str) -> Vec<Line>;
pub fn format_ttl(pttl_ms: i64) -> String;                    // content footer human TTL (SPEC-3)
pub fn format_monitor(line: &str) -> Line;
pub fn format_pubsub(msg: &medis_core::worker::PubSubMsg) -> Line;
pub fn banner_monitor() -> String; pub fn banner_subscribe(..) -> String; // verbatim SPEC-3 banners
pub fn display_bytes(b: &[u8]) -> String;                     // lossless-ish display for binary-safe values
```
