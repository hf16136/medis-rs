//! Shared value editor (SPEC-2 5.7). Called by `content` views, not by the app shell.
//!
//! CONTRACT (fixed by the orchestrator; the editor lane implements the bodies, the content lane
//! only calls these functions):
//! * `load(&mut inst.editor, Some(bytes))` is called when the selected row or string value
//!   changes. `load(.., None)` clears the editor and shows nothing.
//! * `show(ui, inst, cx) -> Option<Vec<u8>>` draws the editor into the region it is given. It
//!   returns `Some(encoded_bytes)` in the frame the user saves (Save button or Cmd/Ctrl+S).
//!   The editor validates and encodes with `medis_core::editor::encode_for_save` and alerts
//!   `INVALID_JSON` etc. itself. The CALLER (the content view) sends the right Op
//!   (StringSet / HashSet / ListSet / SetReplace / ZSetReplace ...), and calls
//!   `mark_saved(&mut inst.editor, bytes)` once the write succeeds.
//! * The editor never sends Redis requests itself.
//!
//! Behaviour notes:
//! * `load` always starts fresh (edits are dropped, a new undo history begins).
//! * Each mode (Raw / JSON / MessagePack) keeps its own edit buffer. Saving writes the active
//!   mode's buffer. An unchanged Raw buffer saves the original bytes, so a value that is not
//!   valid UTF-8 is never rewritten with replacement characters.
//! * Values larger than [`EDIT_LIMIT`] are not editable: the first [`PREVIEW_BYTES`] are shown
//!   read-only (laying out megabytes of text every frame would freeze the UI) and saving is
//!   disabled.

use egui::text_edit::TextBuffer;
use egui::{Align, Align2, Color32, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2};
use medis_core::Evt;
use medis_core::editor::{INVALID_JSON, Mode, Modes, encode_for_save, format_json};

use crate::instance::{Cx, Instance};
use crate::ui::theme::primary_button;

/// Largest value (in bytes) that can be edited.
pub const EDIT_LIMIT: usize = 512 * 1024;
/// How much of a larger value is shown (read-only).
pub const PREVIEW_BYTES: usize = 128 * 1024;
/// The Wrapping checkbox and the format buttons are hidden below this width.
const WRAP_MIN_WIDTH: f32 = 372.0;
const BAR_HEIGHT: f32 = 36.0;
const MODES: [Mode; 3] = [Mode::Raw, Mode::Json, Mode::MessagePack];

fn idx(mode: Mode) -> usize {
    match mode {
        Mode::Raw => 0,
        Mode::Json => 1,
        Mode::MessagePack => 2,
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Raw => "Raw",
        Mode::Json => "JSON",
        Mode::MessagePack => "MessagePack",
    }
}

/// State of this module, stored in `Instance::editor`. Reset (`Default`) on every connect.
pub struct EditorState {
    /// A value is loaded (otherwise the pane is empty).
    loaded: bool,
    /// The bytes as loaded or last saved (empty for a preview).
    orig: Vec<u8>,
    /// `Some(total_len)` when the value is too large to edit and only a preview is shown.
    preview: Option<usize>,
    /// The text each mode started with; `None` = the mode is unavailable.
    base: [Option<String>; 3],
    /// The edit buffers.
    bufs: [String; 3],
    mode: Mode,
    dirty: bool,
    wrap: bool,
    /// Bumped on every load so the text widget starts with a clean undo history.
    generation: u64,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            loaded: false,
            orig: Vec::new(),
            preview: None,
            base: Default::default(),
            bufs: Default::default(),
            mode: Mode::Raw,
            dirty: false,
            wrap: true,
            generation: 0,
        }
    }
}

impl EditorState {
    fn recompute_dirty(&mut self) {
        self.dirty = (0..3).any(|i| self.base[i].as_deref().is_some_and(|b| b != self.bufs[i]));
    }
}

