//! Hash view (SPEC-2 5.0, 5.2, 5.8): a virtualized one-column table of fields on the left, the
//! value editor on the right.
//!
//! * Pages come from `HSCAN` (cursor based). They are de-duplicated by field name and the scan is
//!   never restarted once the cursor has come back to 0.
//! * The table draws `len` rows; rows that are not loaded yet show `Loading...`. The next page is
//!   requested while the deepest visible row is not loaded.
//! * All write requests are tracked in `pending` and finished in `on_reply`, which looks the row
//!   up by field name (row indexes can shift while a request is in flight).

use std::collections::{HashMap, HashSet};

use egui::text::{CCursor, CCursorRange};
use egui::{Align, Color32, Key, Label, Layout, Modifiers, Rect, RichText, Sense, TextEdit};
use egui_extras::{Column, TableBuilder};
use medis_core::worker::Op;
use medis_core::{CoreError, Reply, ReqId};

use super::common;
use crate::instance::{Cx, Instance, Module};
use crate::ui::content::View;
use crate::ui::modal::{Field, ModalSpec};
use crate::ui::{editor, widgets};

/// Row height of the table (SPEC-2 "Numbers").
const ROW_HEIGHT: f32 = 24.0;

/// A write request in flight.
enum Pending {
    Add { field: String },
    Save { field: String, value: Vec<u8> },
    Delete { field: String },
    Exists { src: String, target: String, value: Vec<u8> },
    Rename { src: String, target: String, overwrite: bool },
}

/// Something the user asked for during the frame; applied after the layout pass.
enum Act {
    Select(usize),
    StartRename(usize),
    CommitRename { row: usize, new: String },
    Delete(usize),
    Add,
}

/// The inline rename in progress.
struct Edit {
    row: usize,
    text: String,
    /// First frame: grab the focus and select all.
    first: bool,
}

enum EditEnd {
    Commit,
    Cancel,
}

pub struct State {
    pub key: String,
    pub len: u64,
    items: Vec<(String, Vec<u8>)>,
    seen: HashSet<String>,
    cursor: u64,
    /// The scan came back to cursor 0.
    done: bool,
    /// A page request failed; stop asking.
    failed: bool,
    page_req: Option<ReqId>,
    sel: Option<usize>,
    /// Deepest row drawn in the last frame.
    max_row: usize,
    scroll_to: Option<usize>,
    /// The table was the last thing clicked (so the arrow keys are ours).
    active: bool,
    editing: Option<Edit>,
    pending: HashMap<ReqId, Pending>,
}

impl State {
    pub fn new(key: String, len: u64) -> Self {
        Self {
            key,
            len,
            items: Vec::new(),
            seen: HashSet::new(),
            cursor: 0,
            done: false,
            failed: false,
            page_req: None,
            sel: None,
            max_row: 0,
            scroll_to: None,
            active: true,
            editing: None,
            pending: HashMap::new(),
        }
    }

    fn send(&mut self, inst: &mut Instance, op: Op, pending: Pending) {
        let id = inst.send(Module::Content, op);
        self.pending.insert(id, pending);
    }

    fn request_page(&mut self, inst: &mut Instance) {
        let op = Op::HashPage { key: self.key.clone(), cursor: self.cursor };
        self.page_req = Some(inst.send(Module::Content, op));
    }

    fn pos(&self, field: &str) -> Option<usize> {
        self.items.iter().position(|(f, _)| f == field)
    }

    /// Selects a loaded row and shows its value in the editor.
    fn select(&mut self, inst: &mut Instance, row: usize) {
        if let Some((_, v)) = self.items.get(row) {
            self.sel = Some(row);
            editor::load(&mut inst.editor, Some(v.clone()));
        }
    }

    fn clear_selection(&mut self, inst: &mut Instance) {
        self.sel = None;
        editor::load(&mut inst.editor, None);
    }

    fn ask_delete(&mut self, cx: &mut Cx, row: usize) {
        let Some((field, _)) = self.items.get(row) else { return };
        let (key, field) = (self.key.clone(), field.clone());
        common::confirm_delete(cx, move |inst, _cx| {
            with_state(inst, move |st, inst| {
                if st.key == key {
                    let op = Op::HashDelete { key, field: field.clone() };
                    st.send(inst, op, Pending::Delete { field });
                }
            });
        });
    }

