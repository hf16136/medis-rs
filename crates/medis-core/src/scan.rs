//! The SPEC-1 §4.2 key `SCAN` algorithm.
//!
//! The pure pieces ([`plan`], [`next_count`], [`should_continue`], [`ScanCursor`]) are unit
//! tested; [`scan_with`] drives them against a [`RedisConn`].
//!
//! Cluster mode: `SCAN` is not routed automatically, so the primaries are discovered with
//! `CLUSTER NODES` and visited one after another (`route_command` + `ByAddress`); the
//! [`ScanCursor`] remembers the node index and the per-node cursor. `TYPE` is issued per key
//! (concurrently) in cluster mode because a pipeline may not span slots; in standalone mode it is
//! one pipeline per batch.

use redis::cluster_routing::{RoutingInfo, SingleNodeRoutingInfo};

use crate::conn::RedisConn;
use crate::error::{CoreError, Result};

/// A scanned key and its `TYPE` (`string`, `list`, `hash`, `zset`, `set`, `stream`, ..., `none`).
///
/// Keys are decoded lossily as UTF-8 (the original did the same).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyItem {
    /// Key name.
    pub key: String,
    /// Redis `TYPE` reply.
    pub ktype: String,
}

/// Resumable scan position. `Default` = "not started" (no "scan more" row).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanCursor {
    cursor: u64,
    node: usize,
    nodes: Vec<(String, u16)>,
    started: bool,
    finished: bool,
}

impl ScanCursor {
    /// A fresh cursor (same as `Default`).
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when a scan was run and the keyspace has not been exhausted yet
    /// (the UI shows the `Scan more` row).
    pub fn has_more(&self) -> bool {
        self.started && !self.finished
    }

    /// Text for `Scanning...(cursor <c>)`. In cluster mode `<node>/<nodes>:<cursor>`.
    pub fn display(&self) -> String {
        if self.nodes.len() > 1 {
            format!("{}/{}:{}", self.node + 1, self.nodes.len(), self.cursor)
        } else {
            self.cursor.to_string()
        }
    }

    /// The raw cursor of the current node.
    pub fn raw(&self) -> u64 {
        self.cursor
    }

    /// Records the cursor returned by `SCAN` for the current node. Returns `true` when the whole
    /// keyspace has now been covered.
    pub fn advance(&mut self, new_cursor: u64) -> bool {
        self.started = true;
        if new_cursor != 0 {
            self.cursor = new_cursor;
            return false;
        }
        self.cursor = 0;
        if self.node + 1 < self.nodes.len() {
            self.node += 1;
            false
        } else {
            self.finished = true;
            true
        }
    }

    /// `true` when exhausted.
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

/// What to scan, derived from the user's pattern input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPlan {
    /// The `MATCH` pattern (`*` when the input is empty, `input*` for plain keys).
    pub pattern: String,
    /// For plain keys (no `*`/`?`): the exact key, which is listed first and filtered from SCAN.
    pub filter_key: Option<String>,
}

/// Computes the [`ScanPlan`] for the pattern input box.
pub fn plan(input: &str) -> ScanPlan {
    if input.is_empty() {
        return ScanPlan {
            pattern: "*".into(),
            filter_key: None,
        };
    }
    if !input.contains('*') && !input.contains('?') {
        ScanPlan {
            pattern: format!("{input}*"),
            filter_key: Some(input.to_string()),
        }
    } else {
        ScanPlan {
            pattern: input.to_string(),
            filter_key: None,
        }
    }
}

/// The `COUNT` for the next iteration: 5000 below 10 keys found, 2000 below 50, else 1000.
pub fn next_count(total: usize) -> u32 {
    if total < 10 {
        5000
    } else if total < 50 {
        2000
    } else {
        1000
    }
}

/// Whether the scan loop continues after an iteration.
///
/// Stops when the exact key exists on the first run, the keyspace is exhausted, the total
/// reaches 100, or `total > 0 && times > 200`.
pub fn should_continue(
    filter_exists_first_run: bool,
    exhausted: bool,
    total: usize,
    times: u32,
) -> bool {
    !(filter_exists_first_run || exhausted || total >= 100 || (total > 0 && times > 200))
}

/// Inputs of [`scan_with`].
#[derive(Debug, Clone)]
pub struct ScanRequest<'a> {
    /// The raw text of the pattern input.
    pub pattern: &'a str,
    /// First run after a refresh (enables the "exact key exists" shortcut).
    pub first_time: bool,
    /// The key list already contains rows (skips the `TYPE input` probe for plain keys).
    pub have_keys: bool,
}

