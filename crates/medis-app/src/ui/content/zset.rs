//! ZSet view (SPEC-2 5.5): a virtualized `score | member` table on the left, the editor on the
//! right (saving the editor renames the member, keeping its score).
//!
//! * Rows are loaded in windows (`ops::next_window`) until the deepest visible row is loaded; the
//!   table always has `len` rows and shows `Loading...` for the ones that are not loaded yet.
//! * Sorting by the `score` header toggles `desc` and reloads.
//! * Scores are edited inline (double-click, or the menu `Edit Score`).
//! * Keyboard (Up / Down / Backspace / Delete) works while the table is the focused pane.

use egui::{Align, Color32, Id, Key, Label, Layout, Modifiers, RichText, Sense, Stroke, TextEdit, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use medis_core::ops::{self, ZItem};
use medis_core::worker::Op;
use medis_core::{CoreError, Reply, ReqId};

use super::{View, common};
use crate::instance::{Cx, Instance, Module};
use crate::ui::editor;
use crate::ui::modal::{Field, ModalSpec};
use crate::ui::theme::Palette;
use crate::ui::widgets;

const ROW_H: f32 = 24.0;
/// At most this many bytes of a member are shown in a table cell.
const CELL_BYTES: usize = 256;

/// An inline score edit in progress.
struct ScoreEdit {
    row: usize,
    text: String,
    /// The field still has to be focused and selected.
    fresh: bool,
}

pub struct State {
    pub key: String,
    /// Current size of the zset (the table has this many rows).
    pub len: u64,
    items: Vec<ZItem>,
    desc: bool,
    /// Bumped when the sort order changes, so late replies do not touch the new rows.
    generation: u32,
    /// Page request in flight, with the number of rows that were loaded when it was sent.
    req: Option<(ReqId, u64)>,
    /// A page failed: stop asking.
    stopped: bool,
    max_row: usize,
    sel: Option<usize>,
    auto_selected: bool,
    editor_dirty: bool,
    focused: bool,
    scroll_to: Option<usize>,
    edit: Option<ScoreEdit>,
    /// Score edit in flight: row, member, new score text, generation.
    score_req: Option<(ReqId, Vec<u8>, String, u32)>,
    /// Member rename in flight: row, old member, new member, generation.
    save: Option<(ReqId, Vec<u8>, Vec<u8>, u32)>,
    /// Delete in flight: member, generation.
    remove: Option<(ReqId, Vec<u8>, u32)>,
    /// Add in flight: member, score text, generation.
    add: Option<(ReqId, Vec<u8>, String, f64, u32)>,
}

impl State {
    pub fn new(key: String, len: u64) -> Self {
        Self {
            key,
            len,
            items: Vec::new(),
            desc: false,
            generation: 0,
            req: None,
            stopped: false,
            max_row: 0,
            sel: None,
            auto_selected: false,
            editor_dirty: false,
            focused: false,
            scroll_to: None,
            edit: None,
            score_req: None,
            save: None,
            remove: None,
            add: None,
        }
    }

    fn row_of(&self, member: &[u8]) -> Option<usize> {
        self.items.iter().position(|(m, _)| m == member)
    }
}

/// Runs `f` on the zset view of the instance, if it still shows zset `key`. Used by modal
/// callbacks, which run outside of `show`.
fn with_state(inst: &mut Instance, cx: &mut Cx, key: &str, f: impl FnOnce(&mut State, &mut Instance, &mut Cx)) {
    let mut view = std::mem::take(&mut inst.content.view);
    if let View::ZSet(st) = &mut view
        && st.key == key
    {
        f(st, inst, cx);
    }
    inst.content.view = view;
}

fn request_page(st: &mut State, inst: &mut Instance) {
    let loaded = st.items.len() as u64;
    let id = inst.send(
        Module::Content,
        Op::ZSetPage { key: st.key.clone(), loaded, length: st.len, desc: st.desc },
    );
    st.req = Some((id, loaded));
}

/// Loads the next window while the deepest visible (or selected) row is not loaded yet.
fn maybe_load(st: &mut State, inst: &mut Instance) {
    let need = st.max_row.max(st.sel.unwrap_or(0));
    if !st.stopped && st.req.is_none() && (st.items.len() as u64) < st.len && need >= st.items.len() {
        request_page(st, inst);
    }
}

/// Sends the first load request(s).
pub fn start(st: &mut State, inst: &mut Instance, _cx: &mut Cx) {
    if st.len > 0 {
        request_page(st, inst);
    }
}

fn sync_editor(st: &mut State, inst: &mut Instance) {
    if !st.editor_dirty {
        return;
    }
    match st.sel {
        Some(r) if r < st.items.len() => {
            editor::load(&mut inst.editor, Some(st.items[r].0.clone()));
            st.editor_dirty = false;
        }
        Some(_) => editor::load(&mut inst.editor, None),
        None => {
            editor::load(&mut inst.editor, None);
            st.editor_dirty = false;
        }
    }
}

fn select(st: &mut State, row: Option<usize>) {
    if st.sel == row {
        return;
    }
    st.sel = row;
    st.editor_dirty = true;
}

fn toggle_sort(st: &mut State) {
    st.desc = !st.desc;
    st.generation += 1;
    st.items.clear();
    st.req = None;
    st.stopped = false;
    st.max_row = 0;
    st.sel = None;
    st.auto_selected = false;
    st.editor_dirty = true;
    st.edit = None;
    st.scroll_to = Some(0);
}

fn keyboard(st: &mut State, cx: &mut Cx) {
    // Ignored while an inline edit is active (SPEC-2 5.0).
    if cx.blocked || !st.focused || st.edit.is_some() || cx.ctx.text_edit_focused() {
        return;
    }
    let (up, down, del) = cx.ctx.input_mut(|i| {
        let up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
        let down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
        let back = i.consume_key(Modifiers::NONE, Key::Backspace);
        let delete = i.consume_key(Modifiers::NONE, Key::Delete);
        (up, down, back || delete)
    });
    if up || down {
        let next = match (st.sel, up) {
            (None, _) => (!st.items.is_empty()).then_some(0),
            (Some(r), true) => Some(r.saturating_sub(1)),
            (Some(r), false) => Some(if r + 1 < st.items.len() { r + 1 } else { r }),
        };
        if next.is_some() {
            select(st, next);
            st.scroll_to = next;
        }
    }
    if del {
        request_delete(st, cx);
    }
}

fn request_delete(st: &State, cx: &mut Cx) {
    let Some(row) = st.sel.filter(|r| *r < st.items.len()) else { return };
    let key = st.key.clone();
    let member = st.items[row].0.clone();
    common::confirm_delete(cx, move |inst, cx| {
        with_state(inst, cx, &key, move |st, inst, _cx| {
            if st.remove.is_some() {
                return;
            }
            let id = inst.send(Module::Content, Op::ZSetRemove { key: st.key.clone(), member: member.clone() });
            st.remove = Some((id, member, st.generation));
        });
    });
}

fn open_add(cx: &mut Cx, key: String) {
    let spec = ModalSpec::form(
        "",
        "Insert Member",
        "",
        vec![Field::text("Value:"), Field::number("Score:", None)],
    );
    cx.modal(spec, move |inst, cx, out| {
        if !out.is_ok() {
            return;
        }
        let member = out.text(0).into_bytes();
        let score = out.number(1);
        with_state(inst, cx, &key, move |st, inst, _cx| {
            if st.add.is_some() {
                return;
            }
            let text = fmt_score(score);
            let id = inst.send(
                Module::Content,
                Op::ZSetAdd { key: st.key.clone(), member: member.clone(), score: text.clone() },
            );
            st.add = Some((id, member, text, score, st.generation));
        });
    });
}

/// A score as Redis understands it (`inf` / `-inf` included), without a trailing `.0`.
fn fmt_score(n: f64) -> String {
    if n == f64::INFINITY {
        "inf".into()
    } else if n == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        format!("{n}")
    }
}

