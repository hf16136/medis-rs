//! Pattern Manager (SPEC-3 9): a small resizable window to edit the saved key patterns of the
//! active connection and db (`"<connectionKey>|<db>"`, see [`Instance::pattern_key`]).
//!
//! API for the other lanes:
//!
//! * [`open`]`(inst, cx)`: open (or focus) the window for `inst.pattern_key()`. Call it from the
//!   `Manage Patterns...` item of the pattern dropdown.
//! * [`list`]`(cx, &inst.pattern_key())`: the saved patterns for the dropdown. Cheap to call
//!   every frame (cached for a second, and refreshed at once when the manager changes
//!   something), so the dropdown updates live.
//! * [`show`]`(cx, inst)`: draws the window; the app shell calls it once per frame for the
//!   active instance.
//!
//! The window state lives in egui's temporary memory, keyed by the instance id, so no field on
//! `Instance` is needed.

use egui::{Align, Color32, CornerRadius, Id, Layout, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2};
use medis_core::storage::{Pattern, Storage};

use crate::instance::{Cx, Instance};
use crate::ui::theme::{Palette, primary_button};

const LIST_W: f32 = 210.0;
const LIST_H: f32 = 236.0;
const FORM_W: f32 = 328.0;
const ROW_H: f32 = 24.0;
const CACHE_SECS: f64 = 1.0;

/// The saved patterns for `pattern_key` (`"<connectionKey>|<db>"`), in order.
pub fn list(cx: &Cx, pattern_key: &str) -> Vec<Pattern> {
    list_with(cx.ctx, cx.storage, pattern_key)
}

fn cache_id(pattern_key: &str) -> Id {
    Id::unique(("pattern-list-cache", pattern_key))
}

fn list_with(ctx: &egui::Context, storage: &Storage, pattern_key: &str) -> Vec<Pattern> {
    let id = cache_id(pattern_key);
    let now = ctx.input(|i| i.time);
    if let Some((at, items)) = ctx.data(|d| d.get_temp::<(f64, Vec<Pattern>)>(id))
        && now - at < CACHE_SECS
    {
        return items;
    }
    let items = storage.patterns_for(pattern_key);
    ctx.data_mut(|d| d.insert_temp(id, (now, items.clone())));
    // Come back after the cache expires, to pick up changes made by other windows.
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(CACHE_SECS));
    items
}

fn invalidate(ctx: &egui::Context, pattern_key: &str) {
    ctx.data_mut(|d| d.remove::<(f64, Vec<Pattern>)>(cache_id(pattern_key)));
    ctx.request_repaint();
}

/// State of one open manager window.
#[derive(Clone, Default)]
struct Manager {
    /// The pattern key the window was opened for (kept even if the db changes meanwhile).
    key: String,
    items: Vec<Pattern>,
    stamp: u128,
    selected: Option<usize>,
    /// Form buffers; reloaded when `loaded` differs from the selection.
    name: String,
    value: String,
    loaded: Option<(usize, String)>,
}

impl Manager {
    fn reload(&mut self, storage: &Storage) {
        self.items = storage.patterns_for(&self.key);
        self.stamp = storage.stamp();
        if let Some(i) = self.selected
            && i >= self.items.len()
        {
            self.selected = self.items.len().checked_sub(1);
        }
        self.loaded = None;
    }

    /// Copies the selected pattern into the form when the selection changed.
    fn sync_form(&mut self) {
        let want = self.selected.and_then(|i| self.items.get(i).map(|p| (i, p.key.clone())));
        if want != self.loaded {
            match self.selected.and_then(|i| self.items.get(i)) {
                Some(p) => {
                    self.name = p.name.clone();
                    self.value = p.value.clone();
                }
                None => {
                    self.name.clear();
                    self.value.clear();
                }
            }
            self.loaded = want;
        }
    }
}

fn state_id(inst_id: u64) -> Id {
    Id::unique(("pattern-manager", inst_id))
}

/// Opens the manager for the current connection and db (or brings it to the front).
pub fn open(inst: &Instance, cx: &mut Cx) {
    let id = state_id(inst.id);
    let key = inst.pattern_key();
    let mut m = Manager { key, ..Default::default() };
    m.selected = Some(0);
    m.reload(cx.storage);
    if m.items.is_empty() {
        m.selected = None;
    }
    cx.ctx.data_mut(|d| d.insert_temp(id, m));
    cx.ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, window_id(inst.id)));
    cx.ctx.request_repaint();
}

fn window_id(inst_id: u64) -> Id {
    Id::unique(("pattern-manager-window", inst_id))
}

enum Action {
    Add,
    Remove,
    Save,
    Select(usize),
}