/// Runs the scan loop, calling `on_batch(new_keys, cursor)` after each iteration (and once for
/// the exact-match probe). Return `false` from the callback to cancel; the function then returns
/// `Ok(false)`. Returns `Ok(true)` when the loop ended normally. `cursor` is updated in place and
/// can be reused for "scan more".
pub async fn scan_with<F>(
    conn: &mut RedisConn,
    req: &ScanRequest<'_>,
    cursor: &mut ScanCursor,
    mut on_batch: F,
) -> Result<bool>
where
    F: FnMut(&[KeyItem], &ScanCursor) -> bool,
{
    let plan = plan(req.pattern);
    let mut filter_exists = false;
    let mut first_count = 100u32;

    if let Some(fk) = &plan.filter_key
        && !req.have_keys
    {
        let t = key_type(conn, fk).await?;
        if t != "none" {
            filter_exists = true;
            let item = KeyItem {
                key: fk.clone(),
                ktype: t,
            };
            if !on_batch(&[item], cursor) {
                return Ok(false);
            }
            if req.first_time {
                first_count = 1;
            }
        }
    }

    if conn.is_cluster() && cursor.nodes.is_empty() {
        cursor.nodes = cluster_primaries(conn).await?;
        cursor.node = 0;
        cursor.cursor = 0;
    }

    let mut count = 0usize;
    let mut times = 1u32;
    let mut fetch = first_count;
    loop {
        let (new_cursor, mut keys) = scan_once(conn, cursor, &plan.pattern, fetch).await?;
        if let Some(fk) = &plan.filter_key {
            keys.retain(|k| k.as_slice() != fk.as_bytes());
        }
        let keys: Vec<String> = keys
            .into_iter()
            .map(|k| String::from_utf8_lossy(&k).into_owned())
            .collect();
        count += keys.len();
        let types = key_types(conn, &keys).await?;
        let items: Vec<KeyItem> = keys
            .into_iter()
            .zip(types)
            .map(|(key, ktype)| KeyItem { key, ktype })
            .collect();

        let exhausted = cursor.advance(new_cursor);
        let go = should_continue(filter_exists && req.first_time, exhausted, count, times);
        if !on_batch(&items, cursor) {
            return Ok(false);
        }
        if !go {
            return Ok(true);
        }
        fetch = next_count(count);
        times += 1;
    }
}

/// Convenience wrapper collecting every batch into one `Vec`.
pub async fn scan_collect(
    conn: &mut RedisConn,
    req: &ScanRequest<'_>,
    cursor: &mut ScanCursor,
) -> Result<Vec<KeyItem>> {
    let mut all = Vec::new();
    scan_with(conn, req, cursor, |b, _| {
        all.extend_from_slice(b);
        true
    })
    .await?;
    Ok(all)
}

async fn scan_once(
    conn: &mut RedisConn,
    cursor: &ScanCursor,
    pattern: &str,
    count: u32,
) -> Result<(u64, Vec<Vec<u8>>)> {
    let mut c = redis::cmd("SCAN");
    c.arg(cursor.cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count);
    match conn {
        RedisConn::Single(m) => Ok(c.query_async(m).await?),
        RedisConn::Cluster(cl) => {
            let (host, port) = cursor
                .nodes
                .get(cursor.node)
                .cloned()
                .ok_or_else(|| CoreError::msg("cluster: no primary nodes"))?;
            let v = cl
                .route_command(
                    c,
                    RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress { host, port }),
                )
                .await?;
            Ok(redis::from_redis_value(v.extract_error()?)?)
        }
    }
}

/// `TYPE key` as a lowercase string (`none` when missing).
pub async fn key_type(conn: &mut RedisConn, key: &str) -> Result<String> {
    Ok(redis::cmd("TYPE").arg(key).query_async(conn).await?)
}

/// `TYPE` for many keys: one pipeline (standalone) or concurrent commands (cluster).
pub async fn key_types(conn: &mut RedisConn, keys: &[String]) -> Result<Vec<String>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    match conn {
        RedisConn::Single(m) => {
            let mut p = redis::pipe();
            for k in keys {
                p.cmd("TYPE").arg(k);
            }
            Ok(p.query_async(m).await?)
        }
        RedisConn::Cluster(_) => {
            let futs = keys.iter().map(|k| {
                let mut c = conn.clone();
                let k = k.clone();
                async move { key_type(&mut c, &k).await }
            });
            futures::future::try_join_all(futs).await
        }
    }
}

/// `PTTL` for many keys (`-1` persistent, `-2` missing): pipeline / concurrent like [`key_types`].
pub async fn key_ttls(conn: &mut RedisConn, keys: &[String]) -> Result<Vec<i64>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    match conn {
        RedisConn::Single(m) => {
            let mut p = redis::pipe();
            for k in keys {
                p.cmd("PTTL").arg(k);
            }
            Ok(p.query_async(m).await?)
        }
        RedisConn::Cluster(_) => {
            let futs = keys.iter().map(|k| {
                let mut c = conn.clone();
                let k = k.clone();
                async move {
                    redis::cmd("PTTL")
                        .arg(k)
                        .query_async::<i64>(&mut c)
                        .await
                        .map_err(CoreError::from)
                }
            });
            futures::future::try_join_all(futs).await
        }
    }
}