    fn ask_add(&mut self, cx: &mut Cx) {
        let key = self.key.clone();
        let spec = ModalSpec::form("Insert Member", "Insert Member", "", vec![Field::text("Key:")]);
        cx.modal(spec, move |inst, _cx, out| {
            if !out.is_ok() {
                return;
            }
            let field = out.text(0);
            with_state(inst, move |st, inst| {
                if st.key == key {
                    let op = Op::HashAdd { key, field: field.clone() };
                    st.send(inst, op, Pending::Add { field });
                }
            });
        });
    }

    fn save(&mut self, inst: &mut Instance, cx: &mut Cx, bytes: Vec<u8>) {
        let Some(i) = self.sel.filter(|i| *i < self.items.len()) else {
            cx.alert(common::WAIT_FOR_LOAD);
            return;
        };
        let field = self.items[i].0.clone();
        let op = Op::HashSet { key: self.key.clone(), field: field.clone(), value: bytes.clone() };
        self.send(inst, op, Pending::Save { field, value: bytes });
    }

    fn send_rename(&mut self, inst: &mut Instance, src: String, target: String, value: Vec<u8>, overwrite: bool) {
        let op = Op::HashRename { key: self.key.clone(), src: src.clone(), target: target.clone(), value };
        self.send(inst, op, Pending::Rename { src, target, overwrite });
    }

    /// Up / Down / Backspace / Delete, only when the table is the focus and no text is edited.
    fn keys(&mut self, cx: &Cx, acts: &mut Vec<Act>) {
        if cx.blocked || !self.active || self.editing.is_some() || cx.ctx.egui_wants_keyboard_input() {
            return;
        }
        let (up, down, del) = cx.ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Backspace) | i.consume_key(Modifiers::NONE, Key::Delete),
            )
        });
        let cur = self.sel.filter(|i| *i < self.items.len());
        if up && let Some(i) = cur.filter(|i| *i > 0) {
            self.scroll_to = Some(i - 1);
            acts.push(Act::Select(i - 1));
        }
        if down && let Some(i) = cur.filter(|i| i + 1 < self.items.len()) {
            self.scroll_to = Some(i + 1);
            acts.push(Act::Select(i + 1));
        }
        if del && let Some(i) = cur {
            acts.push(Act::Delete(i));
        }
    }

    /// The left pane: header with the AddButton and the virtualized table.
    fn table(&mut self, ui: &mut egui::Ui, cx: &mut Cx, inst_id: u64, acts: &mut Vec<Act>) {
        let pal = cx.pal;
        let total = self.len as usize;
        self.max_row = 0;
        let scroll_to = self.scroll_to.take();
        let Self { items, editing, sel, max_row, .. } = self;

        common::tight_rows(ui);
        let mut builder = TableBuilder::new(ui)
            .id_salt(("hash-table", inst_id))
            .sense(Sense::click())
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().clip(true));
        if let Some(r) = scroll_to {
            builder = builder.scroll_to_row(r, None);
        }
        builder
            .header(22.0, |mut h| {
                h.col(|ui| {
                    common::cell_pad(ui);
                    ui.label(RichText::new("key").color(pal.text_dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(6.0);
                        if widgets::add_button(ui, &pal).clicked() {
                            acts.push(Act::Add);
                        }
                    });
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, total, |mut row| {
                    let i = row.index();
                    *max_row = (*max_row).max(i);
                    row.set_selected(*sel == Some(i));
                    let loaded = i < items.len();
                    let is_editing = loaded && editing.as_ref().is_some_and(|e| e.row == i);
                    let mut end = None;
                    row.col(|ui| {
                        common::cell_pad(ui);
                        if !loaded {
                            ui.add(Label::new(RichText::new("Loading...").color(pal.text_faint)).selectable(false));
                        } else if is_editing {
                            if let Some(e) = editing.as_mut() {
                                end = edit_ui(ui, e);
                            }
                        } else {
                            ui.add(Label::new(RichText::new(&items[i].0).monospace().size(12.0).color(pal.text)).truncate().selectable(false));
                        }
                    });
                    if !loaded {
                        return;
                    }
                    match end {
                        Some(EditEnd::Commit) => {
                            if let Some(e) = editing.take() {
                                acts.push(Act::CommitRename { row: e.row, new: e.text });
                            }
                        }
                        Some(EditEnd::Cancel) => *editing = None,
                        None => {}
                    }
                    if is_editing {
                        return;
                    }
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() || resp.double_clicked() {
                        acts.push(Act::Select(i));
                    }
                    if resp.double_clicked() {
                        acts.push(Act::StartRename(i));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Copy to Clipboard").clicked() {
                            ui.ctx().copy_text(items[i].0.clone());
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Rename Key").clicked() {
                            acts.push(Act::Select(i));
                            acts.push(Act::StartRename(i));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            acts.push(Act::Select(i));
                            acts.push(Act::Delete(i));
                            ui.close();
                        }
                    });
                });
            });
    }
}

