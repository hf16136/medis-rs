//! One modal at a time for the whole window (SPEC-3 8.4): alerts, confirms and small forms.
//!
//! Open them from any module through [`crate::instance::Cx`]:
//!
//! ```ignore
//! cx.alert("Update Failed");
//! cx.modal(ModalSpec::confirm("Delete selected key?", "Delete", "Are you sure ..."), |inst, cx, out| {
//!     if out.is_ok() { inst.send(Module::Keys, Op::Delete(key)); }
//! });
//! cx.modal(
//!     ModalSpec::form("Create Key", "Create Key", "", vec![
//!         Field::text("Key Name:"),
//!         Field::choice("Type:", &["string", "hash", "list", "set", "zset"]),
//!     ]),
//!     |inst, cx, out| if let Outcome::Ok(v) = &out { /* out.text(0), out.text(1) */ },
//! );
//! ```
//!
//! While a dialog is open it blocks every other input, including the app shortcuts.

use std::collections::VecDeque;

use egui::{Align, Align2, Context, CornerRadius, Frame, Id, Key, Layout, Margin, RichText, Sense, Stroke, Vec2};

use crate::instance::{Cx, Instance};
use crate::ui::theme::{Palette, primary_button};
use crate::ui::widgets;

/// Runs when a dialog closes. Gets the instance that opened it (if it still exists).
pub type Callback = Box<dyn FnOnce(&mut Instance, &mut Cx, Outcome)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKind {
    /// Message with a single OK button.
    Alert,
    /// Warning icon + text. Cancel is the default button (Enter = Cancel).
    Confirm,
    /// Labelled inputs. The action button is the default button (Enter submits).
    Form,
}

#[derive(Clone, Debug)]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    Choice(Vec<String>),
}

/// One input of a form dialog. Labels should include the trailing colon (`Key Name:`).
#[derive(Clone, Debug)]
pub struct Field {
    pub label: String,
    pub kind: FieldKind,
    /// Initial value as text (`true`/`false` for Bool, the option for Choice).
    pub initial: String,
}

impl Field {
    pub fn text(label: &str) -> Self {
        Self { label: label.into(), kind: FieldKind::Text, initial: String::new() }
    }

    pub fn number(label: &str, initial: Option<f64>) -> Self {
        let initial = initial.map(fmt_number).unwrap_or_default();
        Self { label: label.into(), kind: FieldKind::Number, initial }
    }

    pub fn flag(label: &str, initial: bool) -> Self {
        Self { label: label.into(), kind: FieldKind::Bool, initial: initial.to_string() }
    }

    /// A pick-one list; the first option is selected at the start.
    pub fn choice(label: &str, options: &[&str]) -> Self {
        let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        let initial = options.first().cloned().unwrap_or_default();
        Self { label: label.into(), kind: FieldKind::Choice(options), initial }
    }
}

fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() }
}

/// What to show.
#[derive(Clone, Debug)]
pub struct ModalSpec {
    pub kind: ModalKind,
    pub title: String,
    pub content: String,
    /// Label of the action button (`OK` by default).
    pub button: String,
    pub fields: Vec<Field>,
}

impl ModalSpec {
    pub fn alert(message: impl Into<String>) -> Self {
        Self { kind: ModalKind::Alert, title: String::new(), content: message.into(), button: "OK".into(), fields: vec![] }
    }

    pub fn confirm(title: &str, button: &str, content: impl Into<String>) -> Self {
        Self { kind: ModalKind::Confirm, title: title.into(), content: content.into(), button: button.into(), fields: vec![] }
    }

    pub fn form(title: &str, button: &str, content: &str, fields: Vec<Field>) -> Self {
        Self { kind: ModalKind::Form, title: title.into(), content: content.into(), button: button.into(), fields }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Number(f64),
    Bool(bool),
}

/// How a dialog ended. For confirm and alert dialogs `Ok` carries no values.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Ok(Vec<FieldValue>),
    Cancel,
}

impl Outcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }

    /// Text (or chosen option) of field `i`; empty when absent.
    pub fn text(&self, i: usize) -> String {
        match self {
            Outcome::Ok(v) => match v.get(i) {
                Some(FieldValue::Text(s)) => s.clone(),
                _ => String::new(),
            },
            Outcome::Cancel => String::new(),
        }
    }

    pub fn number(&self, i: usize) -> f64 {
        match self {
            Outcome::Ok(v) => match v.get(i) {
                Some(FieldValue::Number(n)) => *n,
                _ => 0.0,
            },
            Outcome::Cancel => 0.0,
        }
    }

    pub fn flag(&self, i: usize) -> bool {
        match self {
            Outcome::Ok(v) => matches!(v.get(i), Some(FieldValue::Bool(true))),
            Outcome::Cancel => false,
        }
    }
}