fn save(st: &mut State, inst: &mut Instance, cx: &mut Cx, bytes: Vec<u8>) {
    let Some(row) = st.sel.filter(|r| *r < st.items.len()) else {
        cx.alert(common::WAIT_FOR_LOAD);
        return;
    };
    if st.save.is_some() {
        return;
    }
    let (old, score) = st.items[row].clone();
    let id = inst.send(
        Module::Content,
        Op::ZSetReplace { key: st.key.clone(), old: old.clone(), new: bytes.clone(), score },
    );
    st.save = Some((id, old, bytes, st.generation));
}

fn commit_score(st: &mut State, inst: &mut Instance, cx: &mut Cx) {
    let Some(e) = st.edit.take() else { return };
    let Some((member, old)) = st.items.get(e.row).cloned() else { return };
    let text = e.text.trim().to_string();
    if text == old || st.score_req.is_some() {
        return;
    }
    if text.parse::<f64>().is_err() {
        cx.alert("Enter a number.");
        return;
    }
    let id = inst.send(
        Module::Content,
        Op::ZSetSetScore { key: st.key.clone(), member: member.clone(), score: text.clone() },
    );
    st.score_req = Some((id, member, text, st.generation));
}

fn paint_bg(ui: &Ui, col: Color32) {
    let r = ui.max_rect().expand2(0.5 * ui.spacing().item_spacing);
    ui.painter().rect_filled(r, 0.0, col);
}

