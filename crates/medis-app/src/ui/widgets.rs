//! Small shared widgets: the AddButton (SPEC-3 8.5), icon buttons, and tiny painted icons.
//! Anything that needs a glyph the default fonts might lack is drawn with the painter.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, epaint};

use super::theme::Palette;

/// The small 16x16 `+` box that sits at the top-right of a list header.
pub fn add_button(ui: &mut Ui, pal: &Palette) -> Response {
    box_button(ui, pal, |p, r, col| {
        let c = r.center();
        p.line_segment([Pos2::new(c.x - 4.0, c.y), Pos2::new(c.x + 4.0, c.y)], Stroke::new(1.5, col));
        p.line_segment([Pos2::new(c.x, c.y - 4.0), Pos2::new(c.x, c.y + 4.0)], Stroke::new(1.5, col));
    })
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The reload box (a circular arrow) that sits 20px to the left of the add button.
pub fn reload_button(ui: &mut Ui, pal: &Palette) -> Response {
    box_button(ui, pal, |p, r, col| {
        let c = r.center();
        let rad = 4.5;
        let pts: Vec<Pos2> = (0..=22)
            .map(|i| {
                let a = -0.6 + (i as f32 / 22.0) * 5.0;
                Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin())
            })
            .collect();
        p.add(epaint::PathShape::line(pts.clone(), Stroke::new(1.3, col)));
        // Arrow head at the end of the arc.
        let tip = *pts.last().unwrap_or(&c);
        p.add(epaint::PathShape::convex_polygon(
            vec![tip + Vec2::new(-3.0, -1.0), tip + Vec2::new(2.5, -2.5), tip + Vec2::new(1.5, 3.0)],
            col,
            Stroke::NONE,
        ));
    })
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn box_button(ui: &mut Ui, pal: &Palette, paint: impl FnOnce(&egui::Painter, Rect, Color32)) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    let (top, bottom) = if pal.dark {
        (Color32::from_rgb(0x44, 0x48, 0x4f), Color32::from_rgb(0x3a, 0x3d, 0x43))
    } else {
        (Color32::WHITE, Color32::from_rgb(0xef, 0xef, 0xef))
    };
    let (top, bottom) = if resp.is_pointer_button_down_on() {
        (bottom, bottom)
    } else if resp.hovered() {
        (top.gamma_multiply(0.97), bottom.gamma_multiply(0.97))
    } else {
        (top, bottom)
    };
    let p = ui.painter();
    // Gradient fill (two stacked halves keep it cheap and crisp).
    let mid = rect.center().y;
    let rounding = CornerRadius::same(2);
    p.rect_filled(rect, rounding, bottom);
    p.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.max.x, mid)),
        CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 },
        top,
    );
    p.rect_stroke(rect, rounding, Stroke::new(1.0, pal.field_border), StrokeKind::Inside);
    paint(p, rect, if pal.dark { pal.text_dim } else { Color32::from_rgb(0x88, 0x88, 0x88) });
    resp
}

/// A flat 20x20 icon button for toolbars (hover shows a soft background).
pub fn icon_button(ui: &mut Ui, pal: &Palette, paint: impl FnOnce(&egui::Painter, Rect, Color32)) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    if resp.hovered() || resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 4.0, pal.hover);
    }
    paint(ui.painter(), rect, if resp.hovered() { pal.text } else { pal.text_dim });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A `+` or `-` glyph centred in `rect`.
pub fn paint_plus(p: &egui::Painter, rect: Rect, col: Color32, minus_only: bool) {
    let c = rect.center();
    p.line_segment([Pos2::new(c.x - 4.5, c.y), Pos2::new(c.x + 4.5, c.y)], Stroke::new(1.5, col));
    if !minus_only {
        p.line_segment([Pos2::new(c.x, c.y - 4.5), Pos2::new(c.x, c.y + 4.5)], Stroke::new(1.5, col));
    }
}

/// A small `x`.
pub fn paint_cross(p: &egui::Painter, rect: Rect, col: Color32) {
    let c = rect.center();
    let d = 3.2;
    p.line_segment([Pos2::new(c.x - d, c.y - d), Pos2::new(c.x + d, c.y + d)], Stroke::new(1.4, col));
    p.line_segment([Pos2::new(c.x - d, c.y + d), Pos2::new(c.x + d, c.y - d)], Stroke::new(1.4, col));
}

/// Tab icons for the Content / Terminal / Config tab bar.
#[derive(Clone, Copy)]
pub enum TabIcon {
    Book,
    Window,
    Cog,
}

