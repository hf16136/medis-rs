//! The app shell: instances (tabs), menus, shortcuts, window title, the connected-phase frame.
//!
//! Menus: on macOS the native menu bar is built with `muda`; on other systems an in-window menu
//! bar is drawn with egui. Both produce the same [`Cmd`]s.

use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Align, CursorIcon, Frame, Key, Layout, Margin, Modifiers, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportCommand};
use medis_core::storage::Storage;

use crate::instance::{AppAction, Cx, Instance, MainTab, Phase};
use crate::ui::favorites::Favorites;
use crate::ui::modal::{Done, Modals};
use crate::ui::theme::{self, Palette};
use crate::ui::widgets::{self, TabIcon};
use crate::ui::{config, connect, content, footer, keys, patterns, terminal};

/// Everything a menu item or shortcut can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    NewWindow,
    NewTab,
    CloseWindow,
    CloseTab,
    SelectTab(usize),
    SelectLastTab,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ThemeSystem,
    ThemeLight,
    ThemeDark,
    ReportIssue,
    LearnMore,
}

pub struct MedisApp {
    /// Kept alive for the whole run; workers are tasks on it.
    _rt: tokio::runtime::Runtime,
    rt_handle: tokio::runtime::Handle,
    storage: Storage,
    favs: Favorites,
    modals: Modals,
    instances: Vec<Instance>,
    active: usize,
    next_id: u64,
    actions: Vec<AppAction>,
    cmd_rx: Receiver<Cmd>,
    /// Kept so the in-window menu can push commands too.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    cmd_tx: Sender<Cmd>,
    /// Events to feed egui next frame (the Edit menu).
    inject: Vec<egui::Event>,
    last_title: String,
    last_fav_poll: f64,
    #[cfg(target_os = "macos")]
    _menu: Option<muda::Menu>,
}