/// A clickable column title with a small sort triangle. Returns `true` when clicked.
fn sort_header(ui: &mut Ui, pal: &Palette, label: &str, desc: bool) -> bool {
    let r = ui.add(Label::new(RichText::new(label).color(pal.text_dim)).sense(Sense::click()));
    let (rect, r2) = ui.allocate_exact_size(Vec2::new(8.0, 6.0), Sense::click());
    let pts = if desc {
        vec![rect.left_top(), rect.right_top(), egui::pos2(rect.center().x, rect.bottom())]
    } else {
        vec![rect.left_bottom(), rect.right_bottom(), egui::pos2(rect.center().x, rect.top())]
    };
    ui.painter().add(egui::epaint::PathShape::convex_polygon(pts, pal.text_dim, Stroke::NONE));
    let clicked = r.clicked() || r2.clicked();
    r.on_hover_cursor(egui::CursorIcon::PointingHand);
    clicked
}

/// Result of drawing the table this frame.
#[derive(Default)]
struct TableOut {
    toggle: bool,
    add: bool,
    clicked: Option<usize>,
    del: Option<usize>,
    start_edit: Option<usize>,
    commit: bool,
    cancel: bool,
    copy: Option<String>,
    score_w: Option<f32>,
    max_vis: usize,
}

