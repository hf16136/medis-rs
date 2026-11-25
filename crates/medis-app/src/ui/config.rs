//! Config tab (SPEC-3 7). Runs `CONFIG GET *` lazily the first time `show` runs, groups the
//! settings, and offers Reload / Save To Config File / Apply.
//!
//! Uses `Op::Exec` with `CONFIG GET/SET/REWRITE`; confirm dialogs go through `cx.modal(..)`.
//! Nothing here blocks: every request is answered through `handle_evt`.

use std::collections::HashMap;

use egui::{Align, Color32, DragValue, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2};
use medis_core::format::display_bytes;
use medis_core::redis::Value;
use medis_core::worker::{Op, Reply};
use medis_core::{Evt, ReqId};

use crate::instance::{Cx, Instance, Module};
use crate::ui::modal::ModalSpec;
use crate::ui::theme::{Palette, primary_button};

const COLUMN_W: f32 = 430.0;
const LABEL_W: f32 = 170.0;

// ---- the setting catalogue (SPEC-3 7.1) ----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Bool,
    Num,
    Enum(&'static [&'static str]),
}

use Kind::{Bool as B, Num as N, Text as T};

const LOGLEVEL: &[&str] = &["debug", "verbose", "notice", "warning"];
const POLICY: &[&str] = &[
    "noeviction",
    "volatile-lru",
    "allkeys-lru",
    "volatile-lfu",
    "allkeys-lfu",
    "volatile-random",
    "allkeys-random",
    "volatile-ttl",
];
const APPENDFSYNC: &[&str] = &["everysec", "always", "no"];

