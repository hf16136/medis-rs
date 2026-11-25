//! Key browser (left panel): pattern input, key list, "Scan more" row, and the `Keys: N` / `DB:` footer of the panel (SPEC-1 4.1-4.2, SPEC-2 4.3-4.11).
//!
//! Owns `inst.selected_key` and `inst.db`. Requests: `Op::Scan`, `Op::SelectDb` and the key ops. The
//! first scan starts lazily the first time `show` runs (state is reset on every connect). The
//! `Keys: N` / `DB:` bar and the `INFO` polling live in `footer.rs` and are drawn from here.

use std::collections::HashMap;
use std::time::Duration;

use egui::text::{CCursorRange, LayoutJob};
use egui::{
    Align, CursorIcon, EventFilter, FontId, Id, Key, Layout, Margin, Modifiers, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, UiBuilder, Vec2,
};
use medis_core::ops::KeyKind;
use medis_core::worker::{Op, Reply};
use medis_core::{CoreError, Evt, KeyItem, ReqId, ScanCursor};

use crate::instance::{Cx, Instance, Module};
use crate::ui::footer;
use crate::ui::modal::{Field, ModalSpec};
use crate::ui::patterns;
use crate::ui::theme::{badge_color, badge_label};
use crate::ui::widgets;

const ROW_H: f32 = 24.0;
const TYPE_COL: f32 = 40.0;
const DEBOUNCE: f64 = 0.2;

/// What a request we sent is for, so the reply can be handled.
enum Pending {
    SelectDb,
    Add(String),
    RenameExists { old: String, new: String },
    Rename { old: String, new: String },
    Delete,
    Pttl(String),
    Expire,
    Duplicate(String),
}

/// An inline rename in progress.
struct Rename {
    idx: usize,
    text: String,
    started: bool,
    scroll_y: f32,
}

/// State of this module, stored in `Instance::keys`. Reset (`Default`) on every connect.
#[derive(Default)]
pub struct KeysState {
    started: bool,
    /// The text of the pattern input.
    pattern: String,
    /// The pattern of the scan that filled the list (used by "Scan more").
    applied: String,
    items: Vec<KeyItem>,
    cursor: ScanCursor,
    scanning: bool,
    scan_id: Option<ReqId>,
    /// When a (debounced) refresh is due.
    scan_at: Option<f64>,
    auto_selected: bool,
    selected: Option<usize>,
    /// A key to select again on the next frame (forces the content pane to reload it).
    reselect: Option<String>,
    pending: HashMap<ReqId, Pending>,
    rename: Option<Rename>,
    scroll_y: f32,
    view_h: f32,
    scroll_set: Option<f32>,
}

fn list_id(inst_id: u64) -> Id {
    Id::unique(("keys-list", inst_id))
}

fn send(inst: &mut Instance, op: Op, pending: Pending) {
    let id = inst.send(Module::Keys, op);
    inst.keys.pending.insert(id, pending);
}

// ---- scanning ---------------------------------------------------------------------------------

/// Clears the list and starts a fresh scan with the current pattern.
fn refresh(inst: &mut Instance) {
    let pattern = inst.keys.pattern.clone();
    let st = &mut inst.keys;
    st.items.clear();
    st.cursor = ScanCursor::default();
    st.selected = None;
    st.rename = None;
    st.auto_selected = false;
    st.scan_at = None;
    st.scroll_set = Some(0.0);
    st.applied = pattern.clone();
    st.scanning = true;
    inst.selected_key = None;
    let id = inst.send(
        Module::Keys,
        Op::Scan { pattern, first_time: true, have_keys: false, cursor: ScanCursor::default() },
    );
    inst.keys.scan_id = Some(id);
}

fn scan_more(inst: &mut Instance) {
    let st = &mut inst.keys;
    if st.scanning || !st.cursor.has_more() {
        return;
    }
    st.scanning = true;
    let op = Op::Scan { pattern: st.applied.clone(), first_time: false, have_keys: true, cursor: st.cursor.clone() };
    let id = inst.send(Module::Keys, op);
    inst.keys.scan_id = Some(id);
}

/// Empties the list without scanning (the db changed).
fn clear_list(inst: &mut Instance) {
    let st = &mut inst.keys;
    st.items.clear();
    st.cursor = ScanCursor::default();
    st.selected = None;
    st.rename = None;
    st.scanning = false;
    st.scan_id = None;
    st.pattern.clear();
    st.applied.clear();
    st.scan_at = Some(0.0);
    inst.selected_key = None;
}