/// The left pane: header + virtualized rows.
fn table(st: &mut State, ui: &mut Ui, cx: &mut Cx) {
    let pal = cx.pal;
    let mut sizes = common::sizes(cx);
    let table_rect = ui.max_rect();
    if ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        st.focused = table_rect.contains(p);
    }

    let desc = st.desc;
    let len = st.len as usize;
    let sel = st.sel;
    let focused = st.focused;

    common::tight_rows(ui);
    let mut builder = TableBuilder::new(ui)
        .id_salt("zset-table")
        .cell_layout(Layout::left_to_right(Align::Center))
        .resizable(true)
        .sense(Sense::click())
        .auto_shrink([false, false])
        .min_scrolled_height(0.0)
        .column(Column::initial(sizes.score_bar_width).at_least(30.0).clip(true))
        .column(Column::remainder().at_least(20.0).clip(true));
    if let Some(r) = st.scroll_to.take() {
        builder = builder.scroll_to_row(r, None);
    }

    let items = &st.items;
    let edit = &mut st.edit;
    let mut out = TableOut::default();

    builder
        .header(ROW_H, |mut h| {
            let (_, r) = h.col(|ui| {
                paint_bg(ui, pal.toolbar);
                common::cell_pad(ui);
                out.toggle = sort_header(ui, &pal, "score", desc);
            });
            out.score_w = Some(r.rect.width());
            h.col(|ui| {
                paint_bg(ui, pal.toolbar);
                common::cell_pad(ui);
                ui.label(RichText::new("member").color(pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    if widgets::add_button(ui, &pal).clicked() {
                        out.add = true;
                    }
                });
            });
        })
        .body(|body| {
            body.rows(ROW_H, len, |mut row| {
                let i = row.index();
                out.max_vis = out.max_vis.max(i);
                let selected = sel == Some(i);
                let (bg, fg) = match (selected, focused) {
                    (true, true) => (Some(pal.accent), pal.accent_text),
                    (true, false) => (Some(pal.tab_active), pal.text),
                    _ => (None, pal.text),
                };
                let editing = edit.as_ref().is_some_and(|e| e.row == i);
                let score_resp = row
                    .col(|ui| {
                        if let Some(bg) = bg {
                            paint_bg(ui, bg);
                        }
                        common::cell_pad(ui);
                        match (items.get(i), edit.as_mut().filter(|e| e.row == i)) {
                            (Some(_), Some(e)) => {
                                let id = Id::unique(("zset-score-edit", i));
                                let r = ui.add(
                                    TextEdit::singleline(&mut e.text)
                                        .id(id)
                                        .font(egui::TextStyle::Monospace)
                                        .text_color(Color32::from_rgb(0x33, 0x33, 0x33))
                                        .background_color(Color32::WHITE)
                                        .desired_width(ui.available_width()),
                                );
                                if e.fresh {
                                    e.fresh = false;
                                    r.request_focus();
                                    if let Some(mut s) = TextEdit::load_state(ui.ctx(), id) {
                                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            egui::text::CCursor::new(e.text.chars().count()),
                                        )));
                                        s.store(ui.ctx(), id);
                                    }
                                } else if ui.input(|inp| inp.key_pressed(Key::Escape)) {
                                    out.cancel = true;
                                } else if r.lost_focus() {
                                    out.commit = true;
                                }
                            }
                            (Some((_, score)), None) => {
                                ui.add(Label::new(RichText::new(score).monospace().size(12.0).color(fg)).truncate());
                            }
                            (None, _) => {
                                ui.label(RichText::new("Loading...").color(pal.text_faint));
                            }
                        }
                    })
                    .1;
                let member_resp = row
                    .col(|ui| {
                        if let Some(bg) = bg {
                            paint_bg(ui, bg);
                        }
                        common::cell_pad(ui);
                        if let Some((m, _)) = items.get(i) {
                            let text = String::from_utf8_lossy(&m[..m.len().min(CELL_BYTES)]);
                            let line = text.lines().next().unwrap_or("");
                            ui.add(Label::new(RichText::new(line).monospace().size(12.0).color(fg)).truncate());
                        }
                    })
                    .1;
                if editing {
                    return;
                }
                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    out.clicked = Some(i);
                }
                if (score_resp.double_clicked() || member_resp.double_clicked()) && i < items.len() {
                    out.start_edit = Some(i);
                }
                if let Some((m, score)) = items.get(i) {
                    resp.context_menu(|ui| {
                        if ui.button("Copy Score to Clipboard").clicked() {
                            out.copy = Some(score.clone());
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Edit Score").clicked() {
                            out.start_edit = Some(i);
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            out.del = Some(i);
                            ui.close();
                        }
                        let _ = m;
                    });
                }
            });
        });

    st.max_row = out.max_vis;
    if let Some(w) = out.score_w
        && ui.input(|i| i.pointer.any_released())
        && (w - sizes.score_bar_width).abs() > 0.5
    {
        sizes.score_bar_width = w;
        common::set_sizes(cx, sizes);
    }

    if let Some(text) = out.copy {
        cx.ctx.copy_text(text);
    }
    if out.toggle {
        toggle_sort(st);
    }
    if out.add {
        open_add(cx, st.key.clone());
    }
    if let Some(i) = out.clicked {
        st.focused = true;
        select(st, Some(i));
    }
    if let Some(i) = out.start_edit {
        st.focused = true;
        select(st, Some(i));
        if let Some((_, score)) = st.items.get(i) {
            st.edit = Some(ScoreEdit { row: i, text: score.clone(), fresh: true });
        }
    }
    if let Some(i) = out.del {
        select(st, Some(i));
        request_delete(st, cx);
    }
    // Commit / cancel are applied by `show` (it has the instance).
    if out.cancel {
        st.edit = None;
    }
    if out.commit {
        cx.ctx.data_mut(|d| d.insert_temp(Id::unique(("zset-commit", cx.inst_id)), true));
    }
}