type GroupDef = (&'static str, &'static [(&'static str, Kind)]);

const GROUPS: &[GroupDef] = &[
    (
        "General",
        &[
            ("port", N),
            ("bind", T),
            ("unixsocket", T),
            ("unixsocketperm", N),
            ("daemonize", B),
            ("pidfile", T),
            ("tcp-backlog", N),
            ("tcp-keepalive", N),
            ("timeout", N),
            ("databases", N),
        ],
    ),
    (
        "Logging",
        &[
            ("loglevel", Kind::Enum(LOGLEVEL)),
            ("logfile", T),
            ("syslog-enabled", B),
            ("syslog-ident", T),
            ("syslog-facility", T),
        ],
    ),
    (
        "Snap Shotting",
        &[
            ("dbfilename", T),
            ("dir", T),
            ("save", T),
            ("stop-writes-on-bgsave-error", B),
            ("rdbcompression", B),
            ("rdbchecksum", B),
        ],
    ),
    (
        "Replication",
        &[
            ("slaveof", T),
            ("replicaof", T),
            ("masterauth", T),
            ("slave-serve-stale-data", B),
            ("replica-serve-stale-data", B),
            ("slave-read-only", B),
            ("replica-read-only", B),
            ("repl-diskless-sync", B),
            ("repl-diskless-sync-delay", N),
            ("repl-ping-slave-period", N),
            ("repl-ping-replica-period", N),
            ("repl-timeout", N),
            ("repl-disable-tcp-nodelay", B),
            ("repl-backlog-size", T),
            ("repl-backlog-ttl", N),
            ("slave-priority", N),
            ("replica-priority", N),
            ("min-slaves-to-write", N),
            ("min-replicas-to-write", N),
            ("min-slaves-max-lag", N),
            ("min-replicas-max-lag", N),
        ],
    ),
    ("Security", &[("requirepass", T), ("rename-command", T)]),
    (
        "Limits",
        &[
            ("maxclients", T),
            ("maxmemory", T),
            ("maxmemory-policy", Kind::Enum(POLICY)),
            ("maxmemory-samples", N),
        ],
    ),
    (
        "Append Only Mode",
        &[
            ("appendonly", B),
            ("appendfilename", T),
            ("appendfsync", Kind::Enum(APPENDFSYNC)),
            ("no-appendfsync-on-rewrite", B),
            ("auto-aof-rewrite-percentage", N),
            ("auto-aof-rewrite-min-size", T),
            ("aof-load-truncated", B),
        ],
    ),
    ("LUA Scripting", &[("lua-time-limit", N)]),
    (
        "Cluster",
        &[
            ("cluster-enabled", B),
            ("cluster-config-file", T),
            ("cluster-node-timeout", N),
            ("cluster-slave-validity-factor", N),
            ("cluster-migration-barrier", N),
            ("cluster-require-full-coverage", B),
        ],
    ),
    ("Slow Log", &[("slowlog-log-slower-than", N), ("slowlog-max-len", N)]),
    ("Latency Monitor", &[("latency-monitor-threshold", N)]),
    ("Event Notification", &[("notify-keyspace-events", T)]),
    (
        "Advanced Config",
        &[
            ("hash-max-ziplist-entries", N),
            ("hash-max-ziplist-value", N),
            ("hash-max-listpack-entries", N),
            ("hash-max-listpack-value", N),
            ("list-max-ziplist-entries", N),
            ("list-max-ziplist-value", N),
            ("set-max-intset-entries", N),
            ("zset-max-ziplist-entries", N),
            ("zset-max-ziplist-value", N),
            ("zset-max-listpack-entries", N),
            ("zset-max-listpack-value", N),
            ("hll-sparse-max-bytes", N),
            ("activerehashing", B),
            ("client-output-buffer-limit", T),
            ("hz", N),
            ("aof-rewrite-incremental-fsync", B),
            ("lazyfree-lazy-eviction", B),
            ("lazyfree-lazy-expire", B),
            ("lazyfree-lazy-server-del", B),
            ("lazyfree-lazy-user-del", B),
            ("lazyfree-lazy-user-flush", B),
            ("replica-lazy-flush", B),
            ("slave-lazy-flush", B),
        ],
    ),
];

/// Names that may be changed with `CONFIG SET` (everything else is shown read-only).
const WRITEABLE: &[&str] = &[
    // Persistence
    "dbfilename",
    "dir",
    "save",
    "rdbcompression",
    "stop-writes-on-bgsave-error",
    "appendonly",
    "appendfsync",
    "aof-rewrite-incremental-fsync",
    "aof-load-truncated",
    "auto-aof-rewrite-percentage",
    "auto-aof-rewrite-min-size",
    // Security
    "requirepass",
    "masterauth",
    // Clients and memory
    "maxclients",
    "maxmemory",
    "maxmemory-policy",
    "maxmemory-samples",
    "client-output-buffer-limit",
    "timeout",
    "tcp-keepalive",
    "lazyfree-lazy-eviction",
    "lazyfree-lazy-expire",
    "lazyfree-lazy-server-del",
    "lazyfree-lazy-user-del",
    "lazyfree-lazy-user-flush",
    // Replication
    "repl-disable-tcp-nodelay",
    "repl-diskless-sync",
    "repl-diskless-sync-delay",
    "repl-ping-slave-period",
    "repl-ping-replica-period",
    "repl-timeout",
    "repl-backlog-ttl",
    "repl-backlog-size",
    "slave-serve-stale-data",
    "replica-serve-stale-data",
    "slave-read-only",
    "replica-read-only",
    "slave-lazy-flush",
    "replica-lazy-flush",
    "slave-priority",
    "replica-priority",
    "min-slaves-to-write",
    "min-replicas-to-write",
    "min-slaves-max-lag",
    "min-replicas-max-lag",
    // Cluster
    "cluster-require-full-coverage",
    "cluster-node-timeout",
    "cluster-migration-barrier",
    "cluster-slave-validity-factor",
    // Encoding thresholds
    "hash-max-ziplist-entries",
    "hash-max-ziplist-value",
    "hash-max-listpack-entries",
    "hash-max-listpack-value",
    "list-max-ziplist-entries",
    "list-max-ziplist-value",
    "list-max-ziplist-size",
    "list-compress-depth",
    "set-max-intset-entries",
    "zset-max-ziplist-entries",
    "zset-max-ziplist-value",
    "zset-max-listpack-entries",
    "zset-max-listpack-value",
    "hll-sparse-max-bytes",
    // Monitoring
    "lua-time-limit",
    "slowlog-log-slower-than",
    "slowlog-max-len",
    "latency-monitor-threshold",
    "watchdog-period",
    "notify-keyspace-events",
    // Other
    "activerehashing",
    "hz",
    "loglevel",
];

// ---- state -----------------------------------------------------------------------------------

struct Item {
    name: String,
    kind: Kind,
    writeable: bool,
    /// What the config file (as far as we know) holds: the value at load time, or after a save.
    orig: String,
    /// What the running server holds: the value at load time, or after the last Apply.
    applied: String,
    /// What the form shows.
    value: String,
}

impl Item {
    fn apply_dirty(&self) -> bool {
        self.value != self.applied
    }

    fn rewrite_dirty(&self) -> bool {
        self.value != self.orig
    }
}

struct Group {
    name: String,
    items: Vec<Item>,
}

/// An Apply (or Save) in flight.
struct Job {
    save: bool,
    /// Outstanding `CONFIG SET` requests: id → (name, value sent).
    pending: HashMap<ReqId, (String, String)>,
    first_error: Option<String>,
    /// The `CONFIG REWRITE` request, once sent.
    rewrite: Option<ReqId>,
}

/// State of this module, stored in `Instance::config`. Reset (`Default`) on every connect.
#[derive(Default)]
pub struct ConfigState {
    requested: bool,
    loading: Option<ReqId>,
    error: Option<String>,
    groups: Vec<Group>,
    job: Option<Job>,
}

impl ConfigState {
    fn busy(&self) -> bool {
        self.loading.is_some() || self.job.is_some()
    }

    fn any_apply_dirty(&self) -> bool {
        self.groups.iter().flat_map(|g| &g.items).any(Item::apply_dirty)
    }

    fn any_rewrite_dirty(&self) -> bool {
        self.groups.iter().flat_map(|g| &g.items).any(Item::rewrite_dirty)
    }
}

fn exec_args(args: &[&str]) -> Op {
    Op::Exec(args.iter().map(|a| a.as_bytes().to_vec()).collect())
}

fn reload(inst: &mut Instance) {
    inst.config.requested = true;
    inst.config.error = None;
    inst.config.loading = Some(inst.send(Module::Config, exec_args(&["CONFIG", "GET", "*"])));
}

/// Sends one `CONFIG SET` per dirty name. With `save`, `CONFIG REWRITE` follows once they succeed.
fn start_apply(inst: &mut Instance, save: bool) {
    let dirty: Vec<(String, String)> = inst
        .config
        .groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| i.apply_dirty())
        .map(|i| (i.name.clone(), i.value.clone()))
        .collect();
    let mut job = Job { save, pending: HashMap::new(), first_error: None, rewrite: None };
    for (name, value) in dirty {
        let id = inst.send(Module::Config, exec_args(&["CONFIG", "SET", &name, &value]));
        job.pending.insert(id, (name, value));
    }
    let nothing_to_set = job.pending.is_empty();
    inst.config.job = Some(job);
    if nothing_to_set {
        if save {
            finish_apply(inst);
        } else {
            inst.config.job = None;
        }
    }
}

/// All `CONFIG SET` answers of a Save are in and none failed: send `CONFIG REWRITE`.
fn finish_apply(inst: &mut Instance) {
    let id = inst.send(Module::Config, exec_args(&["CONFIG", "REWRITE"]));
    if let Some(job) = inst.config.job.as_mut() {
        job.rewrite = Some(id);
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::BulkString(b) => display_bytes(b),
        Value::SimpleString(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Okay => "OK".into(),
        Value::Nil => String::new(),
        other => format!("{other:?}"),
    }
}

fn parse_pairs(v: &Value) -> Vec<(String, String)> {
    match v {
        Value::Array(items) => items.as_chunks::<2>().0.iter().map(|[k, v]| (value_text(k), value_text(v))).collect(),
        Value::Map(pairs) => pairs.iter().map(|(k, v)| (value_text(k), value_text(v))).collect(),
        _ => Vec::new(),
    }
}

fn build_groups(pairs: Vec<(String, String)>) -> Vec<Group> {
    let mut by_name: HashMap<String, String> = HashMap::new();
    let order: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
    for (k, v) in pairs {
        by_name.insert(k, v);
    }
    let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let make = |name: &str, kind: Kind, value: String| Item {
        name: name.to_owned(),
        kind,
        writeable: WRITEABLE.contains(&name),
        orig: value.clone(),
        applied: value.clone(),
        value,
    };
    let mut groups = Vec::new();
    for (gname, defs) in GROUPS {
        let items: Vec<Item> = defs
            .iter()
            .filter_map(|(name, kind)| {
                by_name.get(*name).map(|v| {
                    used.insert(*name);
                    make(name, *kind, v.clone())
                })
            })
            .collect();
        if !items.is_empty() {
            groups.push(Group { name: (*gname).to_owned(), items });
        }
    }
    let other: Vec<Item> = order
        .iter()
        .filter(|n| !used.contains(n.as_str()))
        .filter_map(|n| by_name.get(n).map(|v| make(n, Kind::Text, v.clone())))
        .collect();
    if !other.is_empty() {
        groups.push(Group { name: "Other".into(), items: other });
    }
    groups
}

// ---- UI --------------------------------------------------------------------------------------

/// Draws the module into `ui` (the region the app shell gave it). Called every frame while the
/// region is visible.
pub fn show(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) {
    if !inst.config.requested {
        reload(inst);
    }
    let pal = cx.pal;
    let busy = inst.config.busy();
    let can_apply = !busy && inst.config.any_apply_dirty();
    let can_save = !busy && inst.config.any_rewrite_dirty();
    let dirty = inst.config.any_apply_dirty() || inst.config.any_rewrite_dirty();
    let mut do_reload = false;
    let mut do_apply = false;
    let mut do_save = false;

    egui::Panel::bottom("config-buttons")
        .exact_size(48.0)
        .show_separator_line(true)
        .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin::symmetric(0, 8)))
        .show(ui, |ui| {
            column(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(can_apply, primary_button(&pal, "Apply")).clicked() {
                        do_apply = true;
                    }
                    if ui.add_enabled(can_save, egui::Button::new("Save To Config File")).clicked() {
                        do_save = true;
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Reload")).clicked() {
                        do_reload = true;
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });
        });

    ScrollArea::vertical().id_salt("config-scroll").auto_shrink(false).show(ui, |ui| {
        let w = ui.available_width();
        let pad = ((w - COLUMN_W) / 2.0).max(12.0);
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(COLUMN_W);
                body(ui, inst, &pal);
            });
        });
    });

    if do_reload {
        if dirty {
            cx.modal(
                ModalSpec::confirm(
                    "Reload config?",
                    "Reload",
                    "Are you sure you want to reload the config? Your changes will be lost if you reload the config.",
                ),
                |inst, _cx, out| {
                    if out.is_ok() {
                        reload(inst);
                    }
                },
            );
        } else {
            reload(inst);
        }
    }
    if do_apply {
        cx.modal(
            ModalSpec::confirm(
                "Apply the changes",
                "Apply",
                "Are you sure you want to apply the changes? The changes are only valid for the current session and will be lost when Redis restarts.",
            ),
            |inst, _cx, out| {
                if out.is_ok() {
                    start_apply(inst, false);
                }
            },
        );
    }
    if do_save {
        cx.modal(
            ModalSpec::confirm(
                "Save the changes",
                "Save",
                "Are you sure you want to apply the changes and save the changes to the config file?",
            ),
            |inst, _cx, out| {
                if out.is_ok() {
                    start_apply(inst, true);
                }
            },
        );
    }
}

