//! The connection screen (SPEC-1 section 3): favorites sidebar on the left, the form on the right.
//!
//! Edits live in `ConnectState::form`, a full copy of the selected favorite. "Changed" simply
//! means the form differs from the stored favorite. Connecting goes through
//! `Instance::start_connect` (the medis-core worker); the favorite is saved once the connection
//! succeeds.

use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Align, Layout, Margin, RichText, Ui, Vec2};
use medis_core::ConnectionConfig;

use crate::instance::{Cx, Instance, Phase};
use crate::ui::modal::ModalSpec;
use crate::ui::theme::{self, Palette};

const FORM_WIDTH: f32 = 500.0;
const LABEL_WIDTH: f32 = 140.0;

#[derive(Clone, Copy)]
pub struct Drag {
    pub from: usize,
    pub to: usize,
}

/// A finished background file pick.
enum Pick {
    TlsKey { path: String, contents: String },
    TlsCert { path: String, contents: String },
    TlsCa { path: String, contents: String },
    SshKey { path: String, contents: String },
    Failed(String),
}

pub struct ConnectState {
    /// Key of the selected favorite; `None` means Quick Connect.
    pub selected: Option<String>,
    /// The form values (a copy of the favorite plus unsaved edits).
    pub form: ConnectionConfig,
    pub drag: Option<Drag>,
    nodes_text: String,
    /// Config to store once the connection succeeds.
    pending_save: Option<ConnectionConfig>,
    pick_tx: Sender<Pick>,
    pick_rx: Receiver<Pick>,
}

impl Default for ConnectState {
    fn default() -> Self {
        let (pick_tx, pick_rx) = channel();
        Self {
            selected: None,
            form: ConnectionConfig::quick(),
            drag: None,
            nodes_text: String::new(),
            pending_save: None,
            pick_tx,
            pick_rx,
        }
    }
}

impl ConnectState {
    /// Selects a favorite (or Quick Connect for `None`). Discards unsaved edits.
    pub fn select(&mut self, cx: &mut Cx, key: Option<String>) {
        let found = key.as_deref().and_then(|k| cx.favs.get(k)).cloned();
        match found {
            Some(f) => {
                self.selected = Some(f.key.clone());
                self.form = f;
            }
            None => {
                self.selected = None;
                self.form = ConnectionConfig::quick();
            }
        }
        self.nodes_text = self.form.cluster.nodes.join("\n");
        self.pending_save = None;
    }

    /// Creates a `New Favorite`, saves it and selects it.
    pub fn add_favorite(&mut self, cx: &mut Cx) {
        self.save_new(cx, ConnectionConfig::new_favorite());
    }

    fn save_new(&mut self, cx: &mut Cx, mut fav: ConnectionConfig) {
        fav.key = format!("favorite-{}", uuid::Uuid::new_v4().simple());
        match cx.storage.create_favorite(fav) {
            Ok(key) => {
                cx.favs.reload(cx.storage);
                self.select(cx, Some(key));
            }
            Err(e) => cx.alert(format!("Could not save the favorite: {e}")),
        }
    }

    /// True when a favorite is selected and the form differs from what is stored.
    pub fn changed(&self, cx: &Cx) -> bool {
        match self.selected.as_deref().and_then(|k| cx.favs.get(k)) {
            Some(stored) => *stored != self.form,
            None => false,
        }
    }

    /// Called by the instance when the worker reports a successful connection.
    pub fn on_connected(&mut self, cx: &mut Cx) {
        if let Some(cfg) = self.pending_save.take() {
            match cx.storage.update_favorite(&cfg) {
                Ok(()) => {
                    cx.favs.reload(cx.storage);
                    self.form = cfg;
                }
                Err(e) => cx.alert(format!("Could not save the favorite: {e}")),
            }
        }
    }

    fn poll_picks(&mut self, cx: &mut Cx) {
        while let Ok(p) = self.pick_rx.try_recv() {
            match p {
                Pick::TlsKey { path, contents } => {
                    self.form.tls_key = contents;
                    self.form.tls_key_file = path;
                }
                Pick::TlsCert { path, contents } => {
                    self.form.tls_cert = contents;
                    self.form.tls_cert_file = path;
                }
                Pick::TlsCa { path, contents } => {
                    self.form.tls_ca = contents;
                    self.form.tls_ca_file = path;
                }
                Pick::SshKey { path, contents } => {
                    self.form.ssh_key = Some(contents);
                    self.form.ssh_key_file = Some(path);
                }
                Pick::Failed(msg) => cx.alert(msg),
            }
        }
    }

