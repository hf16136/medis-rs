//! Content tab: the type dispatch (SPEC-2 5.0) plus one sub-view per type in `ui/content/*.rs`.
//!
//! CONTRACT (fixed by the orchestrator; the sub-view lanes fill in `content/<type>.rs`):
//! * When `inst.selected_key` changes, this module sends `Op::OpenKey` and, on
//!   `Reply::KeyOpened { ktype, len }`, builds the sub-view with `<type>::State::new(key, len)`.
//!   It then calls `<type>::start(&mut state, inst, cx)` once, which sends the first page request.
//! * Every frame: `<type>::show(&mut state, ui, inst, cx)`.
//! * Replies: sub-views send with `inst.send(Module::Content, op)` and remember the ids. Every
//!   `Evt::Reply` routed to Content that is not the OpenKey reply goes to
//!   `<type>::on_reply(&mut state, inst, cx, id, &result)`. The sub-view ignores ids it does not
//!   know.
//! * The sub-view state is taken out of `inst.content` while it is called (`std::mem::take`), so
//!   a sub-view can freely use `inst.send`, `inst.editor`, `inst.content_version`, ...
//! * Shared helpers for sub-views are in `content/common.rs`: the split layout with the persisted
//!   width, the delete-item confirm, and the re-selection maths.
//! * Changing the key (or a db switch / reconnect) drops the sub-view; stale replies are ignored.

#[path = "content/common.rs"]
pub mod common;
#[path = "content/hash.rs"]
mod hash;
#[path = "content/list.rs"]
mod list;
#[path = "content/set.rs"]
mod set;
#[path = "content/string.rs"]
mod string;
#[path = "content/zset.rs"]
mod zset;

use egui::{Align, Layout, RichText};
use medis_core::worker::Op;
use medis_core::{CoreError, Evt, Reply, ReqId};

use crate::instance::{Cx, Instance, Module};
use crate::ui::editor;

/// What the pane is showing.
#[derive(Default)]
pub enum View {
    /// No key selected.
    #[default]
    Empty,
    /// TYPE request in flight.
    Loading,
    /// `TYPE` said `none`.
    Deleted,
    /// stream or some other type.
    Unsupported(String),
    String(string::State),
    Hash(hash::State),
    List(list::State),
    Set(set::State),
    ZSet(zset::State),
}

/// State of this module, stored in `Instance::content`. Reset (`Default`) on every connect.
#[derive(Default)]
pub struct ContentState {
    /// Key the current view belongs to.
    key: Option<String>,
    /// db the current view belongs to.
    db: u32,
    /// Pending `OpenKey` request.
    open_req: Option<ReqId>,
    view: View,
}

fn open(inst: &mut Instance, key: Option<String>) {
    editor::load(&mut inst.editor, None);
    inst.content.key = key.clone();
    inst.content.db = inst.db;
    inst.content.open_req = None;
    inst.content.view = match key {
        None => View::Empty,
        Some(k) => {
            let id = inst.send(Module::Content, Op::OpenKey(k));
            inst.content.open_req = Some(id);
            View::Loading
        }
    };
}

/// Draws the content pane.
pub fn show(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    if inst.content.key != inst.selected_key || inst.content.db != inst.db {
        let key = inst.selected_key.clone();
        open(inst, key);
    }

    let mut view = std::mem::take(&mut inst.content.view);
    match &mut view {
        View::Empty => {}
        View::Loading => {
            ui.centered_and_justified(|ui| ui.spinner());
        }
        View::Deleted => {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space((ui.available_height() / 2.0 - 60.0).max(0.0));
                ui.label(RichText::new("🗑").size(62.0).color(cx.pal.text_faint));
                ui.label(RichText::new("The key has been deleted").size(22.0).color(cx.pal.text_faint));
            });
        }
        View::Unsupported(t) => {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(format!("Unsupported type: {t}")).color(cx.pal.text_faint));
            });
        }
        View::String(s) => string::show(s, ui, inst, cx),
        View::Hash(s) => hash::show(s, ui, inst, cx),
        View::List(s) => list::show(s, ui, inst, cx),
        View::Set(s) => set::show(s, ui, inst, cx),
        View::ZSet(s) => zset::show(s, ui, inst, cx),
    }
    // A sub-view may have reset the view (e.g. via `reload`); keep it only if nothing replaced it.
    if matches!(inst.content.view, View::Empty) {
        inst.content.view = view;
    }
}

/// Receives worker events routed to Content.
pub fn handle_evt(inst: &mut Instance, cx: &mut Cx, evt: &Evt) {
    match evt {
        Evt::Reply { id, result } if Some(*id) == inst.content.open_req => {
            inst.content.open_req = None;
            let Some(key) = inst.content.key.clone() else { return };
            inst.content.view = match result {
                Ok(Reply::KeyOpened { ktype, len }) => {
                    let len = len.unwrap_or(0);
                    match ktype.as_str() {
                        "string" => View::String(string::State::new(key, len)),
                        "hash" => View::Hash(hash::State::new(key, len)),
                        "list" => View::List(list::State::new(key, len)),
                        "set" => View::Set(set::State::new(key, len)),
                        "zset" => View::ZSet(zset::State::new(key, len)),
                        "none" => View::Deleted,
                        other => View::Unsupported(other.to_string()),
                    }
                }
                Ok(_) => View::Empty,
                Err(e) => {
                    cx.alert(e.to_string());
                    View::Empty
                }
            };
            let mut view = std::mem::take(&mut inst.content.view);
            match &mut view {
                View::String(s) => string::start(s, inst, cx),
                View::Hash(s) => hash::start(s, inst, cx),
                View::List(s) => list::start(s, inst, cx),
                View::Set(s) => set::start(s, inst, cx),
                View::ZSet(s) => zset::start(s, inst, cx),
                _ => {}
            }
            inst.content.view = view;
        }
        Evt::Reply { id, result } => dispatch_reply(inst, cx, *id, result),
        _ => {}
    }
}

fn dispatch_reply(inst: &mut Instance, cx: &mut Cx, id: ReqId, result: &Result<Reply, CoreError>) {
    let mut view = std::mem::take(&mut inst.content.view);
    match &mut view {
        View::String(s) => string::on_reply(s, inst, cx, id, result),
        View::Hash(s) => hash::on_reply(s, inst, cx, id, result),
        View::List(s) => list::on_reply(s, inst, cx, id, result),
        View::Set(s) => set::on_reply(s, inst, cx, id, result),
        View::ZSet(s) => zset::on_reply(s, inst, cx, id, result),
        _ => {}
    }
    if matches!(inst.content.view, View::Empty) {
        inst.content.view = view;
    }
}