/// `true` while a `SELECT` we sent has not been answered yet.
pub fn db_switch_pending(inst: &Instance) -> bool {
    inst.keys.pending.values().any(|p| matches!(p, Pending::SelectDb))
}

/// Switches to database `n`: `SELECT n`, empties the pattern and refreshes. Called by the footer.
pub fn change_db(inst: &mut Instance, n: u32) {
    if inst.db == n {
        return;
    }
    inst.db = n;
    clear_list(inst);
    send(inst, Op::SelectDb(n), Pending::SelectDb);
}

/// The list should show a key that was just created: put its name in the pattern and refresh.
fn on_create(inst: &mut Instance, key: String) {
    inst.keys.pattern = key;
    inst.keys.scan_at = Some(0.0);
    inst.content_version += 1;
}

// ---- selection --------------------------------------------------------------------------------

fn select(inst: &mut Instance, idx: usize) {
    let key = inst.keys.items.get(idx).map(|i| i.key.clone());
    match key {
        Some(k) => {
            inst.keys.selected = Some(idx);
            inst.selected_key = Some(k);
            ensure_visible(&mut inst.keys, idx);
        }
        None => {
            inst.keys.selected = None;
            inst.selected_key = None;
        }
    }
}

fn ensure_visible(st: &mut KeysState, idx: usize) {
    let top = idx as f32 * ROW_H;
    let bottom = top + ROW_H;
    if top < st.scroll_y {
        st.scroll_set = Some(top);
    } else if st.view_h > 0.0 && bottom > st.scroll_y + st.view_h {
        st.scroll_set = Some(bottom - st.view_h);
    }
}

fn position(inst: &Instance, key: &str) -> Option<usize> {
    inst.keys.items.iter().position(|i| i.key == key)
}

// ---- key flows --------------------------------------------------------------------------------

fn add_flow(cx: &mut Cx) {
    let spec = ModalSpec::form(
        "",
        "Create Key",
        "",
        vec![Field::text("Key Name:"), Field::choice("Type:", &["string", "hash", "list", "set", "zset"])],
    );
    cx.modal(spec, |inst, _cx, out| {
        if !out.is_ok() {
            return;
        }
        let key = out.text(0);
        let kind = match out.text(1).as_str() {
            "hash" => KeyKind::Hash,
            "list" => KeyKind::List,
            "set" => KeyKind::Set,
            "zset" => KeyKind::ZSet,
            _ => KeyKind::String,
        };
        send(inst, Op::AddKey { key: key.clone(), kind }, Pending::Add(key));
    });
}

fn delete_flow(cx: &mut Cx, key: String) {
    let spec = ModalSpec::confirm(
        "Delete selected key?",
        "Delete",
        "Are you sure you want to delete the selected key? This action cannot be undone.",
    );
    cx.modal(spec, move |inst, _cx, out| {
        if !out.is_ok() {
            return;
        }
        if let Some(idx) = position(inst, &key) {
            inst.keys.items.remove(idx);
            let len_after = inst.keys.items.len() as isize;
            let next = if idx as isize >= len_after - 1 { idx as isize - 1 } else { idx as isize };
            if next < 0 {
                inst.keys.selected = None;
                inst.selected_key = None;
            } else {
                select(inst, next as usize);
            }
        }
        send(inst, Op::Delete(key.clone()), Pending::Delete);
    });
}

fn duplicate_flow(cx: &mut Cx, key: String) {
    let spec = ModalSpec::form("", "Duplicate Key", "", vec![Field::text("Target Key:"), Field::flag("Keep TTL:", false)]);
    cx.modal(spec, move |inst, _cx, out| {
        if !out.is_ok() {
            return;
        }
        let target = out.text(0);
        let op = Op::Duplicate { source: key.clone(), target: target.clone(), keep_ttl: out.flag(1) };
        send(inst, op, Pending::Duplicate(target));
    });
}

fn start_rename(inst: &mut Instance, idx: usize) {
    if idx >= inst.keys.items.len() {
        return;
    }
    select(inst, idx);
    let st = &mut inst.keys;
    st.rename = Some(Rename { idx, text: st.items[idx].key.clone(), started: false, scroll_y: st.scroll_y });
}