    fn pick_file(&self, cx: &Cx, title: &'static str, dir: Option<std::path::PathBuf>, make: fn(String, String) -> Pick) {
        let tx = self.pick_tx.clone();
        let ctx = cx.ctx.clone();
        cx.rt.spawn(async move {
            let mut dlg = rfd::AsyncFileDialog::new().set_title(title);
            if let Some(d) = dir {
                dlg = dlg.set_directory(d);
            }
            if let Some(file) = dlg.pick_file().await {
                let path = file.path().to_string_lossy().into_owned();
                let msg = match std::fs::read_to_string(file.path()) {
                    Ok(contents) => make(path, contents),
                    Err(e) => Pick::Failed(format!("Could not read {path}: {e}")),
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            }
        });
    }
}

/// Asks to delete the selected favorite (confirm dialog first).
pub fn request_delete(cx: &mut Cx) {
    cx.modal(
        ModalSpec::confirm(
            "Delete the bookmark?",
            "Delete",
            "Are you sure you want to delete the selected bookmark? This action cannot be undone.",
        ),
        |inst, cx, out| {
            if !out.is_ok() {
                return;
            }
            let Some(key) = inst.connect.selected.clone() else { return };
            let prev = cx.favs.index_of(&key).and_then(|i| i.checked_sub(1)).and_then(|i| cx.favs.list.get(i)).map(|f| f.key.clone());
            if let Err(e) = cx.storage.remove_favorite(&key) {
                cx.alert(format!("Could not delete the favorite: {e}"));
                return;
            }
            cx.favs.reload(cx.storage);
            inst.connect.select(cx, prev);
        },
    );
}

/// Starts connecting with the form's values.
pub fn connect_now(inst: &mut Instance, cx: &mut Cx) {
    if inst.phase != Phase::Form {
        return;
    }
    let mut cfg = inst.connect.form.with_defaults();
    if inst.connect.selected.is_none() {
        // The title of a quick connection is just host:port.
        cfg.name.clear();
    }
    inst.connect.pending_save = if inst.connect.changed(cx) {
        let mut saved = inst.connect.form.with_defaults();
        saved.key = cfg.key.clone();
        Some(saved)
    } else {
        None
    };
    inst.start_connect(cx, &cfg);
}

/// Draws the whole connection screen (sidebar + form).
pub fn show(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) {
    inst.connect.poll_picks(cx);
    // A favorite deleted from another window: fall back to Quick Connect.
    if let Some(k) = inst.connect.selected.clone()
        && cx.favs.get(&k).is_none()
        && inst.phase == Phase::Form
    {
        inst.connect.select(cx, None);
    }

    let pal = cx.pal;
    egui::Panel::left("favorites")
        .default_size(220.0)
        .size_range(180.0..=320.0)
        .resizable(true)
        .frame(egui::Frame::new().fill(pal.sidebar))
        .show(ui, |ui| crate::ui::favorites::show(ui, inst, cx));

    egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.content)).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let avail = ui.available_width();
            let side = ((avail - FORM_WIDTH) / 2.0).max(16.0);
            ui.add_space(36.0);
            ui.horizontal_top(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width((avail - side * 2.0).min(FORM_WIDTH));
                    form(ui, inst, cx, &pal);
                });
            });
            ui.add_space(24.0);
        });
    });
}

fn label_cell(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.allocate_ui_with_layout(Vec2::new(LABEL_WIDTH, 22.0), Layout::right_to_left(Align::Center), |ui| {
        ui.label(RichText::new(text).color(pal.text_dim));
    });
}

/// A labelled row: right-aligned label, then whatever `add` draws.
fn field_row<R>(ui: &mut Ui, pal: &Palette, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        label_cell(ui, pal, label);
        add(ui)
    })
    .inner
}

fn text_row(ui: &mut Ui, pal: &Palette, label: &str, value: &mut String, hint: &str, password: bool) -> egui::Response {
    field_row(ui, pal, label, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .password(password)
                .desired_width(ui.available_width())
                .margin(Margin::symmetric(7, 4)),
        )
    })
}