/// Loads a new value (or clears the editor with `None`).
pub fn load(state: &mut EditorState, value: Option<Vec<u8>>) {
    state.generation += 1;
    state.dirty = false;
    state.preview = None;
    state.base = Default::default();
    state.bufs = Default::default();
    state.orig = Vec::new();
    let Some(bytes) = value else {
        state.loaded = false;
        state.mode = Mode::Raw;
        return;
    };
    state.loaded = true;
    if bytes.len() > EDIT_LIMIT {
        let text = String::from_utf8_lossy(&bytes[..PREVIEW_BYTES]).into_owned();
        state.preview = Some(bytes.len());
        state.bufs[0] = text.clone();
        state.base[0] = Some(text);
        state.mode = Mode::Raw;
        return;
    }
    let modes = Modes::detect(&bytes);
    state.mode = modes.initial_mode();
    state.base = [Some(modes.raw), modes.json, modes.messagepack];
    for i in 0..3 {
        state.bufs[i] = state.base[i].clone().unwrap_or_default();
    }
    state.orig = bytes;
}

/// Marks the current text as saved (clears the dirty flag) after the caller's write succeeded.
/// The editor is re-initialised from the saved bytes (the mode is detected again).
pub fn mark_saved(state: &mut EditorState, value: Vec<u8>) {
    load(state, Some(value));
}

fn human_size(n: usize) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n} B")
    } else if n < KB * KB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{:.1} MB", n / (KB * KB))
    }
}

/// Bytes to store for the current state.
fn encode(st: &EditorState) -> Result<Vec<u8>, String> {
    let i = idx(st.mode);
    if st.mode == Mode::Raw && st.base[0].as_deref() == Some(st.bufs[0].as_str()) {
        return Ok(st.orig.clone());
    }
    encode_for_save(st.mode, &st.bufs[i])
}

/// Draws the editor. Returns the encoded bytes in the frame the user saves.
pub fn show(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) -> Option<Vec<u8>> {
    if !inst.editor.loaded {
        return None;
    }
    let pal = cx.pal;
    let inst_id = inst.id;
    let st = &mut inst.editor;
    let editable = st.preview.is_none();
    let mut save = editable && !cx.blocked && ui.ctx().input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S));
    let mut alert: Option<String> = None;

    // ---- bottom bar ----
    egui::Panel::bottom(("editor-bar", inst_id))
        .exact_size(BAR_HEIGHT)
        .show_separator_line(false)
        .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([Pos2::new(r.left() - 10.0, r.top()), Pos2::new(r.right() + 10.0, r.top())], Stroke::new(1.0, pal.border));
            let narrow = r.width() + 20.0 < WRAP_MIN_WIDTH;
            ui.horizontal_centered(|ui| {
                if ui.add_enabled(editable && st.dirty, primary_button(&pal, "Save Changes")).clicked() {
                    save = true;
                }
                if let Some(total) = st.preview {
                    ui.label(
                        RichText::new(format!("Too large to edit ({}). Showing the first {}.", human_size(total), human_size(PREVIEW_BYTES)))
                            .size(12.0)
                            .color(pal.text_dim),
                    );
                } else if st.mode != Mode::Raw && !narrow {
                    for (label, beautify) in [("Beautify", true), ("Minify", false)] {
                        if ui.small_button(label).clicked() {
                            let i = idx(st.mode);
                            match format_json(&st.bufs[i], beautify) {
                                Some(t) => {
                                    st.bufs[i] = t;
                                    st.recompute_dirty();
                                }
                                None => alert = Some(INVALID_JSON.to_string()),
                            }
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt(("editor-mode", inst_id))
                        .selected_text(mode_label(st.mode))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for m in MODES {
                                let available = st.base[idx(m)].is_some();
                                ui.add_enabled_ui(available, |ui| {
                                    if ui.selectable_label(st.mode == m, mode_label(m)).clicked() {
                                        chosen = Some(m);
                                    }
                                });
                            }
                        });
                    if let Some(m) = chosen {
                        st.mode = m;
                    }
                    if !narrow {
                        ui.checkbox(&mut st.wrap, "Wrapping");
                    }
                });
            });
        });

    // ---- text area ----
    egui::CentralPanel::default().frame(Frame::new().fill(pal.field_bg)).show(ui, |ui| {
        text_area(ui, st, &cx.pal, inst_id);
    });

    if let Some(msg) = alert {
        cx.alert(msg);
    }
    if save && editable {
        return match encode(&inst.editor) {
            Ok(bytes) => Some(bytes),
            Err(msg) => {
                cx.alert(msg);
                None
            }
        };
    }
    None
}

