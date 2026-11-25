//! Value-editor encodings (SPEC-2 §5.7): raw / JSON / MessagePack detection and encoding.
//!
//! * `raw`: lossy UTF-8.
//! * `json`: available when the bytes parse as a JSON **object or array**; shown pretty-printed
//!   with TAB indentation.
//! * `messagepack`: only offered when json is not; the bytes must decode (completely) to a map or
//!   array, shown as tab-pretty JSON. Binary values render as `{"type":"Buffer","data":[..]}`.

use serde_json::{Map, Number, Value as J};

/// Editor mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Plain text.
    Raw,
    /// JSON (object/array).
    Json,
    /// MessagePack shown as JSON.
    MessagePack,
}

/// Alert text for invalid JSON on save.
pub const INVALID_JSON: &str = "The json is invalid. Please check again.";

/// The per-mode texts of a loaded value; `None` = mode unavailable (disabled in the select).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modes {
    /// Lossy UTF-8 text.
    pub raw: String,
    /// Pretty JSON, if the bytes are a JSON object/array.
    pub json: Option<String>,
    /// Pretty JSON of the decoded MessagePack (only when `json` is `None`).
    pub messagepack: Option<String>,
}

impl Modes {
    /// Analyses `bytes`.
    pub fn detect(bytes: &[u8]) -> Self {
        let raw = String::from_utf8_lossy(bytes).into_owned();
        let json = format_json(&raw, true);
        let messagepack = if json.is_some() { None } else { format_msgpack(bytes) };
        Self { raw, json, messagepack }
    }

    /// Initial mode: json if available, else messagepack, else raw.
    pub fn initial_mode(&self) -> Mode {
        if self.json.is_some() {
            Mode::Json
        } else if self.messagepack.is_some() {
            Mode::MessagePack
        } else {
            Mode::Raw
        }
    }

    /// The text of `mode` (`None` if unavailable).
    pub fn text(&self, mode: Mode) -> Option<&str> {
        match mode {
            Mode::Raw => Some(&self.raw),
            Mode::Json => self.json.as_deref(),
            Mode::MessagePack => self.messagepack.as_deref(),
        }
    }

    /// Whether `mode` can be selected.
    pub fn available(&self, mode: Mode) -> bool {
        self.text(mode).is_some()
    }
}

/// Parses `text` as JSON; only objects/arrays are accepted. Returns the pretty (TAB) or minified
/// text.
pub fn format_json(text: &str, beautify: bool) -> Option<String> {
    let v: J = serde_json::from_str(text).ok()?;
    if !(v.is_object() || v.is_array()) {
        return None;
    }
    Some(if beautify { to_pretty(&v) } else { serde_json::to_string(&v).ok()? })
}

fn to_pretty(v: &J) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    if v.serialize(&mut ser).is_err() {
        return v.to_string();
    }
    String::from_utf8(buf).unwrap_or_default()
}

/// Decodes MessagePack `bytes` (the whole buffer) to pretty JSON when the root is a map/array.
pub fn format_msgpack(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let mut cur = std::io::Cursor::new(bytes);
    let v = rmpv::decode::read_value(&mut cur).ok()?;
    if cur.position() as usize != bytes.len() {
        return None;
    }
    if !matches!(v, rmpv::Value::Map(_) | rmpv::Value::Array(_)) {
        return None;
    }
    Some(to_pretty(&mp_to_json(&v)))
}

fn buffer_json(data: &[u8]) -> J {
    let mut m = Map::new();
    m.insert("type".into(), J::String("Buffer".into()));
    m.insert("data".into(), J::Array(data.iter().map(|b| J::from(*b)).collect()));
    J::Object(m)
}

fn mp_to_json(v: &rmpv::Value) -> J {
    use rmpv::Value as M;
    match v {
        M::Nil => J::Null,
        M::Boolean(b) => J::Bool(*b),
        M::Integer(i) => {
            if let Some(n) = i.as_i64() {
                J::from(n)
            } else if let Some(n) = i.as_u64() {
                J::from(n)
            } else {
                J::Null
            }
        }
        M::F32(f) => Number::from_f64(f64::from(*f)).map_or(J::Null, J::Number),
        M::F64(f) => Number::from_f64(*f).map_or(J::Null, J::Number),
        M::String(s) => J::String(s.as_str().map(str::to_string).unwrap_or_else(|| {
            String::from_utf8_lossy(s.as_bytes()).into_owned()
        })),
        M::Binary(b) => buffer_json(b),
        M::Array(a) => J::Array(a.iter().map(mp_to_json).collect()),
        M::Map(m) => {
            let mut o = Map::new();
            for (k, v) in m {
                let key = match k {
                    M::String(s) => s.as_str().unwrap_or_default().to_string(),
                    M::Integer(i) => i.to_string(),
                    M::Boolean(b) => b.to_string(),
                    M::Nil => "null".to_string(),
                    other => mp_to_json(other).to_string(),
                };
                o.insert(key, mp_to_json(v));
            }
            J::Object(o)
        }
        M::Ext(_, data) => buffer_json(data),
    }
}

