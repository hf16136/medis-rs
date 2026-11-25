//! `medis-core`: the UI-independent core of the Medis Redis GUI port.
//!
//! The crate is organised as follows:
//!
//! * [`config`]   – [`ConnectionConfig`] (favorites / quick connect form) and `connection_key()`.
//! * [`storage`]  – favorites, pattern store and pane sizes persisted as JSON.
//! * [`ssh`]      – SSH local-forward tunnel (russh).
//! * [`conn`]     – [`RedisConn`] (single or cluster) and [`connect`] with the verbatim error strings.
//! * [`scan`]     – the SPEC-1 `SCAN` algorithm (per primary in cluster mode).
//! * [`scripts`]  – the Lua scripts (`setKeepTTL`, `lremindex`, `duplicateKey`).
//! * [`ops`]      – the async key / content operations needed by the UI.
//! * [`editor`]   – value-editor encodings (raw / JSON / MessagePack).
//! * [`worker`]   – a tokio task per connection tab driven by `Req` / `Evt` channels.
//! * [`mod@format`], [`cmdline`] – terminal helpers (reply rendering, TTL format, banners;
//!   `sdssplitargs`, completion, command classification).
//!
//! The crate never depends on egui. Call [`install_crypto_provider`] once at program start
//! (it is also invoked from [`connect`]).

pub mod cmdline;
pub mod config;
pub mod conn;
pub mod editor;
pub mod error;
pub mod format;
pub mod ops;
pub mod scan;
pub mod scripts;
pub mod ssh;
pub mod storage;
pub mod worker;

pub use cmdline::{
    COMMANDS, CONFIRM_PROMPT, PROMPT, Special, SplitError, classify, complete, completions,
    needs_confirm, parse_confirm, split_args,
};
pub use config::{ClusterConfig, ConnectionConfig, TlsConfig};
pub use conn::{Connected, RedisConn, connect, connect_with};
pub use error::CoreError;
pub use format::{
    Line, Span, Tone, banner_monitor, banner_subscribe, display_bytes, format_monitor,
    format_monitor_with_offset, format_pubsub, format_ttl, line_text, quote_arg, render_error,
    render_reply,
};
pub use redis;
pub use scan::{KeyItem, ScanCursor};
pub use worker::{Evt, Reply, Req, ReqId, Worker};

/// Installs the rustls `aws-lc-rs` crypto provider as the process default.
///
/// Required before any TLS connection is made. Safe to call more than once (later calls are
/// no-ops).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