struct Entry {
    seq: u64,
    inst_id: u64,
    spec: ModalSpec,
    callback: Option<Callback>,
    values: Vec<String>,
    flags: Vec<bool>,
    submit_failed: bool,
    opened_at: Option<f64>,
    focus_done: bool,
}

/// A finished dialog, to be handed to the app (which owns the instances).
pub struct Done {
    pub inst_id: u64,
    pub callback: Option<Callback>,
    pub outcome: Outcome,
}

/// The dialog queue. Only the first entry is visible.
#[derive(Default)]
pub struct Modals {
    queue: VecDeque<Entry>,
    seq: u64,
}

impl Modals {
    pub fn is_open(&self) -> bool {
        !self.queue.is_empty()
    }

    pub fn alert(&mut self, inst_id: u64, message: impl Into<String>) {
        self.open(inst_id, ModalSpec::alert(message), None);
    }

    pub fn open(&mut self, inst_id: u64, spec: ModalSpec, callback: Option<Callback>) {
        self.seq += 1;
        let values = spec.fields.iter().map(|f| f.initial.clone()).collect();
        let flags = spec.fields.iter().map(|f| f.initial == "true").collect();
        self.queue.push_back(Entry {
            seq: self.seq,
            inst_id,
            spec,
            callback,
            values,
            flags,
            submit_failed: false,
            opened_at: None,
            focus_done: false,
        });
    }

    /// Draws the front dialog. Returns it when the user closed it.
    pub fn show(&mut self, ctx: &Context, pal: &Palette) -> Option<Done> {
        let entry = self.queue.front_mut()?;
        let outcome = show_entry(ctx, pal, entry);
        let outcome = outcome?;
        let e = self.queue.pop_front()?;
        Some(Done { inst_id: e.inst_id, callback: e.callback, outcome })
    }
}

const SHEET_WIDTH: f32 = 420.0;
const GUTTER: f32 = 80.0;
const SLIDE: f64 = 0.15;