/// Draws the window if it is open. Call once per frame for the active instance.
pub fn show(cx: &mut Cx, inst: &Instance) {
    let ctx = cx.ctx;
    let sid = state_id(inst.id);
    let Some(mut m) = ctx.data(|d| d.get_temp::<Manager>(sid)) else { return };

    if m.stamp != cx.storage.stamp() {
        m.reload(cx.storage);
    }
    m.sync_form();

    let pal = cx.pal;
    let blocked = cx.blocked;
    let mut open = true;
    let mut action: Option<Action> = None;

    egui::Window::new("Manage Patterns")
        .id(window_id(inst.id))
        .open(&mut open)
        .default_size([600.0, 300.0])
        .min_size([560.0, 300.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.add_enabled_ui(!blocked, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(14.0, 6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if let Some(i) = pattern_list(ui, &pal, &m) {
                            action = Some(Action::Select(i));
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if small_button(ui, "+", true).on_hover_text("Add a pattern").clicked() {
                                action = Some(Action::Add);
                            }
                            if small_button(ui, "-", m.selected.is_some()).on_hover_text("Remove the selected pattern").clicked() {
                                action = Some(Action::Remove);
                            }
                        });
                    });
                    ui.vertical(|ui| {
                        ui.set_width(FORM_W);
                        if form(ui, &pal, &mut m) {
                            action = Some(Action::Save);
                        }
                    });
                });
            });
        });

    match action {
        Some(Action::Select(i)) => m.selected = Some(i),
        Some(Action::Add) => {
            if cx.storage.create_pattern(&m.key).is_ok() {
                m.reload(cx.storage);
                m.selected = m.items.len().checked_sub(1);
                invalidate(ctx, &m.key);
            }
        }
        Some(Action::Remove) => {
            if let Some(i) = m.selected
                && cx.storage.remove_pattern(&m.key, i).is_ok()
            {
                m.reload(cx.storage);
                m.selected = if m.items.is_empty() { None } else { Some(i.saturating_sub(1)) };
                invalidate(ctx, &m.key);
            }
        }
        Some(Action::Save) => {
            if let Some(i) = m.selected
                && cx.storage.update_pattern(&m.key, i, Some(&m.name), Some(&m.value)).is_ok()
            {
                let (name, value) = (m.name.clone(), m.value.clone());
                m.reload(cx.storage);
                // Keep what was typed (the reload would put the same text back).
                m.name = name;
                m.value = value;
                m.loaded = m.selected.and_then(|i| m.items.get(i).map(|p| (i, p.key.clone())));
                invalidate(ctx, &m.key);
                cx.alert("Save Successfully");
            }
        }
        None => {}
    }

    if open {
        ctx.data_mut(|d| d.insert_temp(sid, m));
    } else {
        ctx.data_mut(|d| d.remove::<Manager>(sid));
    }
}

fn small_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).strong()).min_size(Vec2::new(23.0, 18.0)))
}

/// The list box. Returns the index of a clicked row.
fn pattern_list(ui: &mut Ui, pal: &Palette, m: &Manager) -> Option<usize> {
    let mut clicked = None;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(LIST_W, LIST_H), Sense::hover());
    let fill = if pal.dark { pal.field_bg } else { Color32::WHITE };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    ui.painter().rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, pal.field_border), StrokeKind::Inside);
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(1.0)).layout(Layout::top_down(Align::Min)));
    ScrollArea::vertical().id_salt("pattern-list").auto_shrink(false).show(&mut inner, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        if m.items.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("No patterns yet").size(12.0).color(pal.text_faint));
            });
        }
        for (i, p) in m.items.iter().enumerate() {
            let (row, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
            let active = m.selected == Some(i);
            let (bg, fg) = if active {
                (Some(pal.accent), pal.accent_text)
            } else if resp.hovered() {
                (Some(pal.hover), pal.text)
            } else {
                (None, pal.text)
            };
            if let Some(bg) = bg {
                ui.painter().rect_filled(row, CornerRadius::ZERO, bg);
            }
            let clip = ui.painter().with_clip_rect(row.shrink2(Vec2::new(4.0, 0.0)));
            clip.text(
                egui::pos2(row.left() + 8.0, row.center().y),
                egui::Align2::LEFT_CENTER,
                if p.name.is_empty() { " " } else { p.name.as_str() },
                egui::FontId::proportional(13.0),
                fg,
            );
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// The right-hand form. Returns `true` when Save was pressed.
fn form(ui: &mut Ui, pal: &Palette, m: &mut Manager) -> bool {
    let enabled = m.selected.is_some();
    let mut save = false;
    ui.add_enabled_ui(enabled, |ui| {
        egui::Grid::new("pattern-form").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label("Name:");
            });
            ui.add(TextEdit::singleline(&mut m.name).desired_width(FORM_W - 90.0));
            ui.end_row();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label("Pattern:");
            });
            ui.add(TextEdit::singleline(&mut m.value).desired_width(FORM_W - 90.0));
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            let can_save = !m.name.is_empty() && !m.value.is_empty();
            if ui.add_enabled(can_save, primary_button(pal, "Save")).clicked() {
                save = true;
            }
        });
    });
    if !enabled {
        ui.add_space(8.0);
        ui.label(RichText::new("Select a pattern, or press + to add one.").size(12.0).color(pal.text_faint));
    }
    save
}