fn text_area(ui: &mut Ui, st: &mut EditorState, pal: &crate::ui::theme::Palette, inst_id: u64) {
    let font: FontId = egui::TextStyle::Monospace.resolve(ui.style());
    let i = idx(st.mode);
    let editable = st.preview.is_none();
    let wrap = st.wrap;
    let id = Id::unique(("value-editor", inst_id, st.generation, i));
    let avail_h = ui.available_height();
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let lines = st.bufs[i].bytes().filter(|b| *b == b'\n').count() + 1;
    let digits = lines.to_string().len().max(2);
    let gutter_w = digits as f32 * char_w + 18.0;
    let gutter_bg = if pal.dark { pal.toolbar } else { Color32::from_rgb(0xf7, 0xf7, 0xf7) };
    let text_color = pal.text;
    let number_color = pal.text_faint;
    let rule = pal.border;
    let mut changed = false;

    let area = if wrap { ScrollArea::vertical() } else { ScrollArea::both() };
    area.id_salt(("value-editor-scroll", inst_id)).auto_shrink(false).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (grect, _) = ui.allocate_exact_size(Vec2::new(gutter_w, 1.0), Sense::hover());

            let layout_font = font.clone();
            let mut layouter = move |ui: &Ui, text: &dyn TextBuffer, wrap_width: f32| {
                let width = if wrap { wrap_width } else { f32::INFINITY };
                let job = egui::text::LayoutJob::simple(text.as_str().to_owned(), layout_font.clone(), text_color, width);
                ui.fonts_mut(|f| f.layout_job(job))
            };

            let buf = &mut st.bufs[i];
            let mut readonly: &str;
            let target: &mut dyn TextBuffer = if editable {
                buf
            } else {
                readonly = buf.as_str();
                &mut readonly
            };
            let out = TextEdit::multiline(target)
                .id(id)
                .font(font.clone())
                .text_color(text_color)
                .frame(Frame::new().inner_margin(Margin::symmetric(8, 4)))
                .desired_width(f32::INFINITY)
                .min_size(Vec2::new(0.0, avail_h))
                .layouter(&mut layouter)
                .show(ui);
            changed = out.response.changed();

            // Line-number gutter, aligned with the laid-out rows (wrapped rows get no number).
            let clip = ui.clip_rect();
            let bottom = out.response.rect.bottom().max(clip.bottom());
            let gr = Rect::from_min_max(grect.min, Pos2::new(grect.right(), bottom));
            let p = ui.painter().with_clip_rect(clip);
            p.rect_filled(gr, 0.0, gutter_bg);
            p.line_segment([gr.right_top(), gr.right_bottom()], Stroke::new(1.0, rule));
            let mut line = 1usize;
            let mut starts_line = true;
            for row in &out.galley.rows {
                let y = out.galley_pos.y + row.pos.y;
                if y > clip.bottom() {
                    break;
                }
                if starts_line {
                    if y + row.size.y >= clip.top() {
                        p.text(
                            Pos2::new(gr.right() - 8.0, y + row.size.y / 2.0),
                            Align2::RIGHT_CENTER,
                            line.to_string(),
                            font.clone(),
                            number_color,
                        );
                    }
                    line += 1;
                }
                starts_line = row.ends_with_newline;
            }
        });
    });

    if changed {
        st.recompute_dirty();
    }
}

/// Unused: the editor sends no requests. Kept for the routing table.
pub fn handle_evt(inst: &mut Instance, cx: &mut Cx, evt: &Evt) {
    let _ = (inst, cx, evt);
}