impl MedisApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Runtime) -> Self {
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut app = Self::with_storage(&cc.egui_ctx, rt, Storage::open_default());
        #[cfg(target_os = "macos")]
        {
            app._menu = native_menu::install(&cc.egui_ctx, app.cmd_tx.clone());
        }
        app
    }

    /// Builds the app on an explicit storage, without the native menu (also used by the tests).
    pub fn with_storage(ctx: &egui::Context, rt: tokio::runtime::Runtime, storage: Storage) -> Self {
        theme::install(ctx);
        let (cmd_tx, cmd_rx) = channel();
        let favs = Favorites::load(&storage);
        let rt_handle = rt.handle().clone();

        Self {
            _rt: rt,
            rt_handle,
            storage,
            favs,
            modals: Modals::default(),
            instances: vec![Instance::new(1)],
            active: 0,
            next_id: 2,
            actions: Vec::new(),
            cmd_rx,
            cmd_tx,
            inject: Vec::new(),
            last_title: String::new(),
            last_fav_poll: 0.0,
            #[cfg(target_os = "macos")]
            _menu: None,
        }
    }

    fn new_instance(&mut self) {
        let inst = Instance::new(self.next_id);
        self.next_id += 1;
        self.instances.push(inst);
        self.active = self.instances.len() - 1;
    }

    fn close_instance(&mut self, ctx: &egui::Context, id: u64) {
        let Some(i) = self.instances.iter().position(|x| x.id == id) else { return };
        if self.instances.len() == 1 {
            self.instances[0].disconnect();
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        // Disconnects the worker too (the original leaked the connection).
        self.instances[i].disconnect();
        self.instances.remove(i);
        if i < self.active {
            self.active -= 1;
        } else if i == self.active {
            // Next tab, or else the previous one.
            self.active = i.min(self.instances.len() - 1);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, cmd: Cmd) {
        let modal = self.modals.is_open();
        match cmd {
            // Instance actions are ignored while a dialog is open.
            Cmd::NewTab | Cmd::CloseTab | Cmd::SelectTab(_) | Cmd::SelectLastTab if modal => {}
            Cmd::NewTab => self.new_instance(),
            Cmd::CloseTab => {
                let id = self.instances[self.active].id;
                self.close_instance(ctx, id);
            }
            Cmd::SelectTab(i) => {
                if i < self.instances.len() {
                    self.active = i;
                }
            }
            Cmd::SelectLastTab => self.active = self.instances.len() - 1,
            Cmd::NewWindow => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
            }
            Cmd::CloseWindow => ctx.send_viewport_cmd(ViewportCommand::Close),
            Cmd::Undo => self.inject_key(Key::Z, false),
            Cmd::Redo => self.inject_key(Key::Z, true),
            Cmd::SelectAll => self.inject_key(Key::A, false),
            Cmd::Cut => self.inject.push(egui::Event::Cut),
            Cmd::Copy => self.inject.push(egui::Event::Copy),
            Cmd::Paste => ctx.send_viewport_cmd(ViewportCommand::RequestPaste),
            Cmd::ThemeSystem => theme::set_preference(ctx, egui::ThemePreference::System),
            Cmd::ThemeLight => theme::set_preference(ctx, egui::ThemePreference::Light),
            Cmd::ThemeDark => theme::set_preference(ctx, egui::ThemePreference::Dark),
            Cmd::ReportIssue => ctx.open_url(egui::OpenUrl::new_tab("mailto:medis@zihua.li")),
            Cmd::LearnMore => ctx.open_url(egui::OpenUrl::new_tab("http://getmedis.com")),
        }
        // Injected events are delivered by raw_input_hook on the next frame.
        if !self.inject.is_empty() {
            ctx.request_repaint();
        }
    }

    fn inject_key(&mut self, key: Key, shift: bool) {
        // egui widgets test the logical `command` flag, so it must be set; the
        // physical flag (mac_cmd / ctrl) is set too, as winit would.
        let mut modifiers = Modifiers::COMMAND;
        if cfg!(target_os = "macos") {
            modifiers.mac_cmd = true;
        } else {
            modifiers.ctrl = true;
        }
        modifiers.shift = shift;
        self.inject.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    }

    fn apply_actions(&mut self, ctx: &egui::Context) {
        for a in std::mem::take(&mut self.actions) {
            match a {
                AppAction::NewInstance => self.new_instance(),
                AppAction::CloseInstance(id) => self.close_instance(ctx, id),
            }
        }
    }

    fn poll_storage(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.last_fav_poll > 1.0 {
            self.last_fav_poll = now;
            self.favs.poll(&self.storage);
        }
        // Wake up once a second even when idle, to notice changes from other windows.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }

    fn finish_modal(&mut self, ctx: &egui::Context, done: Done) {
        let Done { inst_id, callback, outcome } = done;
        let Some(cb) = callback else { return };
        let Some(inst) = self.instances.iter_mut().find(|i| i.id == inst_id) else { return };
        let mut cx = make_cx(ctx, &self.storage, &mut self.favs, &mut self.modals, &self.rt_handle, inst_id);
        cb(inst, &mut cx, outcome);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = self.instances[self.active].window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ---- chrome -------------------------------------------------------------------------------

    /// In-window menu bar (not used on macOS, which has the native one).
    #[cfg(not(target_os = "macos"))]
    fn menu_bar(&mut self, ui: &mut Ui, pal: &Palette) {
        let tx = self.cmd_tx.clone();
        egui::Panel::top("menubar")
            .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin::symmetric(6, 2)))
            .show(ui, |ui| {
                let send = |c: Cmd| {
                    let _ = tx.send(c);
                };
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.add(egui::Button::new("New Connection Window").shortcut_text("Ctrl+N")).clicked() {
                            send(Cmd::NewWindow);
                        }
                        if ui.add(egui::Button::new("New Connection Tab").shortcut_text("Ctrl+T")).clicked() {
                            send(Cmd::NewTab);
                        }
                        ui.separator();
                        if ui.add(egui::Button::new("Close Window").shortcut_text("Ctrl+Shift+W")).clicked() {
                            send(Cmd::CloseWindow);
                        }
                        if ui.add(egui::Button::new("Close Tab").shortcut_text("Ctrl+W")).clicked() {
                            send(Cmd::CloseTab);
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        for (label, cmd, sc) in [
                            ("Undo", Cmd::Undo, "Ctrl+Z"),
                            ("Redo", Cmd::Redo, "Ctrl+Shift+Z"),
                            ("Cut", Cmd::Cut, "Ctrl+X"),
                            ("Copy", Cmd::Copy, "Ctrl+C"),
                            ("Paste", Cmd::Paste, "Ctrl+V"),
                            ("Select All", Cmd::SelectAll, "Ctrl+A"),
                        ] {
                            if ui.add(egui::Button::new(label).shortcut_text(sc)).clicked() {
                                send(cmd);
                            }
                        }
                    });
                    ui.menu_button("View", |ui| {
                        if ui.add(egui::Button::new("Toggle Full Screen").shortcut_text("F11")).clicked() {
                            let full = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
                            ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(!full));
                        }
                        ui.separator();
                        if ui.button("Appearance: Match System").clicked() {
                            send(Cmd::ThemeSystem);
                        }
                        if ui.button("Appearance: Light").clicked() {
                            send(Cmd::ThemeLight);
                        }
                        if ui.button("Appearance: Dark").clicked() {
                            send(Cmd::ThemeDark);
                        }
                    });
                    ui.menu_button("Window", |ui| {
                        if ui.add(egui::Button::new("Minimize").shortcut_text("Ctrl+M")).clicked() {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                    });
                    ui.menu_button("Help", |ui| {
                        if ui.button("Report an Issue...").clicked() {
                            send(Cmd::ReportIssue);
                        }
                        if ui.button("Learn More").clicked() {
                            send(Cmd::LearnMore);
                        }
                    });
                });
            });
    }

    /// The strip of instance tabs. Hidden when there is only one instance.
    fn tab_strip(&mut self, ui: &mut Ui, pal: &Palette) {
        if self.instances.len() < 2 {
            return;
        }
        let blocked = self.modals.is_open();
        let mut select: Option<usize> = None;
        let mut close: Option<u64> = None;
        let mut new_tab = false;
        let mut move_to: Option<(usize, usize)> = None;

        egui::Panel::top("instance-tabs")
            .exact_size(32.0)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.window))
            .show(ui, |ui| {
                let tab_id = egui::Id::unique("instance-tab-ids");
                let full = ui.max_rect();
                ui.painter().line_segment([full.left_bottom(), full.right_bottom()], Stroke::new(1.0, pal.border));
                let n = self.instances.len();
                let avail = full.width() - 44.0;
                let w = (avail / n as f32).clamp(110.0, 220.0);
                let top = full.top() + 4.0;
                for (i, inst) in self.instances.iter().enumerate() {
                    let rect = Rect::from_min_size(Pos2::new(full.left() + 8.0 + i as f32 * w, top), Vec2::new(w - 2.0, 28.0));
                    let active = i == self.active;
                    let resp = ui.interact(rect, tab_id.with(("tab", inst.id)), Sense::click_and_drag());
                    let close_rect = Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(16.0));
                    let close_resp = ui.interact(close_rect, tab_id.with(("tab-close", inst.id)), Sense::click());

                    let corner = egui::CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 };
                    if active {
                        ui.painter().rect_filled(rect, corner, pal.content);
                        ui.painter().rect_stroke(rect, corner, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                        // Open the bottom edge so the tab merges with the page.
                        ui.painter().line_segment(
                            [Pos2::new(rect.left() + 1.0, rect.bottom()), Pos2::new(rect.right() - 1.0, rect.bottom())],
                            Stroke::new(2.0, pal.content),
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, corner, pal.hover);
                    }
                    let fg = if active { pal.text } else { pal.text_dim };
                    let text_rect = Rect::from_min_max(Pos2::new(rect.left() + 12.0, rect.top()), Pos2::new(close_rect.left() - 2.0, rect.bottom()));
                    ui.painter().with_clip_rect(text_rect).text(
                        Pos2::new(text_rect.left(), rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &inst.title,
                        egui::FontId::proportional(12.5),
                        fg,
                    );
                    if close_resp.hovered() {
                        ui.painter().circle_filled(close_rect.center(), 8.0, pal.hover.gamma_multiply(1.4));
                    }
                    if active || resp.hovered() || close_resp.hovered() {
                        widgets::paint_cross(ui.painter(), close_rect, if close_resp.hovered() { pal.text } else { pal.text_dim });
                    }
                    if !blocked {
                        if close_resp.clicked() {
                            close = Some(inst.id);
                        } else if resp.is_pointer_button_down_on() {
                            // A tab is selected on mouse down.
                            select = Some(i);
                        }
                        if resp.dragged() {
                            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                            if let Some(p) = ui.input(|inp| inp.pointer.interact_pos()) {
                                let target = (((p.x - full.left() - 8.0) / w).floor().max(0.0) as usize).min(n - 1);
                                if target != i {
                                    move_to = Some((i, target));
                                }
                            }
                        }
                    }
                }
                // The trailing + button.
                let plus = Rect::from_center_size(Pos2::new(full.left() + 8.0 + n as f32 * w + 14.0, top + 14.0), Vec2::splat(22.0));
                let r = ui.interact(plus, tab_id.with("tab-plus"), Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(plus, 5.0, pal.hover);
                }
                widgets::paint_plus(ui.painter(), plus, if r.hovered() { pal.text } else { pal.text_dim }, false);
                if r.on_hover_text("New connection tab").clicked() && !blocked {
                    new_tab = true;
                }
            });

        if let Some(i) = select {
            self.active = i;
        }
        if let Some((from, to)) = move_to {
            let inst = self.instances.remove(from);
            self.instances.insert(to, inst);
            self.active = to;
        }
        if let Some(id) = close {
            self.actions.push(AppAction::CloseInstance(id));
        }
        if new_tab {
            self.actions.push(AppAction::NewInstance);
        }
    }
}