/// Runs `add` in a centred column of the settings width.
fn column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let pad = ((ui.available_width() - COLUMN_W) / 2.0).max(12.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.allocate_ui_with_layout(Vec2::new(COLUMN_W, ui.available_height()), Layout::top_down(Align::Min), add);
    });
}

fn body(ui: &mut Ui, inst: &mut Instance, pal: &Palette) {
    let st = &mut inst.config;
    if let Some(err) = &st.error {
        ui.add_space(20.0);
        ui.label(RichText::new(err.as_str()).color(pal.danger));
        ui.label(RichText::new("Press Reload to try again.").color(pal.text_dim).size(12.0));
        return;
    }
    if st.groups.is_empty() {
        ui.add_space(30.0);
        if st.loading.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading config...").color(pal.text_dim));
            });
        } else {
            ui.label(RichText::new("No settings to show.").color(pal.text_faint));
        }
        return;
    }
    let locked = st.job.is_some() || st.loading.is_some();
    for g in &mut st.groups {
        ui.add_space(20.0);
        ui.label(RichText::new(g.name.as_str()).size(15.0).strong().color(pal.text));
        ui.add_space(2.0);
        ui.separator();
        for item in &mut g.items {
            row(ui, item, pal, locked);
        }
    }
    ui.add_space(20.0);
}

