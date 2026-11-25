//! Terminal command-line helpers (SPEC-3 §6): `sdssplitargs`-compatible tokenising, first-token
//! completion, the FLUSHALL/FLUSHDB confirmation and classification of commands that switch the
//! terminal into a special mode (MONITOR, SUBSCRIBE, SELECT).

use std::fmt;

/// Prompt shown before every input line.
pub const PROMPT: &str = "redis> ";

/// Text of the nested confirmation prompt (FLUSHALL / FLUSHDB); same as [`needs_confirm`] returns.
pub const CONFIRM_PROMPT: &str = "Are you sure (y/n)? ";

/// Error when the line cannot be tokenised (unbalanced quotes, or a closing quote that is not
/// followed by whitespace). Its `Display` is the exact text to print: `Invalid argument(s)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitError;

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid argument(s)")
    }
}

impl std::error::Error for SplitError {}

/// C `isspace` in the "C" locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Splits a command line with the exact semantics of Redis' `sdssplitargs`:
///
/// * arguments are separated by whitespace; leading/trailing whitespace is ignored and an
///   all-whitespace line yields an empty vector;
/// * inside `"..."`: `\xHH`, `\n`, `\r`, `\t`, `\b`, `\a` are decoded, any other `\c` yields `c`
///   (so `\\` and `\"` work);
/// * inside `'...'`: only `\'` is an escape, everything else is literal;
/// * a closing quote must be followed by whitespace or the end of the line;
/// * unterminated quotes are an error;
/// * quotes may start in the middle of a token (`ab"c d"` is the single argument `abc d`);
/// * like the C original, the input ends at the first NUL byte.
///
/// Arguments are bytes (`\xHH` can produce non-UTF-8).
pub fn split_args(line: &str) -> Result<Vec<Vec<u8>>, SplitError> {
    let bytes = line.as_bytes();
    let bytes = match bytes.iter().position(|&b| b == 0) {
        Some(n) => &bytes[..n],
        None => bytes,
    };
    let at = |i: usize| bytes.get(i).copied();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        while at(i).is_some_and(is_space) {
            i += 1;
        }
        if i >= bytes.len() {
            return Ok(out);
        }
        let mut cur: Vec<u8> = Vec::new();
        let (mut inq, mut insq) = (false, false);
        loop {
            let c = at(i);
            if inq {
                match c {
                    Some(b'\\')
                        if at(i + 1) == Some(b'x')
                            && at(i + 2).and_then(hex_val).is_some()
                            && at(i + 3).and_then(hex_val).is_some() =>
                    {
                        let hi = at(i + 2).and_then(hex_val).unwrap_or(0);
                        let lo = at(i + 3).and_then(hex_val).unwrap_or(0);
                        cur.push(hi << 4 | lo);
                        i += 3;
                    }
                    Some(b'\\') if at(i + 1).is_some() => {
                        i += 1;
                        cur.push(match bytes[i] {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            b'b' => 0x08,
                            b'a' => 0x07,
                            other => other,
                        });
                    }
                    Some(b'"') => {
                        if at(i + 1).is_some_and(|n| !is_space(n)) {
                            return Err(SplitError);
                        }
                        out.push(std::mem::take(&mut cur));
                        i += 1;
                        break;
                    }
                    None => return Err(SplitError),
                    Some(b) => cur.push(b),
                }
            } else if insq {
                match c {
                    Some(b'\\') if at(i + 1) == Some(b'\'') => {
                        i += 1;
                        cur.push(b'\'');
                    }
                    Some(b'\'') => {
                        if at(i + 1).is_some_and(|n| !is_space(n)) {
                            return Err(SplitError);
                        }
                        out.push(std::mem::take(&mut cur));
                        i += 1;
                        break;
                    }
                    None => return Err(SplitError),
                    Some(b) => cur.push(b),
                }
            } else {
                match c {
                    None | Some(b' ' | b'\n' | b'\r' | b'\t') => {
                        out.push(std::mem::take(&mut cur));
                        i += 1;
                        break;
                    }
                    Some(b'"') => inq = true,
                    Some(b'\'') => insq = true,
                    Some(b) => cur.push(b),
                }
            }
            i += 1;
        }
    }
}