/// Draws the view.
pub fn show(st: &mut State, ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    keyboard(st, cx);
    sync_editor(st, inst);

    let id = inst.id;
    let mut saved: Option<Vec<u8>> = None;
    {
        let inst_ref = &mut *inst;
        let st_ref = &mut *st;
        common::split(
            ui,
            cx,
            id,
            |ui, cx| table(st_ref, ui, cx),
            |ui, cx| saved = editor::show(ui, inst_ref, cx),
        );
    }
    let commit_id = Id::unique(("zset-commit", inst.id));
    if cx.ctx.data_mut(|d| d.remove_temp::<bool>(commit_id)).unwrap_or(false) {
        commit_score(st, inst, cx);
    }
    if let Some(bytes) = saved {
        save(st, inst, cx, bytes);
    }
    maybe_load(st, inst);
}

/// Handles a reply to a request this view sent (ignore unknown ids).
pub fn on_reply(st: &mut State, inst: &mut Instance, cx: &mut Cx, id: ReqId, result: &Result<Reply, CoreError>) {
    if let Some((rid, loaded)) = st.req
        && rid == id
    {
        st.req = None;
        match result {
            Ok(Reply::ZSet(p)) => {
                if loaded != st.items.len() as u64 {
                    return;
                }
                st.items.extend(p.items.iter().cloned());
                st.len = (st.len as i64 - p.diff).max(st.items.len() as i64) as u64;
                if !st.auto_selected && !st.items.is_empty() {
                    st.auto_selected = true;
                    st.sel = Some(0);
                    st.editor_dirty = true;
                }
            }
            Ok(_) => {}
            Err(e) => {
                st.stopped = true;
                cx.alert(e.to_string());
            }
        }
        return;
    }

    if let Some((rid, member, score, generation)) = st.score_req.clone()
        && rid == id
    {
        st.score_req = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                // The score text is updated in place, with no re-sort.
                if generation == st.generation
                    && let Some(r) = st.row_of(&member)
                {
                    st.items[r].1 = score;
                }
            }
            Err(e) => cx.alert(e.to_string()),
        }
        return;
    }

    if let Some((rid, old, new, generation)) = st.save.clone()
        && rid == id
    {
        st.save = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                if generation == st.generation
                    && let Some(r) = st.row_of(&old)
                {
                    st.items[r].0 = new.clone();
                    if st.sel == Some(r) {
                        editor::mark_saved(&mut inst.editor, new);
                    }
                }
            }
            Err(e) => cx.alert(format!("Redis save failed: {}", e.message)),
        }
        return;
    }

    if let Some((rid, member, generation)) = st.remove.clone()
        && rid == id
    {
        st.remove = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len = st.len.saturating_sub(1);
                if generation == st.generation {
                    let row = st.row_of(&member);
                    if let Some(r) = row {
                        st.items.remove(r);
                    }
                    let from = row.or(st.sel).unwrap_or(0);
                    st.sel = common::reselect_after_delete(from, st.len as usize);
                    st.editor_dirty = true;
                    st.scroll_to = st.sel;
                }
            }
            Err(e) => cx.alert(e.to_string()),
        }
        return;
    }

    if let Some((rid, member, score_text, score, generation)) = st.add.clone()
        && rid == id
    {
        st.add = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len += 1;
                if generation == st.generation {
                    let all_loaded = st.items.len() as u64 + 1 == st.len;
                    let scores: Vec<&str> = st.items.iter().map(|(_, s)| s.as_str()).collect();
                    let pos = ops::zset_insert_index(&scores, score, st.desc);
                    // Insert when the position falls inside the loaded rows, or all rows are loaded.
                    if pos < st.items.len() || all_loaded {
                        st.items.insert(pos, (member, score_text));
                        st.sel = Some(pos);
                        st.editor_dirty = true;
                        st.scroll_to = Some(pos);
                    }
                }
                cx.alert("Added successfully");
            }
            Err(e) => cx.alert(e.to_string()),
        }
    }
}