/// The inline rename input (SPEC-2 5.8): white background, `#333` text; Enter or blur commits,
/// Escape reverts.
fn edit_ui(ui: &mut egui::Ui, e: &mut Edit) -> Option<EditEnd> {
    let id = egui::Id::unique(("hash-rename", e.row));
    let mut out = TextEdit::singleline(&mut e.text)
        .id(id)
        .desired_width(f32::INFINITY)
        .text_color(Color32::from_rgb(0x33, 0x33, 0x33))
        .background_color(Color32::WHITE)
        .show(ui);
    if e.first {
        e.first = false;
        out.response.request_focus();
        let n = e.text.chars().count();
        out.state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        out.state.store(ui.ctx(), id);
        return None;
    }
    let esc = ui.input(|i| i.key_pressed(Key::Escape));
    if esc && (out.response.has_focus() || out.response.lost_focus()) {
        return Some(EditEnd::Cancel);
    }
    if out.response.lost_focus() {
        return Some(EditEnd::Commit);
    }
    None
}

/// Runs `f` on the hash state of the current content view (from a modal callback). Does nothing
/// when the pane shows something else by then.
fn with_state(inst: &mut Instance, f: impl FnOnce(&mut State, &mut Instance)) {
    let mut view = std::mem::take(&mut inst.content.view);
    if let View::Hash(st) = &mut view {
        f(st, inst);
    }
    if matches!(inst.content.view, View::Empty) {
        inst.content.view = view;
    }
}

/// Sends the first load request(s).
pub fn start(st: &mut State, inst: &mut Instance, cx: &mut Cx) {
    let _ = cx;
    if st.len == 0 {
        st.done = true;
        return;
    }
    st.request_page(inst);
}

/// Draws the view.
pub fn show(st: &mut State, ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    let mut acts = Vec::new();
    st.keys(cx, &mut acts);

    if !st.done && !st.failed && st.page_req.is_none() && st.max_row >= st.items.len() {
        st.request_page(inst);
    }

    let mut saved = None;
    let mut left_rect = Rect::NOTHING;
    let inst_id = inst.id;
    common::split(
        ui,
        cx,
        inst_id,
        |ui, cx| {
            left_rect = ui.max_rect();
            st.table(ui, cx, inst_id, &mut acts);
        },
        |ui, cx| {
            saved = editor::show(ui, inst, cx);
        },
    );

    // The arrow keys belong to whatever was clicked last.
    if let Some(p) = cx.ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }) {
        st.active = left_rect.contains(p);
    }

    // The save belongs to the row selected while the editor was drawn: handle it before any
    // selection change of this frame.
    if let Some(bytes) = saved {
        st.save(inst, cx, bytes);
    }
    for act in acts {
        match act {
            Act::Select(row) => {
                if st.sel != Some(row) {
                    st.select(inst, row);
                }
            }
            Act::StartRename(row) => {
                if let Some((f, _)) = st.items.get(row) {
                    st.editing = Some(Edit { row, text: f.clone(), first: true });
                }
            }
            Act::CommitRename { row, new } => {
                let Some((old, value)) = st.items.get(row).cloned() else { continue };
                if new.is_empty() || new == old {
                    continue;
                }
                let op = Op::HashExists { key: st.key.clone(), field: new.clone() };
                st.send(inst, op, Pending::Exists { src: old, target: new, value });
            }
            Act::Delete(row) => st.ask_delete(cx, row),
            Act::Add => st.ask_add(cx),
        }
    }
}