/// Lists the primaries (`host`, `port`) of a cluster via `CLUSTER NODES`.
pub async fn cluster_primaries(conn: &mut RedisConn) -> Result<Vec<(String, u16)>> {
    let RedisConn::Cluster(cl) = conn else {
        return Ok(Vec::new());
    };
    let v = cl
        .route_command(
            redis::cmd("CLUSTER").arg("NODES").clone(),
            RoutingInfo::SingleNode(SingleNodeRoutingInfo::Random),
        )
        .await?;
    let text: String = redis::from_redis_value(v.extract_error()?)?;
    let nodes = parse_cluster_nodes(&text);
    if nodes.is_empty() {
        return Err(CoreError::msg("cluster: no primary nodes found"));
    }
    Ok(nodes)
}

/// Parses `CLUSTER NODES` output into the list of live primaries.
pub fn parse_cluster_nodes(text: &str) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let flags: Vec<&str> = f[2].split(',').collect();
        if !flags.contains(&"master") || flags.contains(&"fail") || flags.contains(&"handshake") || flags.contains(&"noaddr") {
            continue;
        }
        // ip:port@cport[,hostname]
        let addr = f[1].split(',').next().unwrap_or("");
        let addr = addr.split('@').next().unwrap_or("");
        if let Some((host, port)) = addr.rsplit_once(':')
            && let Ok(port) = port.parse::<u16>()
        {
            let host = if host.is_empty() { "127.0.0.1" } else { host };
            out.push((host.to_string(), port));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_cases() {
        assert_eq!(plan("").pattern, "*");
        assert_eq!(plan("").filter_key, None);
        let p = plan("user:1");
        assert_eq!(p.pattern, "user:1*");
        assert_eq!(p.filter_key.as_deref(), Some("user:1"));
        let p = plan("user:*");
        assert_eq!(p.pattern, "user:*");
        assert_eq!(p.filter_key, None);
        assert_eq!(plan("a?c").filter_key, None);
    }

    #[test]
    fn counts() {
        assert_eq!(next_count(0), 5000);
        assert_eq!(next_count(9), 5000);
        assert_eq!(next_count(10), 2000);
        assert_eq!(next_count(49), 2000);
        assert_eq!(next_count(50), 1000);
        assert_eq!(next_count(5000), 1000);
    }

    #[test]
    fn continuation() {
        assert!(should_continue(false, false, 0, 1));
        assert!(!should_continue(true, false, 0, 1)); // exact key on first run
        assert!(!should_continue(false, true, 0, 1)); // cursor 0
        assert!(!should_continue(false, false, 100, 1));
        assert!(should_continue(false, false, 99, 1));
        assert!(should_continue(false, false, 0, 500)); // nothing found yet: keep going
        assert!(!should_continue(false, false, 1, 201));
        assert!(should_continue(false, false, 1, 200));
    }

    #[test]
    fn cursor_single() {
        let mut c = ScanCursor::new();
        assert!(!c.has_more());
        assert!(!c.advance(17));
        assert!(c.has_more());
        assert_eq!(c.raw(), 17);
        assert_eq!(c.display(), "17");
        assert!(c.advance(0));
        assert!(!c.has_more());
        assert!(c.is_finished());
    }

    #[test]
    fn cursor_cluster() {
        let mut c = ScanCursor {
            nodes: vec![("a".into(), 1), ("b".into(), 2)],
            ..Default::default()
        };
        assert!(!c.advance(5));
        assert_eq!(c.display(), "1/2:5");
        assert!(!c.advance(0)); // moves to node 2
        assert_eq!(c.node, 1);
        assert_eq!(c.raw(), 0);
        assert!(c.has_more());
        assert!(c.advance(0));
        assert!(!c.has_more());
    }

    #[test]
    fn cluster_nodes_parse() {
        let t = "\
07c37dfeb235213a872192d90877d0cd55635b91 127.0.0.1:30004@31004,host4 slave e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 0 1426238317239 4 connected
67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 127.0.0.1:30002@31002 master - 0 1426238316232 2 connected 5461-10922
292f8b365bb7edb5e285caf0b7e6ddc7265d2f4f :30003@31003 myself,master - 0 0 3 connected 10923-16383
deadbeef 10.0.0.1:7000@17000 master,fail - 0 0 3 disconnected
";
        assert_eq!(
            parse_cluster_nodes(t),
            vec![("127.0.0.1".to_string(), 30002), ("127.0.0.1".to_string(), 30003)]
        );
    }
}
