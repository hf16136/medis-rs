//! Terminal output formatting (SPEC-3 §6.7/§6.8, §8.1): reply rendering into coloured text
//! lines, error lines, the TTL format of the content footer, MONITOR / pub-sub lines and the
//! verbatim banners. UI-independent: lines are [`Span`]s tagged with a semantic [`Tone`] that the
//! front end maps to colours.
//!
//! Recommended colours (SPEC-3):
//!
//! | Tone | Used for | Colour |
//! |---|---|---|
//! | `Plain` | indentation / separators | terminal default |
//! | `String` | strings, monitor args, pub/sub payload | `#d6ec9c` (monitor/pubsub lines: `#8f9d6a`) |
//! | `Integer` | integers, doubles, booleans, big numbers | `#78CF8A` |
//! | `Nil` | `null` | `#cf7ea9` |
//! | `Error` | error text | `#ee6868` |
//! | `Status` | emphasised label: map keys, monitor command / key args, pub/sub channel | `#cda869` (bold for map keys) |
//! | `Index` | array indices | `#848080` |
//! | `Dim` | secondary info: monitor time and `[db addr]`, pub/sub pattern | readable grey |

use chrono::{FixedOffset, Local, Offset, TimeZone};
use redis::Value;

use crate::cmdline::split_args;
use crate::worker::PubSubMsg;

/// Semantic colour class of a [`Span`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Default text / layout whitespace.
    Plain,
    /// String data.
    String,
    /// Integer-like data.
    Integer,
    /// The nil reply (`null`).
    Nil,
    /// Error text.
    Error,
    /// Emphasised label (map keys, command names, channels).
    Status,
    /// Array index.
    Index,
    /// Secondary, de-emphasised text.
    Dim,
}

/// A run of text with one tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text (never contains a newline).
    pub text: String,
    /// Its tone.
    pub tone: Tone,
}

impl Span {
    /// Convenience constructor.
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self { text: text.into(), tone }
    }
}

/// One output row.
pub type Line = Vec<Span>;

/// The plain text of a line (spans concatenated).
pub fn line_text(line: &Line) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

// ---- bytes -------------------------------------------------------------------------------------

fn push_hex(out: &mut String, b: u8) {
    use std::fmt::Write;
    let _ = write!(out, "\\x{b:02x}");
}

/// Display text for a binary-safe value: valid UTF-8 is shown as is (newlines and tabs kept);
/// invalid bytes and control characters other than `\n`, `\r`, `\t` are shown as `\xHH`.
pub fn display_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            if (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) && (c as u32) < 0x100 {
                push_hex(&mut out, c as u32 as u8);
            } else {
                out.push(c);
            }
        }
        for &bad in chunk.invalid() {
            push_hex(&mut out, bad);
        }
    }
    out
}