fn commit_rename(inst: &mut Instance, idx: usize, new: String) {
    let Some(old) = inst.keys.items.get(idx).map(|i| i.key.clone()) else { return };
    if new.is_empty() || new == old {
        return;
    }
    send(inst, Op::Exists(new.clone()), Pending::RenameExists { old, new });
}

fn do_rename(inst: &mut Instance, old: String, new: String) {
    send(inst, Op::Rename { old: old.clone(), new: new.clone() }, Pending::Rename { old, new });
}

/// Selects the same key again on the next frame so the content pane loads it from scratch.
fn reload_selected(inst: &mut Instance, cx: &Cx) {
    if let Some(k) = inst.selected_key.take() {
        inst.keys.reselect = Some(k);
        cx.ctx.request_repaint();
    }
}

fn on_reply(inst: &mut Instance, cx: &mut Cx, pending: Pending, result: &Result<Reply, CoreError>) {
    let reply = match result {
        Ok(r) => r,
        Err(e) => {
            cx.alert(e.message.clone());
            return;
        }
    };
    match pending {
        Pending::SelectDb => {}
        Pending::Add(key) | Pending::Duplicate(key) => on_create(inst, key),
        Pending::RenameExists { old, new } => {
            if matches!(reply, Reply::Bool(true)) {
                let spec = ModalSpec::confirm(
                    "Overwrite the key?",
                    "Overwrite",
                    format!("Key \"{new}\" already exists. Are you sure you want to overwrite this key?"),
                );
                cx.modal(spec, move |inst, _cx, out| {
                    if out.is_ok() {
                        do_rename(inst, old, new);
                    }
                });
            } else {
                do_rename(inst, old, new);
            }
        }
        Pending::Rename { old, new } => {
            if let Some(mut idx) = position(inst, &old) {
                inst.keys.items[idx].key = new.clone();
                let dup = inst.keys.items.iter().enumerate().position(|(j, k)| j != idx && k.key == new);
                if let Some(j) = dup {
                    inst.keys.items.remove(j);
                    if j < idx {
                        idx -= 1;
                    }
                }
                select(inst, idx);
            }
            inst.selected_key = Some(new);
            inst.content_version += 1;
        }
        Pending::Delete | Pending::Expire => inst.content_version += 1,
        Pending::Pttl(key) => {
            let current = if let Reply::Int(n) = reply { *n } else { -1 };
            let spec = ModalSpec::form("", "Set Expiration", "", vec![Field::number("PTTL (ms):", Some(current as f64))]);
            cx.modal(spec, move |inst, _cx, out| {
                if out.is_ok() {
                    let pttl_ms = out.number(0) as i64;
                    send(inst, Op::SetExpire { key, pttl_ms }, Pending::Expire);
                }
            });
        }
    }
}

/// Receives worker events. Called for replies to requests this module sent (`Module::Keys`),
/// for every `ScanBatch` of its scans, and for every unsolicited event.
pub fn handle_evt(inst: &mut Instance, cx: &mut Cx, evt: &Evt) {
    match evt {
        Evt::ScanBatch { id, keys, cursor } if inst.keys.scan_id == Some(*id) => {
            let st = &mut inst.keys;
            st.items.extend(keys.iter().cloned());
            st.cursor = cursor.clone();
            if !st.auto_selected && inst.selected_key.is_none() && !inst.keys.items.is_empty() {
                inst.keys.auto_selected = true;
                select(inst, 0);
            }
        }
        Evt::ScanDone { id, result } if inst.keys.scan_id == Some(*id) => {
            inst.keys.scanning = false;
            match result {
                Ok(cursor) => inst.keys.cursor = cursor.clone(),
                Err(e) => cx.alert(e.message.clone()),
            }
        }
        // A `SELECT` typed in the terminal: the app already updated `inst.db`.
        Evt::DbSelected(_) => clear_list(inst),
        Evt::Reply { id, result } => {
            if let Some(p) = inst.keys.pending.remove(id) {
                on_reply(inst, cx, p, result);
            }
        }
        _ => {}
    }
}

// ---- drawing ----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum MenuAct {
    Copy,
    Reload,
    Expire,
    Rename,
    Duplicate,
    Delete,
}

