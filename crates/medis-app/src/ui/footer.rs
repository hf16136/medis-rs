//! Footers.
//!
//! * [`show`] is the bar at the bottom of the right pane (SPEC-3 8.1): `Members/Bytes: N`,
//!   `Encoding: ..`, `TTL: ..` for the selected key. It refreshes when `inst.selected_key` or
//!   `inst.content_version` changes.
//! * [`browser_footer`] is the bar under the key list (SPEC-2 4.11): `Keys: N` on the left and the
//!   `DB:` selector on the right. The key browser calls it. This module also owns the `INFO`
//!   polling (on connect, on db change, after key changes and every 10 s) and the one-time
//!   database count.

use std::collections::HashMap;
use std::time::Duration;

use egui::{Align, Layout, Pos2, RichText, Sense, Stroke, UiBuilder, Vec2};
use medis_core::redis::Value;
use medis_core::worker::{Op, Reply};
use medis_core::{Evt, ReqId, format_ttl, ops};

use crate::instance::{Cx, Instance, Module};
use crate::ui::keys;

/// Height of the key browser bar.
pub const BAR_HEIGHT: f32 = 26.0;
const POLL_SECS: f64 = 10.0;

/// State of this module, stored in `Instance::footer`. Reset (`Default`) on every connect.
#[derive(Default)]
pub struct FooterState {
    // ---- key browser footer ----
    /// The last parsed `INFO`.
    info: HashMap<String, String>,
    info_loaded: bool,
    info_inflight: bool,
    info_req: Option<ReqId>,
    next_poll_at: f64,
    /// Number of databases once known.
    db_count: Option<u32>,
    count_sent: bool,
    last_db: Option<u32>,
    last_version: u64,

    // ---- content footer ----
    cf_key: Option<String>,
    cf_version: u64,
    cf_open: Option<ReqId>,
    cf_pttl: Option<ReqId>,
    cf_enc: Option<ReqId>,
    cf_type: String,
    cf_len: Option<u64>,
    cf_ttl_ms: Option<i64>,
    cf_encoding: String,
}

/// The content footer: size, encoding and TTL of the selected key.
pub fn show(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    tick(inst, cx);
    refresh_content(inst);

    let pal = cx.pal;
    let st = &inst.footer;
    if inst.selected_key.is_none() || st.cf_type == "none" {
        return;
    }
    ui.spacing_mut().item_spacing.x = 0.0;
    let text = |s: String| RichText::new(s).size(12.0).color(pal.text_dim);
    if let Some(n) = st.cf_len.filter(|n| *n > 0) {
        let unit = if st.cf_type == "string" { "Bytes" } else { "Members" };
        ui.add_space(5.0);
        ui.label(text(format!("{unit}: {n}")));
        ui.add_space(5.0);
    }
    if !st.cf_encoding.is_empty() {
        ui.add_space(5.0);
        ui.label(text(format!("Encoding: {}", st.cf_encoding)));
        ui.add_space(5.0);
    }
    if let Some(ms) = st.cf_ttl_ms.filter(|ms| *ms >= 0) {
        ui.add_space(5.0);
        ui.label(text(format!("TTL: {}", format_ttl(ms))));
        ui.add_space(5.0);
    }
}

/// Receives worker events for this module's requests.
pub fn handle_evt(inst: &mut Instance, _cx: &mut Cx, evt: &Evt) {
    let Evt::Reply { id, result } = evt else { return };
    let st = &mut inst.footer;
    let id = Some(*id);

    if id == st.cf_open {
        match result {
            Ok(Reply::KeyOpened { ktype, len }) => {
                st.cf_type = ktype.clone();
                st.cf_len = *len;
            }
            _ => {
                st.cf_type.clear();
                st.cf_len = None;
            }
        }
        return;
    }
    if id == st.cf_pttl {
        st.cf_ttl_ms = match result {
            Ok(Reply::Int(n)) => Some(*n),
            _ => None,
        };
        return;
    }
    if id == st.cf_enc {
        st.cf_encoding = match result {
            Ok(Reply::Value(Value::BulkString(b))) => String::from_utf8_lossy(b).into_owned(),
            Ok(Reply::Value(Value::SimpleString(s))) => s.clone(),
            _ => String::new(),
        };
        return;
    }

    match result {
        Ok(Reply::Info(map)) => {
            st.info = map.clone();
            st.info_loaded = true;
            st.info_inflight = false;
        }
        Ok(Reply::DbCount(n)) => st.db_count = Some((*n).max(1)),
        Ok(_) => {}
        Err(_) => {
            // A failed poll is not worth an alert; the next poll tries again.
            if st.info_req == id {
                st.info_inflight = false;
            }
        }
    }
}