/// Splits text into display rows (`\n` or `\r\n`); text without a newline is one row, the empty
/// string is one empty row.
fn text_rows(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

fn text_lines(text: &str, tone: Tone) -> Vec<Line> {
    text_rows(text).into_iter().map(|r| vec![Span::new(r, tone)]).collect()
}

// ---- replies -----------------------------------------------------------------------------------

/// Renders a reply (SPEC-3 §6.7). No redis-cli decorations (`(integer)`, quotes, `(nil)`).
///
/// * Strings (bulk / simple / `OK` / verbatim): [`Tone::String`], newlines become separate rows,
///   an empty string is one empty row; binary uses [`display_bytes`].
/// * Integer, double, boolean, big number: [`Tone::Integer`]. Nil: `null` ([`Tone::Nil`]).
/// * Array and Set: one row per element, `<index>` ([`Tone::Index`], 0-based, right-aligned to
///   the widest index) then the item; a nested array continues on the same row as its index and
///   its further rows are indented. An **empty array renders no rows** (SPEC-3).
/// * Map: one row per entry, key in [`Tone::Status`] then the value (same layout as arrays).
/// * Push: rendered as an array whose first item is the push kind. Attribute: only the data is
///   rendered. ServerError: as [`render_error`].
pub fn render_reply(v: &Value) -> Vec<Line> {
    match v {
        Value::Nil => vec![vec![Span::new("null", Tone::Nil)]],
        Value::Int(i) => vec![vec![Span::new(i.to_string(), Tone::Integer)]],
        Value::Double(d) => vec![vec![Span::new(fmt_double(*d), Tone::Integer)]],
        Value::Boolean(b) => vec![vec![Span::new(b.to_string(), Tone::Integer)]],
        Value::BigNumber(n) => vec![vec![Span::new(n.to_string(), Tone::Integer)]],
        Value::BulkString(b) => text_lines(&display_bytes(b), Tone::String),
        Value::SimpleString(s) => text_lines(s, Tone::String),
        Value::Okay => text_lines("OK", Tone::String),
        Value::VerbatimString { text, .. } => text_lines(text, Tone::String),
        Value::Array(items) | Value::Set(items) => render_list(items.iter().map(Some)),
        Value::Map(pairs) => render_entries(
            pairs.iter().map(|(k, v)| (Span::new(inline(k), Tone::Status), v)),
        ),
        Value::Attribute { data, .. } => render_reply(data),
        Value::Push { kind, data } => {
            let head = Value::SimpleString(kind.to_string());
            render_list(std::iter::once(&head).chain(data.iter()).map(Some))
        }
        Value::ServerError(e) => render_error(&crate::error::redis_error_message(
            &redis::RedisError::from(e.clone()),
        )),
        other => vec![vec![Span::new(format!("{other:?}"), Tone::Plain)]],
    }
}

/// Redis-style double text (`inf`, `-inf`, `nan`; whole numbers without `.0`).
fn fmt_double(d: f64) -> String {
    if d.is_nan() {
        "nan".into()
    } else if d.is_infinite() {
        if d > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        d.to_string()
    }
}

/// One-line text of any value (used for map keys).
fn inline(v: &Value) -> String {
    render_reply(v)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_list<'a>(items: impl Iterator<Item = Option<&'a Value>>) -> Vec<Line> {
    let items: Vec<&Value> = items.flatten().collect();
    let width = items.len().saturating_sub(1).to_string().len();
    render_entries(items.into_iter().enumerate().map(|(i, v)| {
        (Span::new(format!("{i:>width$}"), Tone::Index), v)
    }))
}

/// Lays out `label value` entries: the value's first row follows the label on the same row, the
/// remaining rows are indented under it. An entry whose value renders nothing is just the label.
fn render_entries<'a>(entries: impl Iterator<Item = (Span, &'a Value)>) -> Vec<Line> {
    let mut out = Vec::new();
    for (label, value) in entries {
        let indent = label.text.chars().count() + 1;
        let mut rows = render_reply(value).into_iter();
        let mut first: Line = vec![label];
        if let Some(row) = rows.next() {
            first.push(Span::new(" ", Tone::Plain));
            first.extend(row);
        }
        out.push(first);
        for row in rows {
            let mut line = vec![Span::new(" ".repeat(indent), Tone::Plain)];
            line.extend(row);
            out.push(line);
        }
    }
    out
}

/// Renders an error message as is ([`Tone::Error`], one row per message line).
pub fn render_error(msg: &str) -> Vec<Line> {
    text_lines(msg, Tone::Error)
}

// ---- TTL ---------------------------------------------------------------------------------------

/// Human TTL for the content footer (SPEC-3 §8.1): the largest unit among `ms`, `s`, `min`, `h`,
/// `d` that is ≤ the value, rounded (half up) to at most 2 decimals with trailing zeros stripped
/// and no space: `500ms`, `1.5s`, `2min`, `3.25h`, `7d`; `0` → `0ms`.
///
/// Negative input (no TTL / missing key) returns an empty string: the footer shows the TTL span
/// only when `pttl >= 0`.
pub fn format_ttl(pttl_ms: i64) -> String {
    if pttl_ms < 0 {
        return String::new();
    }
    const UNITS: [(&str, i128); 5] = [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("min", 60_000),
        ("s", 1000),
        ("ms", 1),
    ];
    let v = i128::from(pttl_ms);
    let (name, unit) = UNITS.iter().copied().find(|(_, u)| v >= *u).unwrap_or(("ms", 1));
    let hundredths = (v * 100 + unit / 2) / unit;
    let (int, frac) = (hundredths / 100, hundredths % 100);
    let mut s = int.to_string();
    if frac != 0 {
        s.push('.');
        s.push_str(format!("{frac:02}").trim_end_matches('0'));
    }
    s.push_str(name);
    s
}