fn make_cx<'a>(
    ctx: &'a egui::Context,
    storage: &'a Storage,
    favs: &'a mut Favorites,
    modals: &'a mut Modals,
    rt: &'a tokio::runtime::Handle,
    inst_id: u64,
) -> Cx<'a> {
    let blocked = modals.is_open();
    Cx { ctx, pal: Palette::of(ctx), storage, favs, modals, rt, inst_id, blocked }
}

/// Keyboard shortcuts, for every platform. (On macOS the native menu takes the ones it owns
/// first; the digit shortcuts always arrive here.)
fn shortcuts(ctx: &egui::Context) -> Vec<Cmd> {
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        let cmd = Modifiers::COMMAND;
        if i.consume_key(cmd | Modifiers::SHIFT, Key::W) {
            out.push(Cmd::CloseWindow);
        } else if i.consume_key(cmd, Key::W) {
            out.push(Cmd::CloseTab);
        }
        if i.consume_key(cmd, Key::T) {
            out.push(Cmd::NewTab);
        }
        if i.consume_key(cmd, Key::N) {
            out.push(Cmd::NewWindow);
        }
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8];
        for (n, key) in digits.into_iter().enumerate() {
            let hit = i.consume_key(cmd, key) || (!cfg!(target_os = "macos") && i.consume_key(Modifiers::ALT, key));
            if hit {
                out.push(Cmd::SelectTab(n));
            }
        }
        if i.consume_key(cmd, Key::Num9) || (!cfg!(target_os = "macos") && i.consume_key(Modifiers::ALT, Key::Num9)) {
            out.push(Cmd::SelectLastTab);
        }
        out
    })
}