fn form(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx, pal: &Palette) {
    let busy = inst.phase == Phase::Connecting;
    let is_fav = inst.connect.selected.is_some();
    let changed = inst.connect.changed(cx);
    let mut submit = false;
    let mut save = false;
    let mut duplicate = false;
    let mut add_fav = false;

    // Heading.
    ui.horizontal(|ui| {
        ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
        let title = if is_fav {
            if inst.connect.form.name.trim().is_empty() { "Untitled" } else { inst.connect.form.name.as_str() }
        } else {
            "Quick Connect"
        };
        ui.label(RichText::new(title).size(20.0).strong());
    });
    ui.add_space(8.0);

    ui.spacing_mut().item_spacing.y = 9.0;

    ui.add_enabled_ui(!busy, |ui| {
        let ConnectState { form, nodes_text, .. } = &mut inst.connect;
        let mut enter = false;

        if is_fav {
            let r = text_row(ui, pal, "Name:", &mut form.name, "Bookmark name", false);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        }
        let r = text_row(ui, pal, "Redis Host:", &mut form.host, "localhost", false);
        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let r = text_row(ui, pal, "Port:", &mut form.port, "6379", false);
        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let r = text_row(ui, pal, "Username:", &mut form.username, "Optional (Redis 6 ACL)", false);
        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let r = text_row(ui, pal, "Password:", &mut form.password, "", true);
        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        // Cluster.
        field_row(ui, pal, "Cluster:", |ui| {
            ui.checkbox(&mut form.cluster.enabled, "Connect to a Redis Cluster");
        });
        if form.cluster.enabled {
            field_row(ui, pal, "Seed Nodes:", |ui| {
                let r = ui.add(
                    egui::TextEdit::multiline(nodes_text)
                        .hint_text("host:port, one per line (optional)")
                        .desired_rows(2)
                        .desired_width(ui.available_width())
                        .margin(Margin::symmetric(7, 4)),
                );
                if r.changed() {
                    form.cluster.nodes = nodes_text
                        .split(['\n', ','])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            });
            if form.ssh {
                field_row(ui, pal, "", |ui| {
                    ui.label(RichText::new("An SSH tunnel cannot be used with a cluster.").size(12.0).color(pal.danger));
                });
            }
        }

        // SSL.
        field_row(ui, pal, "SSL:", |ui| {
            ui.checkbox(&mut form.ssl, "");
        });
        if form.ssl {
            let mut want: Option<u8> = None;
            let rows: [(&str, &str, bool, u8); 3] = [
                ("Private Key:", "Select Private Key File (PEM)", !form.tls_key_file.is_empty(), 0),
                ("Certificate:", "Select Certificate File (PEM)", !form.tls_cert_file.is_empty(), 1),
                ("CA:", "Select CA File (PEM)", !form.tls_ca_file.is_empty(), 2),
            ];
            for (label, hint, set, id) in rows {
                field_row(ui, pal, label, |ui| {
                    let path = match id {
                        0 => &mut form.tls_key_file,
                        1 => &mut form.tls_cert_file,
                        _ => &mut form.tls_ca_file,
                    };
                    let mut shown = path.clone();
                    let w = ui.available_width() - 64.0;
                    ui.add(
                        egui::TextEdit::singleline(&mut shown)
                            .hint_text(hint)
                            .interactive(false)
                            .desired_width(w)
                            .margin(Margin::symmetric(7, 4)),
                    );
                    if ui.button("…").on_hover_text("Choose a file").clicked() {
                        want = Some(id);
                    }
                    if set && ui.small_button("×").on_hover_text("Clear").clicked() {
                        match id {
                            0 => {
                                form.tls_key.clear();
                                form.tls_key_file.clear();
                            }
                            1 => {
                                form.tls_cert.clear();
                                form.tls_cert_file.clear();
                            }
                            _ => {
                                form.tls_ca.clear();
                                form.tls_ca_file.clear();
                            }
                        }
                    }
                });
            }
            let mut verify = !form.tls.insecure;
            field_row(ui, pal, "TLS:", |ui| {
                if ui.checkbox(&mut verify, "Verify certificate").changed() {
                    form.tls.insecure = !verify;
                }
                if verify {
                    ui.checkbox(&mut form.tls.skip_hostname, "Skip hostname check");
                }
            });
            if let Some(id) = want {
                match id {
                    0 => inst.connect.pick_file(cx, "Select Private Key File (PEM)", None, |path, contents| Pick::TlsKey { path, contents }),
                    1 => inst.connect.pick_file(cx, "Select Certificate File (PEM)", None, |path, contents| Pick::TlsCert { path, contents }),
                    _ => inst.connect.pick_file(cx, "Select CA File (PEM)", None, |path, contents| Pick::TlsCa { path, contents }),
                }
            }
        }

        // SSH tunnel.
        let ConnectState { form, .. } = &mut inst.connect;
        field_row(ui, pal, "SSH Tunnel:", |ui| {
            ui.checkbox(&mut form.ssh, "");
        });
        let mut pick_ssh = false;
        if form.ssh {
            let r = text_row(ui, pal, "SSH Host:", &mut form.ssh_host, "", false);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let r = text_row(ui, pal, "SSH User:", &mut form.ssh_user, "", false);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            let has_key = form.ssh_key.as_deref().is_some_and(|k| !k.is_empty());
            let label = if has_key { "SSH Key:" } else { "SSH Password:" };
            field_row(ui, pal, label, |ui| {
                let w = ui.available_width() - 34.0;
                if has_key {
                    let mut path = form.ssh_key_file.clone().unwrap_or_default();
                    ui.add(egui::TextEdit::singleline(&mut path).interactive(false).desired_width(w).margin(Margin::symmetric(7, 4)));
                } else {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut form.ssh_password)
                            .password(true)
                            .desired_width(w)
                            .margin(Margin::symmetric(7, 4)),
                    );
                    enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }
                let (glyph, tip) = if has_key { ("×", "Remove the key") } else { ("🔑", "Choose a private key file") };
                if ui.button(glyph).on_hover_text(tip).clicked() {
                    if has_key {
                        form.ssh_key = None;
                        form.ssh_key_file = None;
                        form.ssh_key_passphrase.clear();
                    } else {
                        pick_ssh = true;
                    }
                }
            });
            if has_key && form.ssh_key_is_encrypted() {
                let r = text_row(ui, pal, "SSH Key Passphrase:", &mut form.ssh_key_passphrase, "", true);
                enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            let r = text_row(ui, pal, "SSH Port:", &mut form.ssh_port, "22", false);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        }
        if pick_ssh {
            let dir = dirs::home_dir().map(|h| h.join(".ssh")).filter(|d| d.is_dir());
            inst.connect.pick_file(cx, "Select a private key (Most often in the ~/.ssh)", dir, |path, contents| Pick::SshKey { path, contents });
        }
        if enter && !busy {
            submit = true;
        }
    });

    // Buttons.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
        ui.add_enabled_ui(!busy, |ui| {
            if is_fav {
                if ui.button("Duplicate").clicked() {
                    duplicate = true;
                }
            } else if ui.button("Add to Favorite").clicked() {
                add_fav = true;
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let min = Vec2::new(96.0, 26.0);
            if busy {
                let label = if inst.status.is_empty() { "Redis connecting..." } else { inst.status.as_str() };
                ui.add_enabled(false, theme::primary_button(pal, label).min_size(Vec2::new(150.0, 26.0)));
                ui.add(egui::Spinner::new().size(14.0));
            } else {
                let text = if changed { "Save and Connect" } else { "Connect" };
                let w = if changed { 140.0 } else { min.x };
                if ui.add(theme::primary_button(pal, text).min_size(Vec2::new(w, min.y))).clicked() {
                    submit = true;
                }
                if changed && ui.add(egui::Button::new("Save Changes").min_size(Vec2::new(0.0, min.y))).clicked() {
                    save = true;
                }
            }
        });
    });

    if save {
        let form = inst.connect.form.clone();
        match cx.storage.update_favorite(&form) {
            Ok(()) => cx.favs.reload(cx.storage),
            Err(e) => cx.alert(format!("Could not save the favorite: {e}")),
        }
    }
    if duplicate {
        let fav = inst.connect.form.clone();
        inst.connect.save_new(cx, fav);
    }
    if add_fav {
        let mut fav = inst.connect.form.clone();
        fav.name = "Quick Connect".into();
        inst.connect.save_new(cx, fav);
    }
    if submit {
        connect_now(inst, cx);
    }
}