fn json_to_mp(v: &J) -> rmpv::Value {
    use rmpv::Value as M;
    match v {
        J::Null => M::Nil,
        J::Bool(b) => M::Boolean(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                M::from(i)
            } else if let Some(u) = n.as_u64() {
                M::from(u)
            } else {
                M::F64(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => M::from(s.as_str()),
        J::Array(a) => M::Array(a.iter().map(json_to_mp).collect()),
        J::Object(o) => M::Map(o.iter().map(|(k, v)| (M::from(k.as_str()), json_to_mp(v))).collect()),
    }
}

/// Produces the bytes to store for the active `mode` and its edit buffer `text`.
///
/// * raw → the text bytes;
/// * json → must parse to an object/array, stored minified;
/// * messagepack → same validation, then msgpack-encoded.
///
/// Errors carry [`INVALID_JSON`].
pub fn encode_for_save(mode: Mode, text: &str) -> Result<Vec<u8>, String> {
    match mode {
        Mode::Raw => Ok(text.as_bytes().to_vec()),
        Mode::Json => format_json(text, false)
            .map(String::into_bytes)
            .ok_or_else(|| INVALID_JSON.to_string()),
        Mode::MessagePack => {
            let v: J = serde_json::from_str(text).map_err(|_| INVALID_JSON.to_string())?;
            if !(v.is_object() || v.is_array()) {
                return Err(INVALID_JSON.to_string());
            }
            let mut out = Vec::new();
            rmpv::encode::write_value(&mut out, &json_to_mp(&v)).map_err(|e| e.to_string())?;
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_json() {
        let m = Modes::detect(br#"{"a":1,"b":[1,2]}"#);
        assert_eq!(m.initial_mode(), Mode::Json);
        assert!(m.json.as_ref().unwrap().contains("\n\t\"a\": 1"));
        assert!(m.messagepack.is_none());
        assert_eq!(Modes::detect(b"42").initial_mode(), Mode::Raw);
        assert_eq!(Modes::detect(b"\"str\"").initial_mode(), Mode::Raw);
        assert_eq!(Modes::detect(b"hello").initial_mode(), Mode::Raw);
        assert_eq!(Modes::detect(b"").initial_mode(), Mode::Raw);
    }

    #[test]
    fn msgpack_round_trip() {
        let bytes = encode_for_save(Mode::MessagePack, r#"{"a":1,"b":[true,null,"x"],"c":1.5}"#).unwrap();
        let m = Modes::detect(&bytes);
        assert_eq!(m.initial_mode(), Mode::MessagePack);
        let txt = m.messagepack.unwrap();
        let v: J = serde_json::from_str(&txt).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"][2], "x");
        assert_eq!(v["c"], 1.5);
        assert!(txt.contains("\n\t"));
    }

    #[test]
    fn msgpack_binary_is_buffer() {
        // fixmap 1: "k" -> bin8 [1,2]
        let bytes = [0x81, 0xa1, b'k', 0xc4, 0x02, 0x01, 0x02];
        let t = format_msgpack(&bytes).unwrap();
        let v: J = serde_json::from_str(&t).unwrap();
        assert_eq!(v["k"]["type"], "Buffer");
        assert_eq!(v["k"]["data"], serde_json::json!([1, 2]));
        // trailing garbage is rejected
        assert!(format_msgpack(&[0x90, 0x00]).is_none());
        // primitives are rejected
        assert!(format_msgpack(&[0x01]).is_none());
    }

    #[test]
    fn save_validation() {
        assert_eq!(encode_for_save(Mode::Raw, "x").unwrap(), b"x");
        assert_eq!(encode_for_save(Mode::Json, "{ \"a\" : 1 }").unwrap(), br#"{"a":1}"#);
        assert_eq!(encode_for_save(Mode::Json, "12").unwrap_err(), INVALID_JSON);
        assert_eq!(encode_for_save(Mode::MessagePack, "{").unwrap_err(), INVALID_JSON);
    }
}