// ---- monitor / pub-sub -------------------------------------------------------------------------

/// Quotes an argument redis-cli style when needed: empty, containing whitespace, quotes,
/// backslashes or non-printables; otherwise returned as is. Escapes: `\"`, `\\`, `\n`, `\r`,
/// `\t`, `\a`, `\b`, `\xHH`.
pub fn quote_arg(arg: &[u8]) -> String {
    let plain = !arg.is_empty()
        && std::str::from_utf8(arg).is_ok_and(|s| {
            s.chars().all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '"' | '\'' | '\\'))
        });
    if plain {
        return String::from_utf8_lossy(arg).into_owned();
    }
    let mut out = String::from("\"");
    for chunk in arg.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{7}' => out.push_str("\\a"),
                '\u{8}' => out.push_str("\\b"),
                c if c.is_control() => push_hex(&mut out, c as u32 as u8),
                c => out.push(c),
            }
        }
        for &bad in chunk.invalid() {
            push_hex(&mut out, bad);
        }
    }
    out.push('"');
    out
}

/// Commands whose first argument is *not* a key (best effort, used to highlight keys).
const NO_KEY_FIRST: &[&str] = &[
    "ACL", "AUTH", "BGREWRITEAOF", "BGSAVE", "CLIENT", "CLUSTER", "COMMAND", "CONFIG", "DBSIZE",
    "DEBUG", "DISCARD", "ECHO", "EVAL", "EVALSHA", "EVALSHA_RO", "EVAL_RO", "EXEC", "FCALL",
    "FCALL_RO", "FLUSHALL", "FLUSHDB", "FUNCTION", "HELLO", "INFO", "KEYS", "LASTSAVE", "LATENCY",
    "MEMORY", "MODULE", "MONITOR", "MULTI", "OBJECT", "PING", "PSUBSCRIBE", "PSYNC", "PUBLISH",
    "PUBSUB", "PUNSUBSCRIBE", "QUIT", "RANDOMKEY", "READONLY", "READWRITE", "REPLCONF",
    "REPLICAOF", "RESET", "ROLE", "SAVE", "SCAN", "SCRIPT", "SELECT", "SHUTDOWN", "SLAVEOF",
    "SLOWLOG", "SUBSCRIBE", "SWAPDB", "SYNC", "TIME", "UNSUBSCRIBE", "UNWATCH", "WAIT", "WAITAOF",
    "XGROUP", "XINFO", "BITOP", "PFMERGE", "MIGRATE",
];

/// Formats a line of the `MONITOR` stream:
/// `1339518083.107412 [0 127.0.0.1:60866] "keys" "*"` →
/// `HH:MM:SS.ffffff [0 127.0.0.1:60866] KEYS *` (time in the **local** time zone; command
/// upper-cased in [`Tone::Status`]; the first argument of single-key commands in
/// [`Tone::Status`]; args quoted only when needed). Unparseable lines are passed through as one
/// [`Tone::String`] span.
pub fn format_monitor(line: &str) -> Line {
    monitor_line(line, None)
}

/// Like [`format_monitor`] with a fixed UTC offset in seconds (deterministic; used by tests and by
/// front ends that manage the time zone themselves).
pub fn format_monitor_with_offset(line: &str, utc_offset_secs: i32) -> Line {
    monitor_line(line, Some(utc_offset_secs))
}

fn monitor_time(token: &str, offset: Option<i32>) -> Option<String> {
    let (secs, frac) = token.split_once('.').unwrap_or((token, "0"));
    let secs: i64 = secs.parse().ok()?;
    if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut micros = frac.chars().take(6).collect::<String>();
    while micros.len() < 6 {
        micros.push('0');
    }
    let micros: u32 = micros.parse().ok()?;
    let utc = chrono::DateTime::from_timestamp(secs, micros * 1000)?;
    let off = match offset {
        Some(o) => FixedOffset::east_opt(o)?,
        None => Local.offset_from_utc_datetime(&utc.naive_utc()).fix(),
    };
    Some(utc.with_timezone(&off).format("%H:%M:%S%.6f").to_string())
}

