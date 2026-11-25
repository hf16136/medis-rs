//! List view (SPEC-2 5.3): a virtualized `index | item` table on the left, the editor on the right.
//!
//! * Rows are loaded in windows (`ops::next_window`) until the deepest visible row is loaded; the
//!   table always has `len` rows and shows `Loading...` for the ones that are not loaded yet.
//! * Sorting by the `index` header toggles `desc` and reloads. Redis indexes are
//!   `ops::list_redis_index(row, desc)`.
//! * Keyboard (Up / Down / Backspace / Delete) works while the table is the focused pane, i.e.
//!   after the user clicked into it and until they click somewhere else.

use egui::{Align, Color32, Frame, Key, Label, Layout, Margin, Modifiers, RichText, Sense, Stroke, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use medis_core::ops::{self, NEW_ITEM};
use medis_core::worker::Op;
use medis_core::{CoreError, Reply, ReqId};

use super::{View, common};
use crate::instance::{Cx, Instance, Module};
use crate::ui::editor;
use crate::ui::modal::{Field, ModalSpec};
use crate::ui::theme::Palette;
use crate::ui::widgets;

const ROW_H: f32 = 24.0;
/// At most this many bytes of an item are shown in a table cell.
const CELL_BYTES: usize = 256;

pub struct State {
    pub key: String,
    /// Current length of the list (the table has this many rows).
    pub len: u64,
    items: Vec<Vec<u8>>,
    desc: bool,
    /// Bumped when the sort order changes, so late replies do not touch the new rows.
    generation: u32,
    /// Page request in flight, with the number of rows that were loaded when it was sent.
    req: Option<(ReqId, u64)>,
    /// A page failed: stop asking.
    stopped: bool,
    /// Deepest row that was visible in the last frame.
    max_row: usize,
    sel: Option<usize>,
    auto_selected: bool,
    /// The editor must be re-loaded from `sel`.
    editor_dirty: bool,
    /// The table owns the keyboard (the last press was inside it).
    focused: bool,
    scroll_to: Option<usize>,
    save: Option<(ReqId, usize, Vec<u8>, u32)>,
    remove: Option<(ReqId, usize, u32)>,
    push: Option<(ReqId, bool, u32)>,
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
            save: None,
            remove: None,
            push: None,
        }
    }
}

/// Runs `f` on the list view of the instance, if it still shows list `key`. Used by modal
/// callbacks, which run outside of `show`.
fn with_state(inst: &mut Instance, cx: &mut Cx, key: &str, f: impl FnOnce(&mut State, &mut Instance, &mut Cx)) {
    let mut view = std::mem::take(&mut inst.content.view);
    if let View::List(st) = &mut view
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
        Op::ListPage { key: st.key.clone(), loaded, length: st.len, desc: st.desc },
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
            editor::load(&mut inst.editor, Some(st.items[r].clone()));
            st.editor_dirty = false;
        }
        // Selected but not loaded yet: show nothing until the row arrives.
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
    st.scroll_to = Some(0);
}