impl eframe::App for MedisApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Worker events of every instance (also the ones in background tabs).
        for inst in &mut self.instances {
            let id = inst.id;
            let mut cx = make_cx(ctx, &self.storage, &mut self.favs, &mut self.modals, &self.rt_handle, id);
            inst.drain_events(&mut cx);
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.inject);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let pal = Palette::of(&ctx);
        ui.painter().rect_filled(ui.max_rect(), 0.0, pal.window);

        self.poll_storage(&ctx);

        let mut cmds: Vec<Cmd> = Vec::new();
        while let Ok(c) = self.cmd_rx.try_recv() {
            cmds.push(c);
        }
        // A dialog blocks every shortcut, the tab ones included.
        if !self.modals.is_open() {
            cmds.extend(shortcuts(&ctx));
        }
        cmds.dedup();
        for c in cmds {
            self.apply(&ctx, c);
        }

        #[cfg(not(target_os = "macos"))]
        self.menu_bar(ui, &pal);
        self.tab_strip(ui, &pal);

        let active = self.active.min(self.instances.len() - 1);
        self.active = active;
        {
            let inst = &mut self.instances[active];
            let id = inst.id;
            let mut cx = make_cx(&ctx, &self.storage, &mut self.favs, &mut self.modals, &self.rt_handle, id);
            if inst.phase == Phase::Connected {
                show_connected(ui, inst, &mut cx);
                patterns::show(&mut cx, inst);
            } else {
                connect::show(ui, inst, &mut cx);
            }
        }

        if let Some(done) = self.modals.show(&ctx, &pal) {
            self.finish_modal(&ctx, done);
        }
        self.apply_actions(&ctx);
        self.update_title(&ctx);
    }
}