/// Known Redis command names (upper case, sorted, no duplicates). Multi-word commands appear as
/// their first word (`CLIENT`, `CONFIG`, `XGROUP`, ...).
pub const COMMANDS: &[&str] = &[
    "ACL", "APPEND", "ASKING", "AUTH", "BGREWRITEAOF", "BGSAVE", "BITCOUNT", "BITFIELD",
    "BITFIELD_RO", "BITOP", "BITPOS", "BLMOVE", "BLMPOP", "BLPOP", "BRPOP", "BRPOPLPUSH", "BZMPOP",
    "BZPOPMAX", "BZPOPMIN", "CLIENT", "CLUSTER", "COMMAND", "CONFIG", "COPY", "DBSIZE", "DEBUG",
    "DECR", "DECRBY", "DEL", "DELEX", "DIGEST", "DISCARD", "DUMP", "ECHO", "EVAL", "EVALSHA",
    "EVALSHA_RO", "EVAL_RO", "EXEC", "EXISTS", "EXPIRE", "EXPIREAT", "EXPIRETIME", "FAILOVER",
    "FCALL", "FCALL_RO", "FLUSHALL", "FLUSHDB", "FUNCTION", "GEOADD", "GEODIST", "GEOHASH",
    "GEOPOS", "GEORADIUS", "GEORADIUSBYMEMBER", "GEORADIUSBYMEMBER_RO", "GEORADIUS_RO",
    "GEOSEARCH", "GEOSEARCHSTORE", "GET", "GETBIT", "GETDEL", "GETEX", "GETRANGE", "GETSET",
    "HDEL", "HELLO", "HEXISTS", "HEXPIRE", "HEXPIREAT", "HEXPIRETIME", "HGET", "HGETALL",
    "HGETDEL", "HGETEX", "HINCRBY", "HINCRBYFLOAT", "HKEYS", "HLEN", "HMGET", "HMSET", "HPERSIST",
    "HPEXPIRE", "HPEXPIREAT", "HPEXPIRETIME", "HPTTL", "HRANDFIELD", "HSCAN", "HSET", "HSETEX",
    "HSETNX", "HSTRLEN", "HTTL", "HVALS", "INCR", "INCRBY", "INCRBYFLOAT", "INFO", "KEYS",
    "LASTSAVE", "LATENCY", "LCS", "LINDEX", "LINSERT", "LLEN", "LMOVE", "LMPOP", "LOLWUT", "LPOP",
    "LPOS", "LPUSH", "LPUSHX", "LRANGE", "LREM", "LSET", "LTRIM", "MEMORY", "MGET", "MIGRATE",
    "MODULE", "MONITOR", "MOVE", "MSET", "MSETEX", "MSETNX", "MULTI", "OBJECT", "PERSIST",
    "PEXPIRE", "PEXPIREAT", "PEXPIRETIME", "PFADD", "PFCOUNT", "PFMERGE", "PING", "PSETEX",
    "PSUBSCRIBE", "PSYNC", "PTTL", "PUBLISH", "PUBSUB", "PUNSUBSCRIBE", "QUIT", "RANDOMKEY",
    "READONLY", "READWRITE", "RENAME", "RENAMENX", "REPLCONF", "REPLICAOF", "RESET", "RESTORE",
    "RESTORE-ASKING", "ROLE", "RPOP", "RPOPLPUSH", "RPUSH", "RPUSHX", "SADD", "SAVE", "SCAN",
    "SCARD", "SCRIPT", "SDIFF", "SDIFFSTORE", "SELECT", "SET", "SETBIT", "SETEX", "SETNX",
    "SETRANGE", "SHUTDOWN", "SINTER", "SINTERCARD", "SINTERSTORE", "SISMEMBER", "SLAVEOF",
    "SLOWLOG", "SMEMBERS", "SMISMEMBER", "SMOVE", "SORT", "SORT_RO", "SPOP", "SPUBLISH",
    "SRANDMEMBER", "SREM", "SSCAN", "SSUBSCRIBE", "STRLEN", "SUBSCRIBE", "SUBSTR", "SUNION",
    "SUNIONSTORE", "SUNSUBSCRIBE", "SWAPDB", "SYNC", "TIME", "TOUCH", "TTL", "TYPE", "UNLINK",
    "UNSUBSCRIBE", "UNWATCH", "WAIT", "WAITAOF", "WATCH", "XACK", "XACKDEL", "XADD", "XAUTOCLAIM",
    "XCLAIM", "XDEL", "XDELEX", "XGROUP", "XINFO", "XLEN", "XPENDING", "XRANGE", "XREAD",
    "XREADGROUP", "XREVRANGE", "XSETID", "XTRIM", "ZADD", "ZCARD", "ZCOUNT", "ZDIFF", "ZDIFFSTORE",
    "ZINCRBY", "ZINTER", "ZINTERCARD", "ZINTERSTORE", "ZLEXCOUNT", "ZMPOP", "ZMSCORE", "ZPOPMAX",
    "ZPOPMIN", "ZRANDMEMBER", "ZRANGE", "ZRANGEBYLEX", "ZRANGEBYSCORE", "ZRANGESTORE", "ZRANK",
    "ZREM", "ZREMRANGEBYLEX", "ZREMRANGEBYRANK", "ZREMRANGEBYSCORE", "ZREVRANGE", "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE", "ZREVRANK", "ZSCAN", "ZSCORE", "ZUNION", "ZUNIONSTORE",
];