/// Re-fetches the content footer when the key or the content version changed.
fn refresh_content(inst: &mut Instance) {
    let key_changed = inst.footer.cf_key != inst.selected_key;
    if !key_changed && inst.footer.cf_version == inst.content_version {
        return;
    }
    let key = inst.selected_key.clone();
    let version = inst.content_version;
    if key_changed {
        let st = &mut inst.footer;
        st.cf_type.clear();
        st.cf_len = None;
        st.cf_ttl_ms = None;
        st.cf_encoding.clear();
    }
    let ids = key.as_ref().map(|k| {
        let args = vec![b"OBJECT".to_vec(), b"ENCODING".to_vec(), k.as_bytes().to_vec()];
        (
            inst.send(Module::Footer, Op::OpenKey(k.clone())),
            inst.send(Module::Footer, Op::Pttl(k.clone())),
            inst.send(Module::Footer, Op::Exec(args)),
        )
    });
    let st = &mut inst.footer;
    st.cf_key = key;
    st.cf_version = version;
    (st.cf_open, st.cf_pttl, st.cf_enc) = match ids {
        Some((a, b, c)) => (Some(a), Some(b), Some(c)),
        None => (None, None, None),
    };
}

/// Sends the `INFO` / db-count requests when they are due.
fn tick(inst: &mut Instance, cx: &mut Cx) {
    let now = cx.ctx.input(|i| i.time);

    if !inst.footer.count_sent {
        inst.footer.count_sent = true;
        if inst.is_cluster() {
            inst.footer.db_count = Some(1);
        } else {
            inst.send(Module::Footer, Op::DbCount);
        }
    }

    let db_changed = inst.footer.last_db != Some(inst.db);
    let version_changed = inst.footer.last_version != inst.content_version;
    let due = now >= inst.footer.next_poll_at;
    let wanted = db_changed || version_changed || due;
    if wanted && !inst.footer.info_inflight {
        let id = inst.send(Module::Footer, Op::Info);
        let st = &mut inst.footer;
        st.info_req = Some(id);
        st.info_inflight = true;
        st.next_poll_at = now + POLL_SECS;
        st.last_db = Some(inst.db);
        st.last_version = inst.content_version;
    }

    let wait = (inst.footer.next_poll_at - now).clamp(0.05, POLL_SECS);
    cx.ctx.request_repaint_after(Duration::from_secs_f64(wait));
}

/// Draws the key browser bar: `Keys: N` on the left, `DB:` and the selector on the right.
pub fn browser_footer(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    tick(inst, cx);
    let pal = cx.pal;

    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), BAR_HEIGHT), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, pal.toolbar);
    ui.painter().line_segment([rect.left_top(), Pos2::new(rect.right(), rect.top())], Stroke::new(1.0, pal.border));

    let inner = rect.shrink2(Vec2::new(10.0, 0.0));
    let mut bar = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut bar;

    let count = if inst.footer.info_loaded {
        ops::db_key_count(&inst.footer.info, inst.db).to_string()
    } else {
        "-".into()
    };
    ui.label(RichText::new(format!("Keys: {count}")).size(12.0).color(pal.text_dim));

    if inst.is_cluster() {
        return;
    }

    let mut chosen: Option<u32> = None;
    let busy = keys::db_switch_pending(inst);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // Right to left: the selector first, then its label.
        let total = inst.footer.db_count.unwrap_or(1).max(inst.db + 1);
        let selected = ops::db_label(&inst.footer.info, inst.db);
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt(("db-select", inst.id))
                .selected_text(RichText::new(selected).size(12.0))
                .width(96.0)
                .height(320.0)
                .show_ui(ui, |ui| {
                    for n in 0..total {
                        let label = ops::db_label(&inst.footer.info, n);
                        if ui.selectable_label(inst.db == n, label).clicked() {
                            chosen = Some(n);
                        }
                    }
                });
        });
        ui.label(RichText::new("DB:").size(12.0).color(pal.text_dim));
    });

    if let Some(n) = chosen {
        keys::change_db(inst, n);
    }
}