pub fn paint_tab_icon(p: &egui::Painter, rect: Rect, icon: TabIcon, col: Color32) {
    let c = rect.center();
    let s = Stroke::new(1.3, col);
    match icon {
        TabIcon::Book => {
            let r = Rect::from_center_size(c, Vec2::new(12.0, 10.0));
            p.rect_stroke(r, 1.5, s, StrokeKind::Inside);
            p.line_segment([Pos2::new(c.x, r.top()), Pos2::new(c.x, r.bottom())], s);
        }
        TabIcon::Window => {
            let r = Rect::from_center_size(c, Vec2::new(13.0, 10.0));
            p.rect_stroke(r, 2.0, s, StrokeKind::Inside);
            p.line_segment([Pos2::new(r.left(), r.top() + 3.0), Pos2::new(r.right(), r.top() + 3.0)], s);
        }
        TabIcon::Cog => {
            p.circle_stroke(c, 3.0, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (sn, cs) = a.sin_cos();
                p.line_segment(
                    [Pos2::new(c.x + cs * 4.6, c.y + sn * 4.6), Pos2::new(c.x + cs * 6.4, c.y + sn * 6.4)],
                    Stroke::new(1.8, col),
                );
            }
        }
    }
}

/// Draws a triangle warning sign (confirm dialogs).
pub fn paint_warning(p: &egui::Painter, center: Pos2, size: f32) {
    let h = size * 0.88;
    let pts = vec![
        Pos2::new(center.x, center.y - h / 2.0),
        Pos2::new(center.x + size / 2.0, center.y + h / 2.0),
        Pos2::new(center.x - size / 2.0, center.y + h / 2.0),
    ];
    p.add(epaint::PathShape::convex_polygon(pts, Color32::from_rgb(0xf5, 0xb3, 0x1f), Stroke::new(2.0, Color32::from_rgb(0xf5, 0xb3, 0x1f))));
    let dark = Color32::from_rgb(0x3a, 0x2a, 0x00);
    p.line_segment([Pos2::new(center.x, center.y - h * 0.14), Pos2::new(center.x, center.y + h * 0.1)], Stroke::new(size * 0.09, dark));
    p.circle_filled(Pos2::new(center.x, center.y + h * 0.26), size * 0.05, dark);
}

/// Draws a round info sign (alert dialogs).
pub fn paint_info(p: &egui::Painter, center: Pos2, size: f32, accent: Color32) {
    p.circle_filled(center, size / 2.0, accent);
    p.text(center, Align2::CENTER_CENTER, "i", FontId::proportional(size * 0.62), Color32::WHITE);
}

/// The light/dark switch: a sun while dark (click for light), a moon while light (click for dark).
pub fn theme_toggle(ui: &mut Ui, pal: &Palette) -> Response {
    let dark = pal.dark;
    let label = if dark { "Switch to light theme" } else { "Switch to dark theme" };
    let resp = icon_button(ui, pal, |p, r, col| if dark { paint_sun(p, r, col) } else { paint_moon(p, r, col) });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let resp = resp.on_hover_text(label);
    if resp.clicked() {
        super::theme::toggle(ui.ctx());
    }
    resp
}

/// A sun: a disc with eight rays.
pub fn paint_sun(p: &egui::Painter, rect: Rect, col: Color32) {
    let c = rect.center();
    p.circle_stroke(c, 3.2, Stroke::new(1.4, col));
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let d = Vec2::angled(a);
        p.line_segment([c + d * 5.4, c + d * 7.2], Stroke::new(1.3, col));
    }
}

/// A crescent moon outline: the part of one circle that a second, offset circle does not cover.
pub fn paint_moon(p: &egui::Painter, rect: Rect, col: Color32) {
    let c = rect.center() + Vec2::new(-0.5, 0.5);
    let (outer_r, inner_c, inner_r) = (6.0, c + Vec2::new(3.2, -2.8), 5.2);
    let stroke = Stroke::new(1.4, col);
    // Each arc is one contiguous run of a sampled circle; start the walk where the run is broken
    // so it never wraps around.
    let arc = |centre: Pos2, r: f32, keep: &dyn Fn(Pos2) -> bool| {
        const N: usize = 96;
        let pts: Vec<Pos2> = (0..N).map(|i| centre + Vec2::angled(i as f32 / N as f32 * std::f32::consts::TAU) * r).collect();
        let start = (0..N).find(|&i| !keep(pts[i])).unwrap_or(0);
        let run: Vec<Pos2> = (1..=N).map(|k| pts[(start + k) % N]).filter(|&q| keep(q)).collect();
        if run.len() > 1 {
            p.add(egui::Shape::line(run, stroke));
        }
    };
    arc(c, outer_r, &|q| q.distance(inner_c) > inner_r);
    arc(inner_c, inner_r, &|q| q.distance(c) < outer_r);
}