/// The connected-phase frame: key browser on the left; tab bar, page and footer on the right.
fn show_connected(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) {
    let pal = cx.pal;
    let id = inst.id;

    egui::Panel::left(("key-browser", id))
        .default_size(260.0)
        .size_range(250.0..=640.0)
        .resizable(true)
        .frame(Frame::new().fill(pal.sidebar))
        .show(ui, |ui| keys::show(ui, inst, cx));

    egui::Panel::top(("main-tabs", id))
        .exact_size(36.0)
        .show_separator_line(false)
        .frame(Frame::new().fill(pal.toolbar))
        .show(ui, |ui| main_tab_bar(ui, inst, &pal));

    // The footer describes the selected key, so it belongs under the Content tab only.
    if inst.tab == MainTab::Content {
        egui::Panel::bottom(("footer", id))
            .exact_size(26.0)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().line_segment(
                    [Pos2::new(r.left() - 10.0, r.top()), Pos2::new(r.right() + 10.0, r.top())],
                    Stroke::new(1.0, pal.border),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| footer::show(ui, inst, cx));
            });
    }

    egui::CentralPanel::default().frame(Frame::new().fill(pal.content)).show(ui, |ui| match inst.tab {
        MainTab::Content => content::show(ui, inst, cx),
        MainTab::Terminal => terminal::show(ui, inst, cx),
        MainTab::Config => config::show(ui, inst, cx),
    });
}

/// `Content | Terminal | Config`, right-aligned, with the active one highlighted.
fn main_tab_bar(ui: &mut Ui, inst: &mut Instance, pal: &Palette) {
    let bar = ui.max_rect();
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, pal.border));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // Right-to-left, so list them backwards.
        let tabs = [(MainTab::Config, "Config", TabIcon::Cog), (MainTab::Terminal, "Terminal", TabIcon::Window), (MainTab::Content, "Content", TabIcon::Book)];
        for (tab, label, icon) in tabs {
            let galley = ui.painter().layout_no_wrap(label.to_owned(), egui::FontId::proportional(13.0), pal.text);
            let width = 12.0 + 16.0 + 6.0 + galley.size().x + 12.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, bar.height() - 1.0), Sense::click());
            let active = inst.tab == tab;
            // Painted by hand, so describe it for screen readers (and the GUI tests).
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, label));
            if active {
                ui.painter().rect_filled(rect, 0.0, pal.tab_active);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, pal.hover);
            }
            let fg = if active { pal.text } else { pal.text_dim };
            let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + 12.0 + 8.0, rect.center().y), Vec2::splat(16.0));
            widgets::paint_tab_icon(ui.painter(), icon_rect, icon, fg);
            ui.painter().text(
                Pos2::new(icon_rect.right() + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(13.0),
                fg,
            );
            if resp.clicked() {
                inst.tab = tab;
            }
        }
    });
}

#[cfg(target_os = "macos")]
mod native_menu {
    use std::sync::mpsc::Sender;

