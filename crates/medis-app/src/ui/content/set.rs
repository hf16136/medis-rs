//! Set view (SPEC-2 5.0, 5.4): a virtualized one-column table of members on the left, the value
//! editor on the right.
//!
//! * Pages come from `SSCAN` (cursor based) and are de-duplicated by member bytes. When the
//!   cursor reaches 0 the length becomes the loaded count.
//! * The table draws `len` rows; rows that are not loaded yet show `Loading...`. The next page is
//!   requested while the deepest visible row is not loaded.
//! * Write requests are tracked in `pending` and finished in `on_reply`, which looks the row up by
//!   member bytes (row indexes can shift while a request is in flight).

use std::collections::{HashMap, HashSet};

use egui::{Align, Key, Label, Layout, Modifiers, Rect, RichText, Sense};
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
    Add { member: Vec<u8> },
    Replace { old: Vec<u8>, new: Vec<u8> },
    Delete { member: Vec<u8> },
}

/// Something the user asked for during the frame; applied after the layout pass.
enum Act {
    Select(usize),
    Delete(usize),
    Add,
}

pub struct State {
    pub key: String,
    pub len: u64,
    items: Vec<Vec<u8>>,
    seen: HashSet<Vec<u8>>,
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
            pending: HashMap::new(),
        }
    }

    fn send(&mut self, inst: &mut Instance, op: Op, pending: Pending) {
        let id = inst.send(Module::Content, op);
        self.pending.insert(id, pending);
    }

    fn request_page(&mut self, inst: &mut Instance) {
        let op = Op::SetPage { key: self.key.clone(), cursor: self.cursor };
        self.page_req = Some(inst.send(Module::Content, op));
    }

    fn pos(&self, member: &[u8]) -> Option<usize> {
        self.items.iter().position(|m| m == member)
    }

    /// Selects a loaded row and shows its value in the editor.
    fn select(&mut self, inst: &mut Instance, row: usize) {
        if let Some(m) = self.items.get(row) {
            self.sel = Some(row);
            editor::load(&mut inst.editor, Some(m.clone()));
        }
    }

    fn clear_selection(&mut self, inst: &mut Instance) {
        self.sel = None;
        editor::load(&mut inst.editor, None);
    }

    fn ask_delete(&mut self, cx: &mut Cx, row: usize) {
        let Some(member) = self.items.get(row) else { return };
        let (key, member) = (self.key.clone(), member.clone());
        common::confirm_delete(cx, move |inst, _cx| {
            with_state(inst, move |st, inst| {
                if st.key == key {
                    let op = Op::SetRemove { key, member: member.clone() };
                    st.send(inst, op, Pending::Delete { member });
                }
            });
        });
    }

    fn ask_add(&mut self, cx: &mut Cx) {
        let key = self.key.clone();
        let spec = ModalSpec::form("Insert Member", "Insert Member", "", vec![Field::text("Value:")]);
        cx.modal(spec, move |inst, _cx, out| {
            if !out.is_ok() {
                return;
            }
            let member = out.text(0).into_bytes();
            with_state(inst, move |st, inst| {
                if st.key == key {
                    let op = Op::SetAdd { key, member: member.clone() };
                    st.send(inst, op, Pending::Add { member });
                }
            });
        });
    }

    fn save(&mut self, inst: &mut Instance, cx: &mut Cx, bytes: Vec<u8>) {
        let Some(i) = self.sel.filter(|i| *i < self.items.len()) else {
            cx.alert(common::WAIT_FOR_LOAD);
            return;
        };
        let old = self.items[i].clone();
        let op = Op::SetReplace { key: self.key.clone(), old: old.clone(), new: bytes.clone() };
        self.send(inst, op, Pending::Replace { old, new: bytes });
    }

    /// Up / Down / Backspace / Delete, only when the table is the focus.
    fn keys(&mut self, cx: &Cx, acts: &mut Vec<Act>) {
        if cx.blocked || !self.active || cx.ctx.egui_wants_keyboard_input() {
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
        let Self { items, sel, max_row, .. } = self;

        let mut builder = TableBuilder::new(ui)
            .id_salt(("set-table", inst_id))
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
                    ui.label(RichText::new("member").strong().color(pal.text_dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                    row.col(|ui| {
                        if loaded {
                            let text = String::from_utf8_lossy(&items[i]).into_owned();
                            ui.add(Label::new(RichText::new(text).color(pal.text)).truncate().selectable(false));
                        } else {
                            ui.add(Label::new(RichText::new("Loading...").color(pal.text_faint)).selectable(false));
                        }
                    });
                    if !loaded {
                        return;
                    }
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() || resp.double_clicked() {
                        acts.push(Act::Select(i));
                    }
                    resp.context_menu(|ui| {
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

/// Runs `f` on the set state of the current content view (from a modal callback). Does nothing
/// when the pane shows something else by then.
fn with_state(inst: &mut Instance, f: impl FnOnce(&mut State, &mut Instance)) {
    let mut view = std::mem::take(&mut inst.content.view);
    if let View::Set(st) = &mut view {
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
            Ok(Reply::Set(page)) => {
                for m in &page.items {
                    if st.seen.insert(m.clone()) {
                        st.items.push(m.clone());
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
        Pending::Add { member } => match result {
            Ok(_) => {
                if st.seen.insert(member.clone()) {
                    st.items.push(member);
                }
                st.len += 1;
                inst.content_version += 1;
                let last = st.items.len() - 1;
                st.scroll_to = Some(last);
                st.select(inst, last);
            }
            Err(e) => cx.alert(e.to_string()),
        },
        Pending::Replace { old, new } => match result {
            Ok(_) => {
                inst.content_version += 1;
                if let Some(i) = st.pos(&old) {
                    st.items[i] = new.clone();
                    st.seen.remove(&old);
                    st.seen.insert(new.clone());
                    if st.sel == Some(i) {
                        editor::mark_saved(&mut inst.editor, new);
                    }
                }
            }
            Err(e) => cx.alert(format!("Redis save failed: {e}")),
        },
        Pending::Delete { member } => match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len = st.len.saturating_sub(1);
                let Some(idx) = st.pos(&member) else { return };
                st.items.remove(idx);
                st.seen.remove(&member);
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
    }
}
