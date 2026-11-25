//! Colours, fonts and egui styling, close to the original Medis look (a quiet macOS-style light
//! theme) plus a matching dark theme.
//!
//! Every panel should take its colours from [`Palette`] (`cx.pal` inside a module) instead of
//! hard-coding values, so both themes keep working.

use egui::{Color32, CornerRadius, FontId, Margin, Shadow, Stroke, TextStyle, Theme, Vec2};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// All named colours the app uses. `Copy`, so grab it once per frame.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Window background (behind everything, also the tab strip).
    pub window: Color32,
    /// Left sidebars: favorites list, key browser.
    pub sidebar: Color32,
    /// Main reading surface (tables, editor, connect form).
    pub content: Color32,
    /// Slim bars: tab bar, footers, list headers.
    pub toolbar: Color32,
    /// Dialog sheets.
    pub sheet: Color32,
    /// Hairlines between regions.
    pub border: Color32,
    /// Border of inputs and buttons.
    pub field_border: Color32,
    pub field_bg: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    /// Active tab / pressed segment.
    pub tab_active: Color32,
    pub hover: Color32,
    pub danger: Color32,
    pub terminal_bg: Color32,
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        dark: false,
        window: rgb(0xec, 0xec, 0xec),
        sidebar: rgb(0xe6, 0xe9, 0xed),
        content: rgb(0xff, 0xff, 0xff),
        toolbar: rgb(0xf4, 0xf4, 0xf5),
        sheet: rgb(0xef, 0xef, 0xef),
        border: rgb(0xd3, 0xd3, 0xd3),
        field_border: rgb(0xc5, 0xc5, 0xc5),
        field_bg: rgb(0xff, 0xff, 0xff),
        text: rgb(0x26, 0x28, 0x2b),
        text_dim: rgb(0x70, 0x75, 0x7b),
        text_faint: rgb(0xa3, 0xa7, 0xac),
        accent: rgb(0x11, 0x6c, 0xd6),
        accent_text: Color32::WHITE,
        tab_active: rgb(0xdb, 0xdf, 0xe1),
        hover: rgb(0xe0, 0xe3, 0xe7),
        danger: rgb(0xe0, 0x2a, 0x1c),
        terminal_bg: Color32::BLACK,
    };

    pub const DARK: Palette = Palette {
        dark: true,
        window: rgb(0x1e, 0x1f, 0x22),
        sidebar: rgb(0x26, 0x28, 0x2c),
        content: rgb(0x20, 0x21, 0x25),
        toolbar: rgb(0x2b, 0x2d, 0x32),
        sheet: rgb(0x2f, 0x31, 0x36),
        border: rgb(0x3a, 0x3d, 0x43),
        field_border: rgb(0x4a, 0x4e, 0x56),
        field_bg: rgb(0x17, 0x18, 0x1b),
        text: rgb(0xe4, 0xe6, 0xea),
        text_dim: rgb(0x98, 0x9f, 0xa8),
        text_faint: rgb(0x68, 0x6e, 0x77),
        accent: rgb(0x2f, 0x7f, 0xe6),
        accent_text: Color32::WHITE,
        tab_active: rgb(0x3b, 0x3f, 0x46),
        hover: rgb(0x34, 0x37, 0x3d),
        danger: rgb(0xff, 0x6b, 0x5e),
        terminal_bg: Color32::BLACK,
    };

    /// The palette for the theme egui currently uses.
    pub fn of(ctx: &egui::Context) -> Palette {
        match ctx.theme() {
            Theme::Dark => Palette::DARK,
            Theme::Light => Palette::LIGHT,
        }
    }
}

/// Colour of a key type badge (SPEC-1 4.3). `ktype` is the Redis type name.
pub fn badge_color(ktype: &str) -> Color32 {
    match ktype {
        "string" => rgb(0x5d, 0xc9, 0x36),
        "list" => rgb(0xfc, 0xa3, 0x2a),
        "hash" => rgb(0xb8, 0x65, 0xd0),
        "zset" => rgb(0xfa, 0x50, 0x49),
        "set" => rgb(0x23, 0x9f, 0xf2),
        _ => rgb(0x60, 0xd4, 0xca),
    }
}