    use muda::accelerator::Accelerator;
    use muda::{AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    use super::Cmd;

    fn acc(s: &str) -> Option<Accelerator> {
        s.parse().ok()
    }

    fn item(id: &str, text: &str, accel: Option<&str>) -> MenuItem {
        MenuItem::with_id(MenuId::new(id), text, true, accel.and_then(acc))
    }

    /// Builds the native menu bar and routes clicks into `tx`. Returns the menu (keep it alive).
    pub fn install(ctx: &egui::Context, tx: Sender<Cmd>) -> Option<Menu> {
        build().inspect(|menu| {
            menu.init_for_nsapp();
            let ctx = ctx.clone();
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                let cmd = match ev.id.0.as_str() {
                    "new_window" => Cmd::NewWindow,
                    "new_tab" => Cmd::NewTab,
                    "close_window" => Cmd::CloseWindow,
                    "close_tab" => Cmd::CloseTab,
                    "undo" => Cmd::Undo,
                    "redo" => Cmd::Redo,
                    "cut" => Cmd::Cut,
                    "copy" => Cmd::Copy,
                    "paste" => Cmd::Paste,
                    "select_all" => Cmd::SelectAll,
                    "theme_system" => Cmd::ThemeSystem,
                    "theme_light" => Cmd::ThemeLight,
                    "theme_dark" => Cmd::ThemeDark,
                    "help_issue" => Cmd::ReportIssue,
                    "help_more" => Cmd::LearnMore,
                    _ => return,
                };
                let _ = tx.send(cmd);
                ctx.request_repaint();
            }));
        })
    }

    fn build() -> Option<Menu> {
        let menu = Menu::new();
        let sep = PredefinedMenuItem::separator;

        let about = AboutMetadata {
            name: Some("Medis".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..Default::default()
        };
        let app = Submenu::with_items(
            "Medis",
            true,
            &[
                &PredefinedMenuItem::about(Some("About Medis"), Some(about)),
                &sep(),
                &PredefinedMenuItem::services(None),
                &sep(),
                &PredefinedMenuItem::hide(Some("Hide Medis")),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &sep(),
                &PredefinedMenuItem::quit(Some("Quit Medis")),
            ],
        )
        .ok()?;

        let file = Submenu::with_items(
            "File",
            true,
            &[
                &item("new_window", "New Connection Window", Some("CmdOrCtrl+N")),
                &item("new_tab", "New Connection Tab", Some("CmdOrCtrl+T")),
                &sep(),
                &item("close_window", "Close Window", Some("Shift+CmdOrCtrl+W")),
                &item("close_tab", "Close Tab", Some("CmdOrCtrl+W")),
            ],
        )
        .ok()?;

        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &item("undo", "Undo", Some("CmdOrCtrl+Z")),
                &item("redo", "Redo", Some("Shift+CmdOrCtrl+Z")),
                &sep(),
                &item("cut", "Cut", Some("CmdOrCtrl+X")),
                &item("copy", "Copy", Some("CmdOrCtrl+C")),
                &item("paste", "Paste", Some("CmdOrCtrl+V")),
                &item("select_all", "Select All", Some("CmdOrCtrl+A")),
            ],
        )
        .ok()?;

        let view = Submenu::with_items(
            "View",
            true,
            &[
                &PredefinedMenuItem::fullscreen(Some("Toggle Full Screen")),
                &sep(),
                &item("theme_system", "Appearance: Match System", None),
                &item("theme_light", "Appearance: Light", None),
                &item("theme_dark", "Appearance: Dark", None),
            ],
        )
        .ok()?;

        let window = Submenu::with_items(
            "Window",
            true,
            &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::bring_all_to_front(None)],
        )
        .ok()?;

        let help = Submenu::with_items(
            "Help",
            true,
            &[&item("help_issue", "Report an Issue...", None), &item("help_more", "Learn More", None)],
        )
        .ok()?;

        menu.append_items(&[&app, &file, &edit, &view, &window, &help]).ok()?;
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();
        Some(menu)
    }
}
