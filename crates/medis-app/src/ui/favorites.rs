//! The favorites sidebar of the connection screen: QUICK CONNECT, FAVORITES (drag to reorder),
//! and the `+` / `-` buttons. Also [`Favorites`], the list shared by all tabs and kept in sync
//! with changes other windows make on disk.

use egui::{Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use medis_core::ConnectionConfig;
use medis_core::storage::Storage;

use crate::instance::{Cx, Instance, Phase};
use crate::ui::connect;
use crate::ui::theme::{self, Palette};
use crate::ui::widgets;

/// The saved favorites, shared by every tab of this window.
pub struct Favorites {
    pub list: Vec<ConnectionConfig>,
    stamp: u128,
}

impl Favorites {
    pub fn load(storage: &Storage) -> Self {
        Self { list: storage.load_favorites(), stamp: storage.stamp() }
    }

    pub fn reload(&mut self, storage: &Storage) {
        self.list = storage.load_favorites();
        self.stamp = storage.stamp();
    }

    /// Reloads when another window changed the files. Returns `true` if it did.
    pub fn poll(&mut self, storage: &Storage) -> bool {
        if storage.stamp() != self.stamp {
            self.reload(storage);
            true
        } else {
            false
        }
    }

    pub fn get(&self, key: &str) -> Option<&ConnectionConfig> {
        self.list.iter().find(|f| f.key == key)
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.list.iter().position(|f| f.key == key)
    }
}

const ROW_H: f32 = 28.0;

/// Draws the sidebar. Call inside the left panel of the connection phase.
pub fn show(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) {
    let pal = cx.pal;
    let busy = inst.phase == Phase::Connecting;
    let mut connect_now = false;

    // Bottom buttons first, so the list takes the rest.
    egui::Panel::bottom("fav-buttons")
        .exact_size(30.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(pal.sidebar).inner_margin(egui::Margin::symmetric(10, 5)))
        .show(ui, |ui| {
            let line = Rect::from_min_size(ui.max_rect().left_top() - Vec2::new(10.0, 5.0), Vec2::new(ui.max_rect().width() + 20.0, 1.0));
            ui.painter().rect_filled(line, 0.0, pal.border);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let add = widgets::icon_button(ui, &pal, |p, r, c| widgets::paint_plus(p, r, c, false)).on_hover_text("Add a favorite");
                let can_remove = inst.connect.selected.is_some() && !busy;
                let rem = ui
                    .add_enabled_ui(can_remove, |ui| {
                        widgets::icon_button(ui, &pal, |p, r, c| widgets::paint_plus(p, r, c, true)).on_hover_text("Delete the selected favorite")
                    })
                    .inner;
                if add.clicked() && !busy {
                    inst.connect.add_favorite(cx);
                }
                if rem.clicked() && can_remove {
                    connect::request_delete(cx);
                }
            });
        });

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(12.0);
        pad(ui, |ui| theme::section_label(ui, &pal, "QUICK CONNECT"));
        ui.add_space(4.0);
        let quick = row(ui, &pal, "Quick Connect", inst.connect.selected.is_none(), RowIcon::Bolt, false);
        if quick.clicked() && !busy {
            inst.connect.select(cx, None);
        }
        if quick.double_clicked() && !busy {
            inst.connect.select(cx, None);
            connect_now = true;
        }

        ui.add_space(14.0);
        pad(ui, |ui| theme::section_label(ui, &pal, "FAVORITES"));
        ui.add_space(4.0);

        if cx.favs.list.is_empty() {
            pad(ui, |ui| {
                ui.label(RichText::new("No favorites yet. Press + to add one.").size(12.0).color(pal.text_faint));
            });
        }

        let mut rects: Vec<Rect> = Vec::new();
        let mut started: Option<usize> = None;
        let mut select: Option<String> = None;
        let mut dbl: Option<String> = None;
        for (i, fav) in cx.favs.list.iter().enumerate() {
            let selected = inst.connect.selected.as_deref() == Some(fav.key.as_str());
            let name = if selected { inst.connect.form.name.as_str() } else { fav.name.as_str() };
            let dragging_this = inst.connect.drag.map(|d| d.from) == Some(i);
            let r = row(ui, &pal, name, selected, RowIcon::Bookmark, dragging_this);
            rects.push(r.rect);
            if r.drag_started() && !busy {
                started = Some(i);
            }
            if r.clicked() && !busy {
                select = Some(fav.key.clone());
            }
            if r.double_clicked() && !busy {
                dbl = Some(fav.key.clone());
            }
        }
        if let Some(i) = started {
            inst.connect.drag = Some(connect::Drag { from: i, to: i });
            if let Some(f) = cx.favs.list.get(i) {
                select = Some(f.key.clone());
            }
        }
        if let Some(key) = select
            && inst.connect.selected.as_deref() != Some(key.as_str())
        {
            inst.connect.select(cx, Some(key));
        }
        if let Some(key) = dbl {
            inst.connect.select(cx, Some(key));
            connect_now = true;
        }

        // Drag and drop reorder.
        if let Some(mut d) = inst.connect.drag {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                d.to = rects
                    .iter()
                    .position(|r| p.y < r.bottom() + 1.0)
                    .unwrap_or(rects.len().saturating_sub(1));
            }
            if let Some(r) = rects.get(d.to) {
                // The marker shows where the row will land.
                let y = if d.to > d.from { r.bottom() + 1.0 } else { r.top() - 1.0 };
                ui.painter().line_segment(
                    [Pos2::new(r.left() + 8.0, y), Pos2::new(r.right() - 8.0, y)],
                    Stroke::new(2.0, pal.accent),
                );
            }
            if ui.input(|i| i.pointer.any_released()) {
                if d.from != d.to {
                    let _ = cx.storage.reorder_favorite(d.from, d.to);
                    cx.favs.reload(cx.storage);
                }
                inst.connect.drag = None;
            } else {
                inst.connect.drag = Some(d);
                ui.ctx().request_repaint();
            }
        }
    });

    if connect_now {
        connect::connect_now(inst, cx);
    }
}