#[derive(Default)]
struct Events {
    click: Option<usize>,
    dbl: Option<usize>,
    secondary: Option<usize>,
    menu: Option<(usize, MenuAct)>,
    more: bool,
    /// `Some(None)` = the rename was cancelled, `Some(Some(..))` = commit.
    rename_end: Option<Option<(usize, String)>>,
}

/// Draws the module into `ui` (the region the app shell gave it).
pub fn show(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    let ctx = cx.ctx.clone();
    let now = ctx.input(|i| i.time);

    if !inst.keys.started {
        inst.keys.started = true;
        refresh(inst);
    }
    if let Some(k) = inst.keys.reselect.take() {
        inst.selected_key = Some(k);
    }
    run_debounce(inst, &ctx, now);
    keyboard(inst, cx);

    ui.spacing_mut().item_spacing = Vec2::ZERO;
    pattern_bar(ui, inst, cx, now);
    header_bar(ui, inst, cx);

    let width = ui.available_width();
    let list_h = (ui.available_height() - footer::BAR_HEIGHT).max(ROW_H);
    ui.allocate_ui(Vec2::new(width, list_h), |ui| list(ui, inst, cx));

    footer::browser_footer(ui, inst, cx);
}

fn run_debounce(inst: &mut Instance, ctx: &egui::Context, now: f64) {
    let Some(at) = inst.keys.scan_at else { return };
    let at = if at == 0.0 { now + DEBOUNCE } else { at };
    if now >= at {
        refresh(inst);
    } else {
        inst.keys.scan_at = Some(at);
        ctx.request_repaint_after(Duration::from_secs_f64(at - now));
    }
}

fn keyboard(inst: &mut Instance, cx: &mut Cx) {
    if cx.modals.is_open() {
        return;
    }
    let ctx = cx.ctx.clone();
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) {
        refresh(inst);
        return;
    }
    if inst.keys.rename.is_some() || !ctx.memory(|m| m.has_focus(list_id(inst.id))) {
        return;
    }
    let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
        || ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::C));
    if copy && let Some(k) = &inst.selected_key {
        ctx.copy_text(k.clone());
    }
    let up = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp));
    let down = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown));
    let len = inst.keys.items.len();
    if (up || down) && len > 0 {
        let next = match (inst.keys.selected, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(len - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        select(inst, next);
    }
    let del = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace))
        || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete));
    if del && let Some(k) = inst.selected_key.clone() {
        delete_flow(cx, k);
    }
}

fn pattern_bar(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx, now: f64) {
    let pal = cx.pal;
    let mut picked: Option<String> = None;
    let mut manage = false;
    let mut enter = false;
    let saved = patterns::list(cx, &inst.pattern_key());

    egui::Frame::new().inner_margin(Margin::symmetric(8, 7)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 24.0).max(40.0);
            let te = TextEdit::singleline(&mut inst.keys.pattern)
                .hint_text("Key name or patterns (e.g. user:*)")
                .desired_width(w)
                .margin(Margin::symmetric(6, 3));
            let r = ui.add(te);
            if r.changed() {
                inst.keys.scan_at = Some(now + DEBOUNCE);
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                enter = true;
            }
            let chev = widgets::icon_button(ui, &pal, |p, rect, col| {
                let c = rect.center();
                let s = Stroke::new(1.4, col);
                p.line_segment([Pos2::new(c.x - 4.0, c.y - 1.5), Pos2::new(c.x, c.y + 2.5)], s);
                p.line_segment([Pos2::new(c.x, c.y + 2.5), Pos2::new(c.x + 4.0, c.y - 1.5)], s);
            });
            egui::Popup::menu(&chev).close_behavior(egui::PopupCloseBehavior::CloseOnClick).show(|ui| {
                ui.set_min_width(190.0);
                if saved.is_empty() {
                    ui.add_enabled(false, egui::Label::new("No saved patterns"));
                }
                for p in &saved {
                    if ui.button(&p.name).on_hover_text(&p.value).clicked() {
                        picked = Some(p.value.clone());
                    }
                }
                ui.separator();
                if ui.button("Manage Patterns...").clicked() {
                    manage = true;
                }
            });
        });
    });

    if let Some(v) = picked {
        inst.keys.pattern = v;
        refresh(inst);
    }
    if enter {
        refresh(inst);
    }
    if manage {
        patterns::open(inst, cx);
    }
}