/// All completion candidates for the first token of `line`, with the SPEC-3 case rule applied
/// (the typed prefix is kept verbatim; the added suffix is upper-case if the whole typed prefix
/// is upper-case — an empty prefix counts — otherwise lower-case). Returns each candidate as the
/// full new line. Empty when the line already has more than one token or nothing matches.
pub fn completions(line: &str) -> Vec<String> {
    let lead = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(lead);
    if rest.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let upper = rest.to_uppercase();
    let is_upper = upper == rest;
    COMMANDS
        .iter()
        .filter(|c| c.starts_with(&upper))
        .map(|c| {
            let suffix = &c[upper.len()..];
            let suffix = if is_upper { suffix.to_string() } else { suffix.to_lowercase() };
            format!("{indent}{rest}{suffix}")
        })
        .collect()
}

/// Tab completion of the first token. Returns the full new line when it differs from `line`:
///
/// * a single match → the completed command;
/// * several matches → the longest common prefix, if it extends what was typed (use
///   [`completions`] to list the candidates);
/// * otherwise `None`.
pub fn complete(line: &str) -> Option<String> {
    let all = completions(line);
    let first = all.first()?;
    let mut common = first.clone();
    for c in &all[1..] {
        let n = common
            .chars()
            .zip(c.chars())
            .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
            .count();
        common = common.chars().take(n).collect();
    }
    (common != line && common.len() > line.len()).then_some(common)
}

fn command_name(args: &[Vec<u8>]) -> Option<String> {
    args.first().map(|a| String::from_utf8_lossy(a).to_uppercase())
}

/// `Some(prompt)` when the command must be confirmed first: FLUSHALL / FLUSHDB with any
/// arguments (case-insensitive). The prompt is `Are you sure (y/n)? `.
pub fn needs_confirm(args: &[Vec<u8>]) -> Option<String> {
    match command_name(args).as_deref() {
        Some("FLUSHALL" | "FLUSHDB") => Some(CONFIRM_PROMPT.to_string()),
        _ => None,
    }
}