fn pad(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        add(ui);
    });
}

#[derive(Clone, Copy)]
enum RowIcon {
    Bolt,
    Bookmark,
}

fn row(ui: &mut Ui, pal: &Palette, text: &str, active: bool, icon: RowIcon, faded: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click_and_drag());
    let rect = rect.shrink2(Vec2::new(6.0, 0.0));
    let (bg, fg) = if active {
        (Some(pal.accent), pal.accent_text)
    } else if resp.hovered() {
        (Some(pal.hover), pal.text)
    } else {
        (None, pal.text)
    };
    let fg = if faded { fg.gamma_multiply(0.4) } else { fg };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 5.0, if faded { bg.gamma_multiply(0.5) } else { bg });
    }
    let icon_col = if active { Color32::WHITE } else { pal.text_dim };
    let c = Pos2::new(rect.left() + 17.0, rect.center().y);
    match icon {
        RowIcon::Bolt => {
            let pts = [(2.0, -6.0), (-3.5, 1.0), (-0.3, 1.0), (-2.0, 6.0), (3.5, -1.5), (0.3, -1.5), (2.0, -6.0)];
            let path: Vec<Pos2> = pts.iter().map(|(x, y)| c + Vec2::new(*x, *y)).collect();
            ui.painter().add(egui::epaint::PathShape::convex_polygon(path, icon_col, Stroke::NONE));
        }
        RowIcon::Bookmark => {
            let path = vec![
                c + Vec2::new(-3.5, -6.0),
                c + Vec2::new(3.5, -6.0),
                c + Vec2::new(3.5, 6.0),
                c + Vec2::new(0.0, 3.0),
                c + Vec2::new(-3.5, 6.0),
            ];
            ui.painter().add(egui::epaint::PathShape::convex_polygon(path, icon_col, Stroke::NONE));
        }
    }
    let galley_pos = Pos2::new(rect.left() + 32.0, rect.center().y);
    let shown = if text.is_empty() { "Untitled" } else { text };
    let clip = ui.painter().with_clip_rect(Rect::from_min_max(rect.min, Pos2::new(rect.right() - 6.0, rect.bottom())));
    clip.text(galley_pos, egui::Align2::LEFT_CENTER, shown, egui::FontId::proportional(13.0), fg);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