fn header_bar(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    let pal = cx.pal;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, pal.toolbar);
    p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, pal.border));
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, pal.border));
    p.text(
        Pos2::new(rect.left() + TYPE_COL + 2.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "name",
        FontId::proportional(12.0),
        pal.text_dim,
    );
    let inner = Rect::from_min_max(rect.min, Pos2::new(rect.right() - 8.0, rect.bottom()));
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    child.spacing_mut().item_spacing.x = 4.0;
    if widgets::add_button(&mut child, &pal).on_hover_text("New key").clicked() {
        add_flow(cx);
    }
    if widgets::reload_button(&mut child, &pal).on_hover_text("Reload keys").clicked() {
        refresh(inst);
    }
}

fn list(ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    let pal = cx.pal;
    let lid = list_id(inst.id);
    let focused = cx.ctx.memory(|m| m.has_focus(lid));
    let more_row = inst.keys.cursor.has_more() || inst.keys.scanning;
    let total = inst.keys.items.len() + usize::from(more_row);
    let mut rename = inst.keys.rename.take();
    let mut ev = Events::default();
    let selected = inst.keys.selected;
    let scanning = inst.keys.scanning;
    let more_text = if scanning {
        format!("Scanning...(cursor {})", inst.keys.cursor.display())
    } else {
        "Scan more".to_string()
    };
    let mono = FontId::monospace(12.0);

    let mut area = ScrollArea::vertical().id_salt(lid).auto_shrink([false, false]);
    let programmatic = inst.keys.scroll_set.take();
    if let Some(y) = programmatic {
        area = area.vertical_scroll_offset(y);
    }
    let items = &inst.keys.items;
    let out = area.show_rows(ui, ROW_H, total, |ui, range| {
        for i in range {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::CLICK);
            if i >= items.len() {
                let col = if scanning { pal.text_faint } else { pal.text_dim };
                if resp.hovered() && !scanning {
                    ui.painter().rect_filled(rect, 0.0, pal.hover);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 10.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &more_text,
                    FontId::proportional(12.0),
                    col,
                );
                if !scanning && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    ev.more = true;
                }
                continue;
            }
            let item = &items[i];
            let is_sel = selected == Some(i);
            // The row is painted by hand, so describe it for screen readers (and the GUI tests).
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, is_sel, &item.key));
            let (bg, fg) = if is_sel && focused {
                (Some(pal.accent), pal.accent_text)
            } else if is_sel {
                (Some(pal.tab_active), pal.text)
            } else if resp.hovered() {
                (Some(pal.hover), pal.text)
            } else {
                (None, pal.text)
            };
            if let Some(bg) = bg {
                ui.painter().rect_filled(rect, 0.0, bg);
            }
            paint_badge(ui, rect, &item.ktype);

            let name_rect = Rect::from_min_max(
                Pos2::new(rect.left() + TYPE_COL + 2.0, rect.top()),
                Pos2::new(rect.right() - 8.0, rect.bottom()),
            );
            match rename.as_mut().filter(|r| r.idx == i) {
                Some(r) => rename_edit(ui, name_rect, r, &mono, &mut ev, ui.scope_id().with(i)),
                None => {
                    let mut job = LayoutJob::simple_singleline(item.key.clone(), mono.clone(), fg);
                    job.wrap.max_width = name_rect.width();
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    let galley = ui.painter().layout_job(job);
                    let pos = Pos2::new(name_rect.left(), name_rect.center().y - galley.size().y / 2.0);
                    ui.painter().with_clip_rect(name_rect).galley(pos, galley, fg);
                }
            }

            if resp.clicked() {
                ev.click = Some(i);
            }
            if resp.double_clicked() {
                ev.dbl = Some(i);
            }
            if resp.secondary_clicked() {
                ev.secondary = Some(i);
            }
            resp.context_menu(|ui| {
                ui.set_min_width(180.0);
                let entries = [
                    ("Copy to Clipboard", MenuAct::Copy),
                    ("Reload", MenuAct::Reload),
                    ("", MenuAct::Copy),
                    ("Set expiration", MenuAct::Expire),
                    ("Rename Key...", MenuAct::Rename),
                    ("Duplicate Key...", MenuAct::Duplicate),
                    ("Delete", MenuAct::Delete),
                ];
                for (label, act) in entries {
                    if label.is_empty() {
                        ui.separator();
                    } else if ui.button(label).clicked() {
                        ev.menu = Some((i, act));
                        ui.close();
                    }
                }
            });
        }
    });
    let scroll_y = out.state.clamped_offset().y;
    inst.keys.view_h = out.inner_rect.height();
    inst.keys.rename = rename;

    // Keep the list focusable so the arrow keys and shortcuts work, without stealing arrows from
    // the rest of the window.
    ui.interact(out.inner_rect, lid, Sense::focusable_noninteractive());
    if focused {
        let filter = EventFilter { vertical_arrows: true, ..Default::default() };
        cx.ctx.memory_mut(|m| m.set_focus_lock_filter(lid, filter));
    }
    if total == 0 {
        let c = out.inner_rect.center();
        ui.painter().text(c, egui::Align2::CENTER_CENTER, "No keys found", FontId::proportional(12.0), pal.text_faint);
    }
    // Scrolling by the user cancels a rename; our own scroll-into-view does not.
    if let Some(r) = &mut inst.keys.rename {
        if programmatic.is_some() {
            r.scroll_y = scroll_y;
        } else if (scroll_y - r.scroll_y).abs() > 0.5 {
            inst.keys.rename = None;
        }
    }
    inst.keys.scroll_y = scroll_y;

    apply_events(inst, cx, ev);
}