fn show_entry(ctx: &Context, pal: &Palette, e: &mut Entry) -> Option<Outcome> {
    let now = ctx.input(|i| i.time);
    let opened = *e.opened_at.get_or_insert(now);
    let t = ((now - opened) / SLIDE).clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.request_repaint();
    }
    let ease = 1.0 - (1.0 - t as f32).powi(3);
    let y = -(1.0 - ease) * 60.0;

    let id = Id::unique(("medis-modal", e.seq));
    let kind = e.spec.kind;
    let mut result: Option<Outcome> = None;
    let first_frame = !e.focus_done;

    let frame = Frame::new()
        .fill(pal.sheet)
        .stroke(Stroke::new(1.0, pal.field_border))
        .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 })
        .inner_margin(Margin { left: 20, right: 20, top: 18, bottom: 16 })
        .shadow(egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: egui::Color32::from_black_alpha(80) });

    let backdrop = if pal.dark { egui::Color32::from_black_alpha(90) } else { egui::Color32::from_black_alpha(36) };
    let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, Vec2::new(0.0, y));
    let resp = egui::Modal::new(id).area(area).backdrop_color(backdrop).frame(frame).show(ctx, |ui| {
        ui.set_width(SHEET_WIDTH - 40.0);
        ui.spacing_mut().item_spacing = Vec2::new(10.0, 8.0);
        let mut submit = false;
        let mut cancel = false;

        // Heading and text.
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(GUTTER, 40.0), Layout::top_down(Align::Center), |ui| {
                if kind != ModalKind::Form {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                    if kind == ModalKind::Confirm {
                        widgets::paint_warning(ui.painter(), rect.center(), 40.0);
                    } else {
                        widgets::paint_info(ui.painter(), rect.center(), 38.0, pal.accent);
                    }
                }
            });
            ui.vertical(|ui| {
                ui.set_width(SHEET_WIDTH - 40.0 - GUTTER - 10.0);
                if !e.spec.title.is_empty() {
                    ui.label(RichText::new(&e.spec.title).strong().size(14.0));
                }
                if !e.spec.content.is_empty() {
                    ui.label(RichText::new(&e.spec.content).size(12.0).color(pal.text));
                }
            });
        });

        // Form fields.
        if kind == ModalKind::Form && !e.spec.fields.is_empty() {
            ui.add_space(4.0);
            for (i, field) in e.spec.fields.iter().enumerate() {
                let error = if e.submit_failed { field_error(field, &e.values[i]) } else { None };
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(Vec2::new(GUTTER, 22.0), Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(&field.label).size(12.0));
                    });
                    let width = ui.available_width();
                    match &field.kind {
                        FieldKind::Text | FieldKind::Number => {
                            let te = egui::TextEdit::singleline(&mut e.values[i]).desired_width(width).margin(Margin::symmetric(6, 3));
                            let r = ui.add(te);
                            if i == 0 && first_frame {
                                r.request_focus();
                            }
                            if r.lost_focus() && ui.input(|inp| inp.key_pressed(Key::Enter)) {
                                submit = true;
                            }
                        }
                        FieldKind::Bool => {
                            ui.checkbox(&mut e.flags[i], "");
                        }
                        FieldKind::Choice(options) => {
                            egui::ComboBox::from_id_salt((id, i))
                                .selected_text(&e.values[i])
                                .width(width.min(180.0))
                                .show_ui(ui, |ui| {
                                    for o in options {
                                        ui.selectable_value(&mut e.values[i], o.clone(), o);
                                    }
                                });
                        }
                    }
                });
                if let Some(msg) = error {
                    ui.horizontal(|ui| {
                        ui.add_space(GUTTER + ui.spacing().item_spacing.x);
                        ui.label(RichText::new(msg).size(12.0).color(egui::Color32::from_rgb(0xff, 0x2a, 0x1c)));
                    });
                }
            }
        }

        // Buttons, right aligned, 20px above.
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let min = Vec2::new(78.0, 24.0);
            match kind {
                ModalKind::Alert => {
                    let b = ui.add(primary_button(pal, e.spec.button.clone()).min_size(min));
                    if first_frame {
                        b.request_focus();
                    }
                    if b.clicked() {
                        submit = true;
                    }
                }
                ModalKind::Confirm => {
                    let action = ui.add(egui::Button::new(e.spec.button.clone()).min_size(min));
                    let cancel_btn = ui.add(primary_button(pal, "Cancel").min_size(min));
                    // Tab order should read Cancel, then the action.
                    if first_frame {
                        cancel_btn.request_focus();
                    }
                    if action.clicked() {
                        submit = true;
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                }
                ModalKind::Form => {
                    let action = ui.add(primary_button(pal, e.spec.button.clone()).min_size(min));
                    let cancel_btn = ui.add(egui::Button::new("Cancel").min_size(min));
                    if action.clicked() {
                        submit = true;
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                }
            }
        });
        (submit, cancel)
    });

    e.focus_done = true;
    let (mut submit, mut cancel) = resp.inner;

    // Esc cancels (an alert only has OK, so Esc closes it the same way).
    if resp.is_top_modal && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        if kind == ModalKind::Alert {
            submit = true;
        } else {
            cancel = true;
        }
    }
    // Enter with nothing focused (e.g. after a click on the sheet) means the default button.
    if !submit && !cancel && ctx.memory(|m| m.focused().is_none()) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
        match kind {
            ModalKind::Confirm => cancel = true,
            _ => submit = true,
        }
    }

    if cancel {
        result = Some(Outcome::Cancel);
    } else if submit {
        if kind == ModalKind::Form {
            let invalid = e.spec.fields.iter().enumerate().any(|(i, f)| field_error(f, &e.values[i]).is_some());
            if invalid {
                e.submit_failed = true;
            } else {
                result = Some(Outcome::Ok(collect(e)));
            }
        } else {
            result = Some(Outcome::Ok(vec![]));
        }
    }
    result
}

fn field_error(f: &Field, v: &str) -> Option<String> {
    match f.kind {
        FieldKind::Text if v.is_empty() => Some("This field is required.".into()),
        FieldKind::Number if v.trim().is_empty() => Some("This field is required.".into()),
        FieldKind::Number if v.trim().parse::<f64>().is_err() => Some("Enter a number.".into()),
        _ => None,
    }
}

fn collect(e: &Entry) -> Vec<FieldValue> {
    e.spec
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| match f.kind {
            FieldKind::Text | FieldKind::Choice(_) => FieldValue::Text(e.values[i].clone()),
            FieldKind::Number => FieldValue::Number(e.values[i].trim().parse().unwrap_or(0.0)),
            FieldKind::Bool => FieldValue::Bool(e.flags[i]),
        })
        .collect()
}