/// Handles a reply to a request this view sent (ignore unknown ids).
pub fn on_reply(st: &mut State, inst: &mut Instance, cx: &mut Cx, id: ReqId, result: &Result<Reply, CoreError>) {
    if st.page_req == Some(id) {
        st.page_req = None;
        match result {
            Ok(Reply::Hash(page)) => {
                for (f, v) in &page.items {
                    if st.seen.insert(f.clone()) {
                        st.items.push((f.clone(), v.clone()));
                    }
                }
                st.cursor = page.cursor;
                if page.cursor == 0 {
                    st.done = true;
                    st.len = st.items.len() as u64;
                } else if st.len < st.items.len() as u64 {
                    st.len = st.items.len() as u64;
                }
                if st.sel.is_none() && !st.items.is_empty() {
                    st.select(inst, 0);
                }
            }
            Ok(_) => st.failed = true,
            Err(e) => {
                st.failed = true;
                cx.alert(e.to_string());
            }
        }
        return;
    }
    let Some(pending) = st.pending.remove(&id) else { return };
    match pending {
        Pending::Add { field } => match result {
            Ok(_) => {
                if st.seen.insert(field.clone()) {
                    st.items.push((field, b"New Member".to_vec()));
                }
                st.len += 1;
                inst.content_version += 1;
                let last = st.items.len() - 1;
                st.scroll_to = Some(last);
                st.select(inst, last);
            }
            Err(e) => cx.alert(e.to_string()),
        },
        Pending::Save { field, value } => match result {
            Ok(_) => {
                if let Some(i) = st.pos(&field) {
                    st.items[i].1 = value.clone();
                    if st.sel == Some(i) {
                        editor::mark_saved(&mut inst.editor, value);
                    }
                }
                inst.content_version += 1;
            }
            Err(e) => cx.alert(format!("Redis save failed: {e}")),
        },
        Pending::Delete { field } => match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len = st.len.saturating_sub(1);
                let Some(idx) = st.pos(&field) else { return };
                st.items.remove(idx);
                st.seen.remove(&field);
                match st.sel {
                    Some(s) if s != idx => {
                        if s > idx {
                            st.sel = Some(s - 1);
                        }
                    }
                    _ => {
                        let last = st.items.len().saturating_sub(1);
                        match common::reselect_after_delete(idx, st.len as usize) {
                            Some(n) if !st.items.is_empty() => {
                                st.scroll_to = Some(n.min(last));
                                st.select(inst, n.min(last));
                            }
                            _ => st.clear_selection(inst),
                        }
                    }
                }
            }
            Err(e) => cx.alert(e.to_string()),
        },
        Pending::Exists { src, target, value } => match result {
            Ok(Reply::Bool(false)) => st.send_rename(inst, src, target, value, false),
            Ok(Reply::Bool(true)) => {
                let key = st.key.clone();
                let body = format!(
                    "Field \"{target}\" already exists. Are you sure you want to overwrite this field?"
                );
                let spec = ModalSpec::confirm("Overwrite the field?", "Overwrite", body);
                cx.modal(spec, move |inst, _cx, out| {
                    if out.is_ok() {
                        with_state(inst, move |st, inst| {
                            if st.key == key {
                                st.send_rename(inst, src, target, value, true);
                            }
                        });
                    }
                });
            }
            Ok(_) => {}
            Err(e) => cx.alert(e.to_string()),
        },
        Pending::Rename { src, target, overwrite } => match result {
            Ok(_) => {
                inst.content_version += 1;
                let sel_field = st.sel.and_then(|i| st.items.get(i)).map(|(f, _)| f.clone());
                if overwrite {
                    st.len = st.len.saturating_sub(1);
                    if let Some(d) = st.pos(&target) {
                        st.items.remove(d);
                        st.seen.remove(&target);
                    }
                }
                if let Some(i) = st.pos(&src) {
                    st.items[i].0 = target.clone();
                    st.seen.remove(&src);
                    st.seen.insert(target.clone());
                }
                // Keep the selection on the same field (the renamed row, if it was selected).
                let was_dup = sel_field.as_deref() == Some(target.as_str()) && overwrite;
                let sel_field = match sel_field {
                    Some(f) if f == src => Some(target.clone()),
                    other => other,
                };
                match sel_field.and_then(|f| st.pos(&f)) {
                    Some(i) => {
                        st.sel = Some(i);
                        if was_dup {
                            st.select(inst, i);
                        }
                    }
                    None => st.sel = None,
                }
            }
            Err(e) => cx.alert(e.to_string()),
        },
    }
}