fn paint_badge(ui: &egui::Ui, row: Rect, ktype: &str) {
    let rect = Rect::from_center_size(Pos2::new(row.left() + TYPE_COL / 2.0 + 2.0, row.center().y), Vec2::new(32.0, 16.0));
    let p = ui.painter();
    if ktype.is_empty() {
        let col = ui.visuals().widgets.noninteractive.bg_stroke.color;
        p.rect_stroke(rect, 3.0, Stroke::new(1.0, col), StrokeKind::Inside);
    } else {
        p.rect_filled(rect, 3.0, badge_color(ktype));
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            badge_label(ktype).to_uppercase(),
            FontId::proportional(10.0),
            egui::Color32::WHITE,
        );
    }
}

fn rename_edit(ui: &mut egui::Ui, rect: Rect, r: &mut Rename, font: &FontId, ev: &mut Events, id: Id) {
    let ctx = ui.ctx().clone();
    if !r.started {
        let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::select_all_str(&r.text)));
        state.store(&ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let out = ui
        .scope_builder(UiBuilder::new().max_rect(rect.shrink2(Vec2::new(0.0, 2.0))), |ui| {
            TextEdit::singleline(&mut r.text)
                .id(id)
                .font(font.clone())
                .desired_width(rect.width())
                .margin(Margin::symmetric(4, 1))
                .show(ui)
        })
        .inner;
    if r.started && out.response.lost_focus() {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            ev.rename_end = Some(None);
        } else {
            ev.rename_end = Some(Some((r.idx, r.text.clone())));
        }
    }
    r.started = true;
}

fn apply_events(inst: &mut Instance, cx: &mut Cx, ev: Events) {
    let lid = list_id(inst.id);
    if let Some(i) = ev.secondary.or(ev.click) {
        cx.ctx.memory_mut(|m| m.request_focus(lid));
        if inst.keys.selected != Some(i) {
            // Choosing another row also ends a rename of the old one.
            select(inst, i);
        }
    }
    if let Some(end) = ev.rename_end {
        inst.keys.rename = None;
        if let Some((idx, text)) = end {
            commit_rename(inst, idx, text);
        }
        cx.ctx.memory_mut(|m| m.request_focus(lid));
    }
    if let Some(i) = ev.dbl {
        start_rename(inst, i);
    }
    if ev.more {
        scan_more(inst);
    }
    if let Some((i, act)) = ev.menu {
        select(inst, i);
        let Some(key) = inst.keys.items.get(i).map(|k| k.key.clone()) else { return };
        match act {
            MenuAct::Copy => cx.ctx.copy_text(key),
            MenuAct::Reload => reload_selected(inst, cx),
            MenuAct::Expire => send(inst, Op::Pttl(key.clone()), Pending::Pttl(key)),
            MenuAct::Rename => start_rename(inst, i),
            MenuAct::Duplicate => duplicate_flow(cx, key),
            MenuAct::Delete => delete_flow(cx, key),
        }
    }
}