/// Interprets the answer to the confirmation prompt: `Some(true)` for `y`/`yes`, `Some(false)`
/// for `n`/`no` (trimmed, case-insensitive), `None` for anything else (re-prompt).
pub fn parse_confirm(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// Commands that switch the terminal into a different mode or change the UI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Special {
    /// `MONITOR` (exactly one argument): open a monitor stream.
    Monitor,
    /// `SUBSCRIBE ch [ch ...]`: open a pub/sub stream on these channels.
    Subscribe {
        /// Channels (lossy UTF-8).
        channels: Vec<String>,
    },
    /// `PSUBSCRIBE pat [pat ...]`.
    PSubscribe {
        /// Patterns (lossy UTF-8).
        patterns: Vec<String>,
    },
    /// `SELECT n` (the UI db changes when it succeeds).
    Select(u32),
    /// Everything else: run it as a plain command.
    None,
}

/// Classifies a tokenised command (case-insensitive). `SUBSCRIBE`/`PSUBSCRIBE` need at least one
/// argument and `MONITOR` must have none, otherwise the command is [`Special::None`] and goes to
/// the server as is (which answers with an error).
pub fn classify(args: &[Vec<u8>]) -> Special {
    let lossy = |a: &Vec<u8>| String::from_utf8_lossy(a).into_owned();
    match command_name(args).as_deref() {
        Some("MONITOR") if args.len() == 1 => Special::Monitor,
        Some("SUBSCRIBE") if args.len() > 1 => Special::Subscribe {
            channels: args[1..].iter().map(lossy).collect(),
        },
        Some("PSUBSCRIBE") if args.len() > 1 => Special::PSubscribe {
            patterns: args[1..].iter().map(lossy).collect(),
        },
        Some("SELECT") if args.len() == 2 => String::from_utf8_lossy(&args[1])
            .trim()
            .parse::<u32>()
            .map_or(Special::None, Special::Select),
        _ => Special::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(line: &str) -> Vec<String> {
        split_args(line)
            .unwrap()
            .into_iter()
            .map(|a| String::from_utf8_lossy(&a).into_owned())
            .collect()
    }

    fn b(line: &str) -> Vec<Vec<u8>> {
        split_args(line).unwrap()
    }

    #[test]
    fn plain_and_whitespace() {
        assert_eq!(s("get foo"), ["get", "foo"]);
        assert_eq!(s("  get \t foo\n"), ["get", "foo"]);
        assert!(s("").is_empty());
        assert!(s("   \t ").is_empty());
        assert_eq!(s("a   b"), ["a", "b"]);
        // \v and \f are skipped between tokens but are *not* delimiters inside one (C quirk)
        assert_eq!(s("\x0bget"), ["get"]);
        assert_eq!(s("a\x0bb"), ["a\x0bb"]);
    }

    #[test]
    fn double_quotes() {
        assert_eq!(s(r#"set k "hello world""#), ["set", "k", "hello world"]);
        assert_eq!(s(r#""a\nb""#), ["a\nb"]);
        assert_eq!(s(r#""\r\t\b\a""#), ["\r\t\x08\x07"]);
        assert_eq!(s(r#""q\"q""#), ["q\"q"]);
        assert_eq!(s(r#""back\\slash""#), ["back\\slash"]);
        assert_eq!(s(r#""\z""#), ["z"]); // unknown escape yields the char
        assert_eq!(s(r#""'single'""#), ["'single'"]);
        assert_eq!(s(r#""""#), [""]); // empty argument
    }

    #[test]
    fn hex_escapes() {
        assert_eq!(b(r#""\x41\x62""#), vec![b"Ab".to_vec()]);
        assert_eq!(b(r#""\xff\x00""#), vec![vec![0xff, 0x00]]);
        assert_eq!(b(r#""\xFF""#), vec![vec![0xff]]);
        // invalid hex: backslash handled as a generic escape -> "x" then the digits
        assert_eq!(s(r#""\xZZ""#), ["xZZ"]);
        assert_eq!(s(r#""\x4""#), ["x4"]);
        assert_eq!(s(r#""\x""#), ["x"]);
    }

    #[test]
    fn single_quotes() {
        assert_eq!(s("'a b'"), ["a b"]);
        assert_eq!(s(r"'it\'s'"), ["it's"]);
        assert_eq!(s(r"'a\nb'"), ["a\\nb"]); // no escapes except \'
        assert_eq!(s(r"'a\\b'"), ["a\\\\b"]);
        assert_eq!(s(r#"'say "hi"'"#), ["say \"hi\""]);
        assert_eq!(s("''"), [""]);
        // `\'` is an escaped quote, so this string never terminates
        assert_eq!(split_args(r"'\'"), Err(SplitError));
    }

    #[test]
    fn closing_quote_must_precede_space() {
        assert_eq!(split_args(r#""a"b"#), Err(SplitError));
        assert_eq!(split_args(r#""a""b""#), Err(SplitError));
        assert_eq!(split_args("'a'b"), Err(SplitError));
        assert_eq!(split_args(r#"'a'"b""#), Err(SplitError));
        assert_eq!(s(r#""a" "b""#), ["a", "b"]);
        assert_eq!(s("'a'\t'b'"), ["a", "b"]);
        assert_eq!(s(r#""a"   "#), ["a"]);
    }

    #[test]
    fn unbalanced_quotes() {
        assert_eq!(split_args(r#""abc"#), Err(SplitError));
        assert_eq!(split_args("'abc"), Err(SplitError));
        assert_eq!(split_args(r#"get "a"#), Err(SplitError));
        assert_eq!(split_args(r#""abc\"#), Err(SplitError)); // trailing backslash
        assert_eq!(split_args(r#"""""#), Err(SplitError));
        assert_eq!(SplitError.to_string(), "Invalid argument(s)");
    }

    #[test]
    fn quote_in_middle_of_token() {
        assert_eq!(s(r#"ab"c d"ef"#.trim_end_matches("ef")), ["abc d"]);
        assert_eq!(split_args(r#"ab"c d"ef"#), Err(SplitError));
        assert_eq!(s(r#"x"y z" w"#), ["xy z", "w"]);
        assert_eq!(s("a'b c' d"), ["ab c", "d"]);
    }

    #[test]
    fn nul_ends_the_input() {
        assert_eq!(s("get a\0 b"), ["get", "a"]);
        assert!(s("\0abc").is_empty());
    }

    #[test]
    fn unicode_passthrough() {
        assert_eq!(s("set ключ \"значение 1\""), ["set", "ключ", "значение 1"]);
    }

    #[test]
    fn commands_list_is_sane() {
        let mut sorted = COMMANDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, COMMANDS, "COMMANDS must be sorted and unique");
        assert!(COMMANDS.iter().all(|c| *c == c.to_uppercase()));
        for c in ["GET", "SET", "CLIENT", "CONFIG", "XGROUP", "FLUSHALL", "FLUSHDB", "MONITOR", "SUBSCRIBE", "PSUBSCRIBE", "SELECT", "HSCAN", "ZRANGE", "OBJECT", "SCRIPT"] {
            assert!(COMMANDS.contains(&c), "{c}");
        }
        assert!(COMMANDS.len() > 200);
    }

    #[test]
    fn completion_case_rule() {
        // lower-case typed prefix -> lower-case suffix
        assert_eq!(complete("flushd").as_deref(), Some("flushdb"));
        assert_eq!(complete("flushA").as_deref(), Some("flushAll")); // mixed: suffix lower
        // upper-case prefix -> upper-case suffix
        assert_eq!(complete("FLUSHD").as_deref(), Some("FLUSHDB"));
        // mixed prefix -> lower-case suffix, prefix kept verbatim
        assert_eq!(complete("FlushD").as_deref(), Some("FlushDb"));
        assert_eq!(complete("Monit").as_deref(), Some("Monitor"));
        // the typed prefix is never rewritten
        assert_eq!(complete("sUbSc").as_deref(), Some("sUbScribe"));
    }

    #[test]
    fn completion_ambiguity_and_misses() {
        // PSUBSCRIBE / PSYNC / PSETEX share "PS": common prefix is just "PS" -> nothing to add
        assert_eq!(complete("ps"), None);
        let all = completions("ps");
        assert!(all.contains(&"psubscribe".to_string()));
        assert!(all.contains(&"psync".to_string()));
        assert!(all.iter().all(|c| c.starts_with("ps")));
        // longest common prefix is applied: HGETA... only HGETALL; "hge" matches HGET, HGETALL,
        // HGETDEL, HGETEX -> common "hget"
        assert_eq!(complete("hge").as_deref(), Some("hget"));
        assert_eq!(complete("HGE").as_deref(), Some("HGET"));
        // exact match that is also a prefix of others: nothing more to add
        assert_eq!(complete("get"), None);
        // unique full command: unchanged -> None
        assert_eq!(complete("ping"), None);
        assert_eq!(complete("zzzz"), None);
        // only the first token is completed
        assert_eq!(complete("get fo"), None);
        assert_eq!(complete("get "), None);
        assert!(completions("get fo").is_empty());
    }

    #[test]
    fn completion_empty_prefix_and_indent() {
        // empty prefix counts as upper-case; all commands match, no common prefix
        assert_eq!(complete(""), None);
        let all = completions("");
        assert_eq!(all.len(), COMMANDS.len());
        assert_eq!(all[0], COMMANDS[0]); // upper-case
        // leading whitespace is preserved
        assert_eq!(complete("  flushd").as_deref(), Some("  flushdb"));
        assert_eq!(complete("   "), None);
    }

    #[test]
    fn confirm() {
        assert_eq!(needs_confirm(&b("flushall")).as_deref(), Some("Are you sure (y/n)? "));
        assert_eq!(needs_confirm(&b("FlushDb async")).as_deref(), Some("Are you sure (y/n)? "));
        assert_eq!(needs_confirm(&b("get flushall")), None);
        assert_eq!(needs_confirm(&[]), None);
        assert_eq!(parse_confirm(" Y "), Some(true));
        assert_eq!(parse_confirm("YES"), Some(true));
        assert_eq!(parse_confirm("no"), Some(false));
        assert_eq!(parse_confirm("N"), Some(false));
        assert_eq!(parse_confirm("yep"), None);
        assert_eq!(parse_confirm(""), None);
        assert_eq!(parse_confirm("nope"), None);
    }

    #[test]
    fn classify_commands() {
        assert_eq!(classify(&b("monitor")), Special::Monitor);
        assert_eq!(classify(&b("MONITOR extra")), Special::None);
        assert_eq!(
            classify(&b("subscribe a b")),
            Special::Subscribe { channels: vec!["a".into(), "b".into()] }
        );
        assert_eq!(classify(&b("subscribe")), Special::None);
        assert_eq!(
            classify(&b("PSubscribe news.*")),
            Special::PSubscribe { patterns: vec!["news.*".into()] }
        );
        assert_eq!(classify(&b("select 3")), Special::Select(3));
        assert_eq!(classify(&b("SELECT '12'")), Special::Select(12));
        assert_eq!(classify(&b("select \"5\"")), Special::Select(5));
        assert_eq!(classify(&b("select x")), Special::None);
        assert_eq!(classify(&b("select -1")), Special::None);
        assert_eq!(classify(&b("select")), Special::None);
        assert_eq!(classify(&b("select 1 2")), Special::None);
        assert_eq!(classify(&b("get a")), Special::None);
        assert_eq!(classify(&[]), Special::None);
    }
}