fn keyboard(st: &mut State, cx: &mut Cx) {
    if cx.blocked || !st.focused || cx.ctx.text_edit_focused() {
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
    common::confirm_delete(cx, move |inst, cx| {
        with_state(inst, cx, &key, move |st, inst, _cx| {
            if st.remove.is_some() || row >= st.items.len() {
                return;
            }
            let index = ops::list_redis_index(row as u64, st.desc);
            let id = inst.send(Module::Content, Op::ListRemove { key: st.key.clone(), index });
            st.remove = Some((id, row, st.generation));
        });
    });
}

fn open_add(st: &State, cx: &mut Cx) {
    let key = st.key.clone();
    let spec = ModalSpec::form("", "Insert Item", "", vec![Field::choice("Insert To:", &["head", "tail"])]);
    cx.modal(spec, move |inst, cx, out| {
        if !out.is_ok() {
            return;
        }
        let head = out.text(0) == "head";
        with_state(inst, cx, &key, move |st, inst, _cx| {
            if st.push.is_some() {
                return;
            }
            let id = inst.send(Module::Content, Op::ListPush { key: st.key.clone(), head });
            st.push = Some((id, head, st.generation));
        });
    });
}

fn save(st: &mut State, inst: &mut Instance, cx: &mut Cx, bytes: Vec<u8>) {
    let Some(row) = st.sel.filter(|r| *r < st.items.len()) else {
        cx.alert(common::WAIT_FOR_LOAD);
        return;
    };
    if st.save.is_some() {
        return;
    }
    let index = ops::list_redis_index(row as u64, st.desc);
    let id = inst.send(Module::Content, Op::ListSet { key: st.key.clone(), index, value: bytes.clone() });
    st.save = Some((id, row, bytes, st.generation));
}

/// Fills the cell background (gapless, like the table's own selection fill).
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
    let len = st.len;
    let sel = st.sel;
    let focused = st.focused;

    let mut builder = TableBuilder::new(ui)
        .id_salt("list-table")
        .resizable(true)
        .sense(Sense::click())
        .auto_shrink([false, false])
        .min_scrolled_height(0.0)
        .column(Column::initial(sizes.index_bar_width).at_least(30.0).clip(true))
        .column(Column::remainder().at_least(20.0).clip(true));
    if let Some(r) = st.scroll_to.take() {
        builder = builder.scroll_to_row(r, None);
    }

    let items = &st.items;
    let mut toggle = false;
    let mut add = false;
    let mut idx_w: Option<f32> = None;
    let mut clicked: Option<usize> = None;
    let mut del: Option<usize> = None;
    let mut max_vis = 0usize;

    builder
        .header(ROW_H, |mut h| {
            let (_, r) = h.col(|ui| {
                paint_bg(ui, pal.toolbar);
                toggle = sort_header(ui, &pal, "index", desc);
            });
            idx_w = Some(r.rect.width());
            h.col(|ui| {
                paint_bg(ui, pal.toolbar);
                ui.label(RichText::new("item").color(pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::add_button(ui, &pal).clicked() {
                        add = true;
                    }
                });
            });
        })
        .body(|body| {
            body.rows(ROW_H, len as usize, |mut row| {
                let i = row.index();
                max_vis = max_vis.max(i);
                let selected = sel == Some(i);
                let (bg, fg) = match (selected, focused) {
                    (true, true) => (Some(pal.accent), pal.accent_text),
                    (true, false) => (Some(pal.tab_active), pal.text),
                    _ => (None, pal.text),
                };
                let number = if desc { len.saturating_sub(1).saturating_sub(i as u64) } else { i as u64 };
                row.col(|ui| {
                    if let Some(bg) = bg {
                        paint_bg(ui, bg);
                    }
                    Frame::new().fill(pal.hover).corner_radius(8.0).inner_margin(Margin::symmetric(6, 1)).show(ui, |ui| {
                        ui.label(RichText::new(number.to_string()).size(11.0).color(pal.text_dim));
                    });
                });
                row.col(|ui| {
                    if let Some(bg) = bg {
                        paint_bg(ui, bg);
                    }
                    match items.get(i) {
                        Some(bytes) => {
                            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(CELL_BYTES)]);
                            let line = text.lines().next().unwrap_or("");
                            ui.add(Label::new(RichText::new(line).monospace().size(12.0).color(fg)).truncate());
                        }
                        None => {
                            ui.label(RichText::new("Loading...").color(pal.text_faint));
                        }
                    }
                });
                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    clicked = Some(i);
                }
                if i < items.len() {
                    resp.context_menu(|ui| {
                        if ui.button("Delete").clicked() {
                            del = Some(i);
                            ui.close();
                        }
                    });
                }
            });
        });

    st.max_row = max_vis;
    if let Some(w) = idx_w
        && ui.input(|i| i.pointer.any_released())
        && (w - sizes.index_bar_width).abs() > 0.5
    {
        sizes.index_bar_width = w;
        common::set_sizes(cx, sizes);
    }

    if toggle {
        toggle_sort(st);
    }
    if add {
        open_add(st, cx);
    }
    if let Some(i) = clicked {
        st.focused = true;
        select(st, Some(i));
    }
    if let Some(i) = del {
        select(st, Some(i));
        request_delete(st, cx);
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
            Ok(Reply::List(p)) => {
                if loaded != st.items.len() as u64 {
                    return; // rows were inserted in between; ask again
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

    if let Some((rid, row, bytes, generation)) = st.save.clone()
        && rid == id
    {
        st.save = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                if generation == st.generation && row < st.items.len() {
                    st.items[row] = bytes.clone();
                    if st.sel == Some(row) {
                        editor::mark_saved(&mut inst.editor, bytes);
                    }
                }
            }
            Err(e) => cx.alert(format!("Redis save failed: {}", e.message)),
        }
        return;
    }

    if let Some((rid, row, generation)) = st.remove
        && rid == id
    {
        st.remove = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len = st.len.saturating_sub(1);
                if generation == st.generation {
                    if row < st.items.len() {
                        st.items.remove(row);
                    }
                    st.sel = common::reselect_after_delete(row, st.len as usize);
                    st.editor_dirty = true;
                    st.scroll_to = st.sel;
                }
            }
            Err(e) => cx.alert(e.to_string()),
        }
        return;
    }

    if let Some((rid, head, generation)) = st.push
        && rid == id
    {
        st.push = None;
        match result {
            Ok(_) => {
                inst.content_version += 1;
                st.len += 1;
                if generation == st.generation {
                    // Head of an ascending list and tail of a descending one both land on top.
                    if head != st.desc {
                        st.items.insert(0, NEW_ITEM.as_bytes().to_vec());
                        st.sel = Some(0);
                        st.editor_dirty = true;
                        st.scroll_to = Some(0);
                    } else if st.items.len() as u64 + 1 == st.len {
                        st.items.push(NEW_ITEM.as_bytes().to_vec());
                    }
                }
            }
            Err(e) => cx.alert(e.to_string()),
        }
    }
}