fn monitor_line(line: &str, offset: Option<i32>) -> Line {
    let fallback = || vec![Span::new(line, Tone::String)];
    let line_trim = line.trim_end();
    let Some((time_tok, rest)) = line_trim.split_once(' ') else {
        return fallback();
    };
    let Some(time) = monitor_time(time_tok, offset) else {
        return fallback();
    };
    let rest = rest.trim_start();
    let Some((origin, args_text)) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) else {
        return fallback();
    };
    let Ok(args) = split_args(args_text) else {
        return fallback();
    };
    let mut out = vec![
        Span::new(time, Tone::Dim),
        Span::new(" ", Tone::Plain),
        Span::new(format!("[{origin}]"), Tone::Dim),
    ];
    let mut it = args.iter();
    let Some(cmd) = it.next() else {
        return out;
    };
    let name = String::from_utf8_lossy(cmd).to_uppercase();
    out.push(Span::new(" ", Tone::Plain));
    out.push(Span::new(quote_arg(name.as_bytes()), Tone::Status));
    let key_first = !NO_KEY_FIRST.contains(&name.as_str());
    for (i, a) in it.enumerate() {
        out.push(Span::new(" ", Tone::Plain));
        let tone = if i == 0 && key_first { Tone::Status } else { Tone::String };
        out.push(Span::new(quote_arg(a), tone));
    }
    out
}

/// Formats a pub/sub message: `<channel> <payload>` or, for pattern subscriptions,
/// `<pattern> <channel> <payload>`. Channel [`Tone::Status`], pattern [`Tone::Dim`], payload
/// [`Tone::String`] ([`display_bytes`]).
pub fn format_pubsub(msg: &PubSubMsg) -> Line {
    let mut out = Vec::new();
    if let Some(p) = &msg.pattern {
        out.push(Span::new(p.clone(), Tone::Dim));
        out.push(Span::new(" ", Tone::Plain));
    }
    out.push(Span::new(msg.channel.clone(), Tone::Status));
    out.push(Span::new(" ", Tone::Plain));
    out.push(Span::new(display_bytes(&msg.payload), Tone::String));
    out
}

/// Banner echoed when entering monitor mode (display in `#aac6e3`).
pub fn banner_monitor() -> String {
    "Enter monitor mode. Press Ctrl+C to exit.".to_string()
}