fn row(ui: &mut Ui, item: &mut Item, pal: &Palette, locked: bool) {
    let dirty = item.apply_dirty() || item.rewrite_dirty();
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(LABEL_W, 22.0), Layout::right_to_left(Align::Center), |ui| {
            let col = if dirty {
                pal.accent
            } else if item.writeable {
                pal.text
            } else {
                pal.text_dim
            };
            let mut t = RichText::new(item.name.as_str()).size(12.0).color(col);
            if dirty {
                t = t.strong();
            }
            ui.add(egui::Label::new(t).truncate()).on_hover_text(item.name.as_str());
        });
        ui.add_enabled_ui(item.writeable && !locked, |ui| control(ui, item));
    });
}

fn control(ui: &mut Ui, item: &mut Item) {
    let kind = item.kind;
    let is_yes_no = item.value == "yes" || item.value == "no";
    match kind {
        Kind::Bool if is_yes_no => {
            let mut on = item.value == "yes";
            if ui.checkbox(&mut on, "").changed() {
                item.value = if on { "yes" } else { "no" }.into();
            }
        }
        Kind::Num if item.value.parse::<i64>().is_ok() => {
            let mut n: i64 = item.value.parse().unwrap_or(0);
            if ui.add(DragValue::new(&mut n).speed(1.0)).changed() {
                item.value = n.to_string();
            }
        }
        Kind::Enum(options) if options.contains(&item.value.as_str()) => {
            egui::ComboBox::from_id_salt(("config-enum", item.name.as_str()))
                .selected_text(item.value.clone())
                .width(ui.available_width().min(200.0))
                .show_ui(ui, |ui| {
                    for o in options {
                        ui.selectable_value(&mut item.value, (*o).to_owned(), *o);
                    }
                });
        }
        _ => {
            ui.add(TextEdit::singleline(&mut item.value).desired_width(ui.available_width()).text_color(text_col(ui)));
        }
    }
}