/// Text inside a badge: `str` for strings, otherwise the type name.
pub fn badge_label(ktype: &str) -> &str {
    if ktype == "string" { "str" } else { ktype }
}

/// A filled accent button (the "primary" action of a form or dialog).
pub fn primary_button(pal: &Palette, text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(pal.accent_text).strong())
        .fill(pal.accent)
        .stroke(Stroke::NONE)
}

/// Small upper-case section heading used in sidebars ("FAVORITES").
pub fn section_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).size(10.5).strong().color(pal.text_faint));
}

fn visuals_for(p: &Palette) -> egui::Visuals {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let r = CornerRadius::same(4);

    v.panel_fill = p.content;
    v.window_fill = p.toolbar;
    v.extreme_bg_color = p.field_bg;
    v.faint_bg_color = if p.dark { rgb(0x25, 0x27, 0x2b) } else { rgb(0xf7, 0xf8, 0xf9) };
    v.code_bg_color = p.toolbar;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(70) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(60) };
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.danger;
    v.override_text_color = Some(p.text);
    v.weak_text_alpha = 0.7;
    v.selection.bg_fill = if p.dark { rgb(0x2a, 0x55, 0x93) } else { rgb(0xb6, 0xd4, 0xf7) };
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let button_bg = if p.dark { rgb(0x3a, 0x3d, 0x43) } else { rgb(0xfb, 0xfb, 0xfb) };
    let button_hover = if p.dark { rgb(0x45, 0x49, 0x50) } else { rgb(0xf0, 0xf1, 0xf3) };
    let button_down = if p.dark { rgb(0x50, 0x54, 0x5c) } else { rgb(0xe2, 0xe4, 0xe8) };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.content;
    w.noninteractive.weak_bg_fill = p.toolbar;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = button_bg;
    w.inactive.weak_bg_fill = button_bg;
    w.inactive.bg_stroke = Stroke::new(1.0, p.field_border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = button_hover;
    w.hovered.weak_bg_fill = button_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.text_faint);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = button_down;
    w.active.weak_bg_fill = button_down;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = button_down;
    w.open.weak_bg_fill = button_down;
    w.open.bg_stroke = Stroke::new(1.0, p.field_border);
    w.open.corner_radius = r;
    v
}

/// Installs fonts, text sizes, spacing and both colour themes. Call once at start-up.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Light, visuals_for(&Palette::LIGHT));
    ctx.set_visuals_of(Theme::Dark, visuals_for(&Palette::DARK));
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Heading, FontId::proportional(16.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 3.0);
        s.spacing.interact_size = Vec2::new(24.0, 22.0);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.window_margin = Margin::same(12);
        s.spacing.scroll.bar_width = 8.0;
        s.spacing.scroll.floating = true;
        s.interaction.selectable_labels = false;
    });
    ctx.set_theme(egui::ThemePreference::System);
}

/// Uses the macOS system fonts when they exist (they are read at run time, never bundled); on
/// other systems egui's built-in fonts stay.
fn install_fonts(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    {
        use egui::{FontData, FontDefinitions, FontFamily};
        use std::sync::Arc;
        let mut fonts = FontDefinitions::default();
        let mut add = |name: &str, path: &str, family: FontFamily| {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                fonts.families.entry(family).or_default().insert(0, name.to_owned());
            }
        };
        add("sf-ui", "/System/Library/Fonts/SFNS.ttf", FontFamily::Proportional);
        add("menlo", "/System/Library/Fonts/Menlo.ttc", FontFamily::Monospace);
        ctx.set_fonts(fonts);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = ctx;
}

/// Switch between following the system, light and dark.
pub fn set_preference(ctx: &egui::Context, pref: egui::ThemePreference) {
    ctx.set_theme(pref);
}