/// Banner echoed when entering subscription mode (display in `#aac6e3`; the prompt is empty
/// while subscribed).
pub fn banner_subscribe() -> String {
    "Enter subscription mode. Press Ctrl+C to exit.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txt(v: &Value) -> Vec<String> {
        render_reply(v).iter().map(line_text).collect()
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn scalars() {
        assert_eq!(txt(&Value::Nil), ["null"]);
        assert_eq!(txt(&Value::Int(-5)), ["-5"]);
        assert_eq!(txt(&Value::Okay), ["OK"]);
        assert_eq!(txt(&Value::SimpleString("PONG".into())), ["PONG"]);
        assert_eq!(txt(&bulk("hello")), ["hello"]);
        assert_eq!(txt(&bulk("")), [""]);
        assert_eq!(txt(&Value::Boolean(true)), ["true"]);
        assert_eq!(txt(&Value::Double(1.5)), ["1.5"]);
        assert_eq!(txt(&Value::Double(3.0)), ["3"]);
        assert_eq!(txt(&Value::Double(f64::INFINITY)), ["inf"]);
        assert_eq!(txt(&Value::Double(f64::NEG_INFINITY)), ["-inf"]);
        assert_eq!(txt(&Value::Double(f64::NAN)), ["nan"]);
        assert_eq!(
            txt(&Value::VerbatimString { format: redis::VerbatimFormat::Text, text: "txt".into() }),
            ["txt"]
        );
        let big = redis::parse_redis_value(b"(3492890328409238509324850943850943825024385\r\n")
            .map(|v| txt(&v))
            .unwrap_or_default();
        assert_eq!(big, ["3492890328409238509324850943850943825024385"]);
    }

    #[test]
    fn scalar_tones() {
        let tone = |v: Value| render_reply(&v)[0][0].tone;
        assert_eq!(tone(Value::Nil), Tone::Nil);
        assert_eq!(tone(Value::Int(1)), Tone::Integer);
        assert_eq!(tone(Value::Double(1.0)), Tone::Integer);
        assert_eq!(tone(Value::Boolean(false)), Tone::Integer);
        assert_eq!(tone(bulk("x")), Tone::String);
        assert_eq!(tone(Value::Okay), Tone::String);
        assert_eq!(tone(Value::SimpleString("x".into())), Tone::String);
    }

    #[test]
    fn multiline_strings() {
        assert_eq!(txt(&bulk("a\nb\r\nc")), ["a", "b", "c"]);
        assert_eq!(txt(&bulk("a\n")), ["a", ""]);
        let rows = render_reply(&bulk("x\ny"));
        assert!(rows.iter().all(|r| r.len() == 1 && r[0].tone == Tone::String));
    }

    #[test]
    fn binary_is_escaped() {
        assert_eq!(txt(&Value::BulkString(vec![b'a', 0xff, 0x00, b'b'])), ["a\\xff\\x00b"]);
        assert_eq!(display_bytes(b"tab\tnl\n"), "tab\tnl\n");
        assert_eq!(display_bytes("héllo ✓".as_bytes()), "héllo ✓");
        assert_eq!(display_bytes(&[0x1b, b'[']), "\\x1b[");
        assert_eq!(display_bytes(b""), "");
        assert_eq!(display_bytes(&[0xe2, 0x82]), "\\xe2\\x82"); // truncated sequence
    }

    #[test]
    fn flat_array() {
        let v = Value::Array(vec![bulk("a"), Value::Int(2), Value::Nil]);
        assert_eq!(txt(&v), ["0 a", "1 2", "2 null"]);
        let rows = render_reply(&v);
        assert_eq!(rows[0][0], Span::new("0", Tone::Index));
        assert_eq!(rows[0][1], Span::new(" ", Tone::Plain));
        assert_eq!(rows[0][2], Span::new("a", Tone::String));
        assert_eq!(rows[1][2].tone, Tone::Integer);
        assert_eq!(rows[2][2].tone, Tone::Nil);
    }

    #[test]
    fn index_alignment() {
        let v = Value::Array((0..11).map(Value::Int).collect());
        let t = txt(&v);
        assert_eq!(t[0], " 0 0");
        assert_eq!(t[9], " 9 9");
        assert_eq!(t[10], "10 10");
    }

    #[test]
    fn nested_arrays() {
        let v = Value::Array(vec![
            Value::Array(vec![bulk("a"), bulk("b")]),
            bulk("c"),
            Value::Array(vec![Value::Array(vec![Value::Int(1)])]),
        ]);
        assert_eq!(txt(&v), ["0 0 a", "  1 b", "1 c", "2 0 0 1"]);
    }

    #[test]
    fn multiline_inside_array_is_indented() {
        let v = Value::Array(vec![bulk("l1\nl2"), bulk("x")]);
        assert_eq!(txt(&v), ["0 l1", "  l2", "1 x"]);
    }

    #[test]
    fn empty_arrays_render_nothing() {
        assert!(render_reply(&Value::Array(vec![])).is_empty());
        assert!(render_reply(&Value::Set(vec![])).is_empty());
        assert!(render_reply(&Value::Map(vec![])).is_empty());
        let v = Value::Array(vec![Value::Array(vec![]), bulk("x")]);
        assert_eq!(txt(&v), ["0", "1 x"]);
    }

    #[test]
    fn map_and_set() {
        let m = Value::Map(vec![
            (bulk("name"), bulk("medis")),
            (bulk("tags"), Value::Array(vec![bulk("a"), bulk("b")])),
            (Value::Int(7), Value::Nil),
        ]);
        assert_eq!(txt(&m), ["name medis", "tags 0 a", "     1 b", "7 null"]);
        let rows = render_reply(&m);
        assert_eq!(rows[0][0], Span::new("name", Tone::Status));
        let s = Value::Set(vec![bulk("x"), bulk("y")]);
        assert_eq!(txt(&s), ["0 x", "1 y"]);
    }

    #[test]
    fn push_and_attribute() {
        let p = Value::Push {
            kind: redis::PushKind::Message,
            data: vec![bulk("chan"), bulk("payload")],
        };
        assert_eq!(txt(&p), ["0 message", "1 chan", "2 payload"]);
        let a = Value::Attribute {
            data: Box::new(Value::Int(1)),
            attributes: vec![(bulk("ttl"), Value::Int(2))],
        };
        assert_eq!(txt(&a), ["1"]);
    }

    #[test]
    fn server_error_value() {
        let e = redis::parse_redis_value(b"-ERR unknown command 'foo'\r\n").unwrap();
        let rows = render_reply(&e);
        assert_eq!(line_text(&rows[0]), "ERR unknown command 'foo'");
        assert_eq!(rows[0][0].tone, Tone::Error);
        // an error nested in an array keeps the error tone
        let arr = Value::Array(vec![Value::Int(1), e]);
        let rows = render_reply(&arr);
        assert_eq!(line_text(&rows[1]), "1 ERR unknown command 'foo'");
        assert_eq!(rows[1][2].tone, Tone::Error);
    }

    #[test]
    fn error_rendering() {
        let r = render_error("ERR wrong number of arguments");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], vec![Span::new("ERR wrong number of arguments", Tone::Error)]);
        assert_eq!(render_error("a\nb").len(), 2);
        assert_eq!(render_error("").len(), 1);
    }

    #[test]
    fn ttl_format() {
        assert_eq!(format_ttl(0), "0ms");
        assert_eq!(format_ttl(1), "1ms");
        assert_eq!(format_ttl(500), "500ms");
        assert_eq!(format_ttl(999), "999ms");
        assert_eq!(format_ttl(1000), "1s");
        assert_eq!(format_ttl(1500), "1.5s");
        assert_eq!(format_ttl(1234), "1.23s");
        assert_eq!(format_ttl(1235), "1.24s"); // half up
        assert_eq!(format_ttl(1005), "1.01s");
        assert_eq!(format_ttl(1004), "1s"); // rounds down, trailing zeros stripped
        assert_eq!(format_ttl(1996), "2s"); // 1.996 -> 2.00 -> "2"
    }

    #[test]
    fn ttl_format_units() {
        assert_eq!(format_ttl(59_000), "59s");
        assert_eq!(format_ttl(60_000), "1min");
        assert_eq!(format_ttl(120_000), "2min");
        assert_eq!(format_ttl(90_000), "1.5min");
        assert_eq!(format_ttl(3_600_000), "1h");
        assert_eq!(format_ttl(11_700_000), "3.25h");
        assert_eq!(format_ttl(86_400_000), "1d");
        assert_eq!(format_ttl(7 * 86_400_000), "7d");
        assert_eq!(format_ttl(86_400_000 * 3 / 2), "1.5d");
        assert!(format_ttl(i64::MAX).ends_with('d'));
        assert_eq!(format_ttl(-1), "");
        assert_eq!(format_ttl(-2), "");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_arg(b"abc"), "abc");
        assert_eq!(quote_arg(b"a:b/c*"), "a:b/c*");
        assert_eq!(quote_arg(b""), "\"\"");
        assert_eq!(quote_arg(b"a b"), "\"a b\"");
        assert_eq!(quote_arg(b"q\"q"), "\"q\\\"q\"");
        assert_eq!(quote_arg(b"back\\slash"), "\"back\\\\slash\"");
        assert_eq!(quote_arg(b"nl\n\t\r"), "\"nl\\n\\t\\r\"");
        assert_eq!(quote_arg(&[0x07, 0x08]), "\"\\a\\b\"");
        assert_eq!(quote_arg(&[b'a', 0xff, 0x01]), "\"a\\xff\\x01\"");
        assert_eq!(quote_arg("ключ".as_bytes()), "ключ");
        // quoting round-trips through split_args
        for raw in [&b"a b"[..], b"q\"q", b"x\\y", &[0xff, 0x00, b'z'], b"", b"it's"] {
            let q = quote_arg(raw);
            assert_eq!(split_args(&q).unwrap(), vec![raw.to_vec()], "{q}");
        }
    }

    const MON: &str = r#"1339518083.107412 [0 127.0.0.1:60866] "keys" "*""#;

    #[test]
    fn monitor_line_utc() {
        let l = format_monitor_with_offset(MON, 0);
        assert_eq!(line_text(&l), "16:21:23.107412 [0 127.0.0.1:60866] KEYS *");
        assert_eq!(l[0], Span::new("16:21:23.107412", Tone::Dim));
        assert_eq!(l[2], Span::new("[0 127.0.0.1:60866]", Tone::Dim));
        assert_eq!(l[4], Span::new("KEYS", Tone::Status));
        // KEYS takes a pattern, not a key
        assert_eq!(l[6], Span::new("*", Tone::String));
    }

    #[test]
    fn monitor_offset_and_micros() {
        let l = format_monitor_with_offset(r#"1339518083.5 [1 lua] "get" "user:1""#, 3600 * 7);
        assert_eq!(line_text(&l), "23:21:23.500000 [1 lua] GET user:1");
        assert_eq!(l[6].tone, Tone::Status); // key of a single-key command
        let l = format_monitor_with_offset(r#"1339518083.000001 [0 x] "set" "k" "v w""#, -3600);
        assert_eq!(line_text(&l), "15:21:23.000001 [0 x] SET k \"v w\"");
        assert_eq!(l[6].tone, Tone::Status);
        assert_eq!(l[8].tone, Tone::String);
    }

    #[test]
    fn monitor_escapes_and_fallbacks() {
        let l = format_monitor_with_offset(r#"1.000000 [0 a:1] "set" "k" "\xff\"q""#, 0);
        assert_eq!(line_text(&l), "00:00:01.000000 [0 a:1] SET k \"\\xff\\\"q\"");
        // command only
        let l = format_monitor_with_offset(r#"1.0 [0 a:1] "ping""#, 0);
        assert_eq!(line_text(&l), "00:00:01.000000 [0 a:1] PING");
        // unparseable lines pass through
        for bad in ["OK", "garbage line here", "abc [0 x] \"get\"", "1.0 no-bracket \"get\"", "1.0 [0 x] \"unterminated"] {
            let l = format_monitor_with_offset(bad, 0);
            assert_eq!(l, vec![Span::new(bad, Tone::String)], "{bad}");
        }
    }

    #[test]
    fn monitor_local_time_runs() {
        // the local offset depends on the machine: only check the shape
        let t = line_text(&format_monitor(MON));
        assert_eq!(t.len(), "HH:MM:SS.ffffff [0 127.0.0.1:60866] KEYS *".len());
        assert_eq!(&t[2..3], ":");
        assert!(t.ends_with(".107412 [0 127.0.0.1:60866] KEYS *"));
    }

    #[test]
    fn pubsub_lines() {
        let m = PubSubMsg { channel: "news".into(), pattern: None, payload: b"hello world".to_vec() };
        let l = format_pubsub(&m);
        assert_eq!(line_text(&l), "news hello world");
        assert_eq!(l[0].tone, Tone::Status);
        assert_eq!(l[2], Span::new("hello world", Tone::String));
        let p = PubSubMsg { channel: "news.sport".into(), pattern: Some("news.*".into()), payload: vec![0xff] };
        let l = format_pubsub(&p);
        assert_eq!(line_text(&l), "news.* news.sport \\xff");
        assert_eq!(l[0].tone, Tone::Dim);
    }

    #[test]
    fn banners_are_verbatim() {
        assert_eq!(banner_monitor(), "Enter monitor mode. Press Ctrl+C to exit.");
        assert_eq!(banner_subscribe(), "Enter subscription mode. Press Ctrl+C to exit.");
    }
}