fn text_col(ui: &Ui) -> Color32 {
    ui.visuals().text_color()
}

// ---- events ----------------------------------------------------------------------------------

/// Receives worker events. Called for replies to requests this module sent (`Module::Config`),
/// for every `ScanBatch` of its scans, and for every unsolicited event (`DbSelected`, `PubSub`,
/// `Monitor`, `StreamEnded`, `Connected`) whether or not the module is visible.
pub fn handle_evt(inst: &mut Instance, cx: &mut Cx, evt: &Evt) {
    let Evt::Reply { id, result } = evt else { return };
    let id = *id;

    // CONFIG GET *
    if inst.config.loading == Some(id) {
        inst.config.loading = None;
        match result {
            Ok(Reply::Value(v)) => {
                inst.config.groups = build_groups(parse_pairs(v));
                inst.config.error = None;
            }
            Ok(_) => inst.config.error = Some("Unexpected reply to CONFIG GET".into()),
            Err(e) => {
                inst.config.groups.clear();
                inst.config.error = Some(e.message.clone());
            }
        }
        return;
    }

    // CONFIG SET replies
    let st = &mut inst.config;
    if let Some(job) = st.job.as_mut()
        && let Some((name, value)) = job.pending.remove(&id)
    {
        match result {
            Ok(_) => {
                if let Some(item) = st.groups.iter_mut().flat_map(|g| &mut g.items).find(|i| i.name == name) {
                    item.applied = value;
                }
            }
            Err(e) => {
                job.first_error.get_or_insert_with(|| e.message.clone());
            }
        }
        if job.pending.is_empty() {
            if let Some(msg) = job.first_error.clone() {
                st.job = None;
                cx.alert(msg);
            } else if job.save {
                finish_apply(inst);
            } else {
                st.job = None;
            }
        }
        return;
    }

    // CONFIG REWRITE reply
    if st.job.as_ref().is_some_and(|j| j.rewrite == Some(id)) {
        st.job = None;
        match result {
            Ok(_) => {
                for item in st.groups.iter_mut().flat_map(|g| &mut g.items) {
                    item.orig = item.applied.clone();
                }
            }
            Err(e) => cx.alert(e.message.clone()),
        }
    }
}
