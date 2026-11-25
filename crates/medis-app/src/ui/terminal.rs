//! Terminal tab (SPEC-3 6): a redis-cli style shell on top of `medis_core::cmdline` and
//! `medis_core::format`.
//!
//! `Op::Exec`, `Op::PubSubStart`, `Op::MonitorStart`. `Evt::PubSub`, `Evt::Monitor` and
//! `Evt::StreamEnded` arrive here even while the tab is hidden, so the scrollback keeps growing.
//! Nothing here blocks: every request is answered through [`handle_evt`].

use std::collections::VecDeque;

use egui::text::CCursor;
use egui::text_edit::TextEditState;
use egui::text_selection::{CCursorRange, LabelSelectionState};
use egui::{
    Color32, Context, Event, FontId, Frame, Id, Key, Label, Margin, Modifiers, ScrollArea, Sense, Stroke, TextEdit,
    TextFormat, TextWrapMode, Ui, text::LayoutJob,
};
use medis_core::cmdline::{
    CONFIRM_PROMPT, PROMPT, Special, classify, complete, completions, needs_confirm, parse_confirm, split_args,
};
use medis_core::format::{
    Line, Tone, banner_monitor, banner_subscribe, format_monitor, format_pubsub, render_error, render_reply,
};
use medis_core::worker::{Op, Reply, StreamKind};
use medis_core::{Evt, ReqId};

use crate::instance::{Cx, Instance, Module};

/// Most rows kept in the scrollback (oldest are dropped first).
const MAX_ROWS: usize = 1000;
/// Commands remembered per connection.
const MAX_HISTORY: usize = medis_core::storage::HISTORY_LIMIT;
/// Longer lines are cut so one huge value cannot stall the layout.
const MAX_ROW_CHARS: usize = 20_000;
const FONT_SIZE: f32 = 13.0;

const COL_PLAIN: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);
const COL_PROMPT: Color32 = Color32::WHITE;
const COL_INPUT: Color32 = Color32::from_rgb(0xf2, 0xf2, 0xf2);
const COL_NESTED: Color32 = Color32::from_rgb(0xaa, 0xc6, 0xe3);
const COL_STRING: Color32 = Color32::from_rgb(0xd6, 0xec, 0x9c);
const COL_STREAM: Color32 = Color32::from_rgb(0x8f, 0x9d, 0x6a);
const COL_INTEGER: Color32 = Color32::from_rgb(0x78, 0xcf, 0x8a);
const COL_NIL: Color32 = Color32::from_rgb(0xcf, 0x7e, 0xa9);
const COL_ERROR: Color32 = Color32::from_rgb(0xee, 0x68, 0x68);
const COL_STATUS: Color32 = Color32::from_rgb(0xcd, 0xa8, 0x69);
const COL_INDEX: Color32 = Color32::from_rgb(0x84, 0x80, 0x80);
const COL_DIM: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
const COL_RULE: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

fn tone_color(t: Tone) -> Color32 {
    match t {
        Tone::Plain => COL_PLAIN,
        Tone::String => COL_STRING,
        Tone::Integer => COL_INTEGER,
        Tone::Nil => COL_NIL,
        Tone::Error => COL_ERROR,
        Tone::Status => COL_STATUS,
        Tone::Index => COL_INDEX,
        Tone::Dim => COL_DIM,
    }
}

/// One scrollback row, already coloured (the terminal is black in both themes).
struct Row {
    spans: Vec<(String, Color32)>,
}

fn clean(text: &str) -> String {
    let mut s: String = text.chars().filter(|c| *c != '\r').collect();
    if s.contains('\t') {
        s = s.replace('\t', "    ");
    }
    if s.chars().count() > MAX_ROW_CHARS {
        s = s.chars().take(MAX_ROW_CHARS).collect();
        s.push('…');
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// A command reply.
    Reply,
    /// A line of the MONITOR stream (string tone is the olive base colour, the command is pink).
    Monitor,
    /// A pub/sub message.
    PubSub,
}

fn row_from(line: &Line, style: Style) -> Row {
    let mut seen_status = false;
    let spans = line
        .iter()
        .map(|s| {
            let mut col = tone_color(s.tone);
            match (style, s.tone) {
                (Style::Monitor | Style::PubSub, Tone::String) => col = COL_STREAM,
                (Style::Monitor, Tone::Status) if !seen_status => {
                    seen_status = true;
                    col = COL_NIL;
                }
                _ => {}
            }
            (clean(&s.text), col)
        })
        .collect();
    Row { spans }
}

/// What the terminal is doing.
#[derive(Default)]
enum Mode {
    #[default]
    Normal,
    /// Waiting for `y`/`n` before running this FLUSHALL / FLUSHDB.
    Confirm(Vec<Vec<u8>>),
    /// A command is running; input is disabled.
    Busy(ReqId),
    /// MONITOR stream (`start` is the pending start request).
    Monitor { start: Option<ReqId> },
    /// SUBSCRIBE / PSUBSCRIBE stream.
    Subscribe { start: Option<ReqId> },
}

/// State of this module, stored in `Instance::terminal`. Reset (`Default`) on every connect.
#[derive(Default)]
pub struct TerminalState {
    rows: VecDeque<Row>,
    input: String,
    mode: Mode,
    history: Vec<String>,
    history_loaded: bool,
    /// Index into `history` while browsing with Up/Down.
    hist_pos: Option<usize>,
    /// What was typed before browsing started.
    draft: String,
    want_focus: bool,
    scroll_bottom: bool,
    last_frame: u64,
}

impl TerminalState {
    fn push_row(&mut self, row: Row) {
        self.rows.push_back(row);
        while self.rows.len() > MAX_ROWS {
            self.rows.pop_front();
        }
    }

    fn push_lines(&mut self, lines: &[Line], style: Style) {
        for l in lines {
            self.push_row(row_from(l, style));
        }
    }

    fn push_text(&mut self, text: &str, col: Color32) {
        self.push_row(Row { spans: vec![(clean(text), col)] });
    }

    fn echo(&mut self, prompt: &str, prompt_col: Color32, line: &str) {
        self.push_row(Row { spans: vec![(prompt.to_owned(), prompt_col), (clean(line), COL_INPUT)] });
    }

    fn prompt(&self) -> (&'static str, Color32) {
        match self.mode {
            Mode::Confirm(_) => (CONFIRM_PROMPT, COL_NESTED),
            Mode::Subscribe { .. } | Mode::Monitor { .. } => ("", COL_PROMPT),
            _ => (PROMPT, COL_PROMPT),
        }
    }

    fn accepts_input(&self) -> bool {
        matches!(self.mode, Mode::Normal | Mode::Confirm(_))
    }

    fn in_stream(&self) -> bool {
        matches!(self.mode, Mode::Monitor { .. } | Mode::Subscribe { .. })
    }
}

// ---- history on disk -------------------------------------------------------------------------

fn connection_key(inst: &Instance) -> String {
    inst.info.as_deref().map(|i| i.connection_key.clone()).unwrap_or_default()
}

fn ensure_history(inst: &mut Instance, cx: &Cx) {
    if inst.terminal.history_loaded {
        return;
    }
    let key = connection_key(inst);
    inst.terminal.history = cx.storage.load_history(&key);
    inst.terminal.history_loaded = true;
}

/// Remembers `line` (not empty, not AUTH, not a repeat of the last entry). Re-reads the file
/// first so tabs of the same connection share one list.
fn remember(inst: &mut Instance, cx: &Cx, line: &str) {
    let first = line.split_whitespace().next().unwrap_or("");
    if first.eq_ignore_ascii_case("AUTH") {
        return;
    }
    let key = connection_key(inst);
    let mut list = cx.storage.load_history(&key);
    if list.last().map(String::as_str) != Some(line) {
        list.push(line.to_owned());
    }
    let skip = list.len().saturating_sub(MAX_HISTORY);
    list.drain(..skip);
    let _ = cx.storage.save_history(&key, &list);
    inst.terminal.history = list;
    inst.terminal.history_loaded = true;
}

// ---- running commands ------------------------------------------------------------------------

fn submit(inst: &mut Instance, cx: &mut Cx, line: String) {
    ensure_history(inst, cx);
    inst.terminal.scroll_bottom = true;
    inst.terminal.hist_pos = None;
    match std::mem::take(&mut inst.terminal.mode) {
        Mode::Confirm(args) => {
            inst.terminal.echo(CONFIRM_PROMPT, COL_NESTED, &line);
            match parse_confirm(&line) {
                Some(true) => run(inst, args),
                Some(false) => {}
                None => inst.terminal.mode = Mode::Confirm(args),
            }
        }
        Mode::Normal => {
            if line.trim().is_empty() {
                return;
            }
            inst.terminal.echo(PROMPT, COL_PROMPT, &line);
            remember(inst, cx, &line);
            match split_args(&line) {
                Err(e) => inst.terminal.push_lines(&render_error(&e.to_string()), Style::Reply),
                Ok(args) if args.is_empty() => {}
                Ok(args) => {
                    if needs_confirm(&args).is_some() {
                        inst.terminal.mode = Mode::Confirm(args);
                    } else {
                        run(inst, args);
                    }
                }
            }
        }
        other => inst.terminal.mode = other,
    }
}

fn run(inst: &mut Instance, args: Vec<Vec<u8>>) {
    match classify(&args) {
        Special::Monitor => {
            inst.terminal.push_text(&banner_monitor(), COL_NESTED);
            let id = inst.send(Module::Terminal, Op::MonitorStart);
            inst.terminal.mode = Mode::Monitor { start: Some(id) };
        }
        Special::Subscribe { channels } => {
            inst.terminal.push_text(&banner_subscribe(), COL_NESTED);
            let id = inst.send(Module::Terminal, Op::PubSubStart { channels, patterns: Vec::new() });
            inst.terminal.mode = Mode::Subscribe { start: Some(id) };
        }
        Special::PSubscribe { patterns } => {
            inst.terminal.push_text(&banner_subscribe(), COL_NESTED);
            let id = inst.send(Module::Terminal, Op::PubSubStart { channels: Vec::new(), patterns });
            inst.terminal.mode = Mode::Subscribe { start: Some(id) };
        }
        // SELECT is a plain command; the worker reports `Evt::DbSelected`, which the shell applies.
        Special::Select(_) | Special::None => {
            let id = inst.send(Module::Terminal, Op::Exec(args));
            inst.terminal.mode = Mode::Busy(id);
        }
    }
}

/// Ctrl+C: leave the current nested mode, or clear the input line at the top level.
fn interrupt(inst: &mut Instance) {
    let st = &mut inst.terminal;
    match std::mem::take(&mut st.mode) {
        Mode::Confirm(_) => {
            st.push_row(Row { spans: vec![(CONFIRM_PROMPT.to_owned(), COL_NESTED), ("^C".into(), COL_DIM)] });
            st.input.clear();
        }
        Mode::Monitor { .. } => {
            st.push_text("^C", COL_DIM);
            inst.send(Module::Terminal, Op::MonitorStop);
        }
        Mode::Subscribe { .. } => {
            st.push_text("^C", COL_DIM);
            inst.send(Module::Terminal, Op::PubSubStop);
        }
        Mode::Busy(id) => st.mode = Mode::Busy(id),
        Mode::Normal => st.input.clear(),
    }
    let st = &mut inst.terminal;
    st.hist_pos = None;
    if matches!(st.mode, Mode::Normal) {
        st.want_focus = true;
        st.scroll_bottom = true;
    }
}

// ---- keyboard --------------------------------------------------------------------------------

fn has_selection(ctx: &Context, input_id: Id) -> bool {
    let label = ctx.plugin_opt::<LabelSelectionState>().is_some_and(|p| p.lock().has_selection());
    let edit = TextEditState::load(ctx, input_id)
        .and_then(|s| s.cursor.char_range())
        .is_some_and(|r| !r.is_empty());
    label || edit
}

/// `true` when Ctrl+C was pressed and is meant as an interrupt (not a copy of a selection).
fn take_ctrl_c(ctx: &Context, input_id: Id) -> bool {
    // On macOS Ctrl+C is a plain key event and Cmd+C is the copy command.
    let key = ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::C));
    if key {
        return true;
    }
    // Elsewhere Ctrl+C is the copy command: it only interrupts when nothing is selected.
    if !cfg!(target_os = "macos") && ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Copy))) {
        if has_selection(ctx, input_id) {
            return false;
        }
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Copy)));
        return true;
    }
    false
}

fn cursor_to_end(ctx: &Context, id: Id, text: &str) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(text.chars().count()))));
    state.store(ctx, id);
}

fn history_step(st: &mut TerminalState, up: bool) {
    if st.history.is_empty() {
        return;
    }
    let last = st.history.len() - 1;
    let next = match (st.hist_pos, up) {
        (None, true) => {
            st.draft = st.input.clone();
            Some(last)
        }
        (None, false) => return,
        (Some(i), true) => Some(i.saturating_sub(1)),
        (Some(i), false) if i < last => Some(i + 1),
        (Some(_), false) => None,
    };
    st.hist_pos = next;
    st.input = match next {
        Some(i) => st.history[i].clone(),
        None => std::mem::take(&mut st.draft),
    };
}

/// Tab: complete the first word; when that cannot go further, list the candidates.
fn tab_complete(st: &mut TerminalState) {
    if let Some(new) = complete(&st.input) {
        st.input = new;
        return;
    }
    let all = completions(&st.input);
    if all.len() < 2 {
        return;
    }
    st.scroll_bottom = true;
    let typed = st.input.clone();
    st.echo(PROMPT, COL_PROMPT, &typed);
    const SHOWN: usize = 120;
    let mut row = String::new();
    for name in all.iter().take(SHOWN) {
        if !row.is_empty() && row.len() + name.trim().len() + 2 > 100 {
            st.push_text(&row, COL_PLAIN);
            row.clear();
        }
        if !row.is_empty() {
            row.push_str("  ");
        }
        row.push_str(name.trim());
    }
    if !row.is_empty() {
        st.push_text(&row, COL_PLAIN);
    }
    if all.len() > SHOWN {
        st.push_text(&format!("... and {} more", all.len() - SHOWN), COL_DIM);
    }
}

// ---- UI --------------------------------------------------------------------------------------

/// Draws the module into `ui` (the region the app shell gave it). Called every frame while the
/// region is visible.
pub fn show(ui: &mut Ui, inst: &mut Instance, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let input_id = Id::unique(("terminal-input", inst.id));
    let modal_open = cx.modals.is_open();
    ensure_history(inst, cx);

    // Focus the input when the tab becomes visible.
    let frame_nr = ctx.cumulative_frame_nr();
    if frame_nr > inst.terminal.last_frame + 1 {
        inst.terminal.want_focus = true;
    }
    inst.terminal.last_frame = frame_nr;

    let font = FontId::monospace(FONT_SIZE);
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let mut submitted: Option<String> = None;
    let mut interrupted = false;

    if !modal_open {
        // Cmd/Ctrl+L clears the scrollback.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::L)) {
            inst.terminal.rows.clear();
        }
        let focused = ctx.memory(|m| m.has_focus(input_id));
        let st = &inst.terminal;
        if (focused || st.in_stream() || matches!(st.mode, Mode::Confirm(_))) && take_ctrl_c(&ctx, input_id) {
            interrupted = true;
        }
    }

    // ---- the input row ----
    egui::Panel::bottom(("terminal-input-row", inst.id))
        .exact_size(row_h + 14.0)
        .show_separator_line(false)
        .frame(Frame::new().fill(cx.pal.terminal_bg).inner_margin(Margin::symmetric(12, 0)))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment(
                [egui::pos2(r.left() - 12.0, r.top()), egui::pos2(r.right() + 12.0, r.top())],
                Stroke::new(1.0, COL_RULE),
            );
            let st = &mut inst.terminal;
            let interactive = st.accepts_input() && !modal_open;
            let focused = ui.memory(|m| m.has_focus(input_id));
            if interactive && focused {
                let (up, down, tab) = ui.input_mut(|i| {
                    (
                        i.consume_key(Modifiers::NONE, Key::ArrowUp),
                        i.consume_key(Modifiers::NONE, Key::ArrowDown),
                        i.consume_key(Modifiers::NONE, Key::Tab),
                    )
                });
                if matches!(st.mode, Mode::Normal) {
                    if up || down {
                        history_step(st, up);
                        cursor_to_end(ui.ctx(), input_id, &st.input);
                    }
                    if tab {
                        tab_complete(st);
                        cursor_to_end(ui.ctx(), input_id, &st.input);
                    }
                }
            }
            let (prompt, prompt_col) = st.prompt();
            let v = ui.visuals_mut();
            v.selection.bg_fill = Color32::from_rgb(0x26, 0x4f, 0x78);
            v.selection.stroke = Stroke::NONE;
            v.text_cursor.stroke = Stroke::new(2.0, Color32::WHITE);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if !prompt.is_empty() {
                    ui.add(Label::new(egui::RichText::new(prompt).font(font.clone()).color(prompt_col)).selectable(false));
                }
                let out = TextEdit::singleline(&mut st.input)
                    .id(input_id)
                    .font(font.clone())
                    .frame(Frame::new())
                    .margin(Margin::ZERO)
                    .text_color(COL_INPUT)
                    .desired_width(f32::INFINITY)
                    .lock_focus(true)
                    .interactive(interactive)
                    .show(ui);
                let resp = out.response;
                if resp.changed() {
                    st.hist_pos = None;
                }
                if interactive && resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submitted = Some(std::mem::take(&mut st.input));
                    resp.request_focus();
                }
                if st.want_focus && interactive {
                    resp.request_focus();
                    st.want_focus = false;
                }
            });
        });

    // ---- the scrollback ----
    egui::CentralPanel::default()
        .frame(Frame::new().fill(cx.pal.terminal_bg).inner_margin(Margin::symmetric(12, 6)))
        .show(ui, |ui| {
            let st = &mut inst.terminal;
            let v = ui.visuals_mut();
            v.selection.bg_fill = Color32::from_rgb(0x26, 0x4f, 0x78);
            v.selection.stroke = Stroke::NONE;
            ui.spacing_mut().item_spacing.y = 0.0;
            // Clicking empty space focuses the input (lower layer than the selectable rows).
            let bg = ui.interact(ui.max_rect(), input_id.with("terminal-bg"), Sense::click());
            let n = st.rows.len();
            let mut area = ScrollArea::both().id_salt(("terminal-scroll", input_id)).auto_shrink(false).stick_to_bottom(true);
            if st.scroll_bottom {
                area = area.vertical_scroll_offset(1.0e9);
                st.scroll_bottom = false;
            }
            area.show_rows(ui, row_h, n, |ui, range| {
                for i in range {
                    let Some(row) = st.rows.get(i) else { break };
                    let mut job = LayoutJob::default();
                    for (text, col) in &row.spans {
                        job.append(text, 0.0, TextFormat { font_id: font.clone(), color: *col, ..Default::default() });
                    }
                    if job.text.is_empty() {
                        job.append(" ", 0.0, TextFormat { font_id: font.clone(), color: COL_PLAIN, ..Default::default() });
                    }
                    ui.add(Label::new(job).wrap_mode(TextWrapMode::Extend).selectable(true));
                }
            });
            if bg.clicked() && !has_selection(ui.ctx(), input_id) {
                st.want_focus = true;
            }
        });

    // ---- effects (need the whole instance) ----
    if interrupted {
        interrupt(inst);
    }
    if let Some(line) = submitted {
        submit(inst, cx, line);
        ctx.request_repaint();
    }
}

/// Receives worker events. Called for replies to requests this module sent (`Module::Terminal`),
/// for every `ScanBatch` of its scans, and for every unsolicited event (`DbSelected`, `PubSub`,
/// `Monitor`, `StreamEnded`, `Connected`) whether or not the module is visible.
pub fn handle_evt(inst: &mut Instance, _cx: &mut Cx, evt: &Evt) {
    let st = &mut inst.terminal;
    match evt {
        Evt::Reply { id, result } => match st.mode {
            Mode::Busy(bid) if bid == *id => {
                st.mode = Mode::Normal;
                st.want_focus = true;
                st.scroll_bottom = true;
                match result {
                    Ok(Reply::Value(v)) => st.push_lines(&render_reply(v), Style::Reply),
                    Ok(_) => {}
                    Err(e) => st.push_lines(&render_error(&e.message), Style::Reply),
                }
            }
            Mode::Monitor { start: Some(sid) } | Mode::Subscribe { start: Some(sid) } if sid == *id => match result {
                Ok(_) => {
                    st.mode = match st.mode {
                        Mode::Monitor { .. } => Mode::Monitor { start: None },
                        _ => Mode::Subscribe { start: None },
                    };
                }
                Err(e) => {
                    st.mode = Mode::Normal;
                    st.want_focus = true;
                    st.scroll_bottom = true;
                    st.push_lines(&render_error(&e.message), Style::Reply);
                }
            },
            _ => {}
        },
        Evt::Monitor(line) => {
            if matches!(st.mode, Mode::Monitor { .. }) {
                st.push_row(row_from(&format_monitor(line), Style::Monitor));
            }
        }
        Evt::PubSub(msg) => {
            if matches!(st.mode, Mode::Subscribe { .. }) {
                st.push_row(row_from(&format_pubsub(msg), Style::PubSub));
            }
        }
        Evt::StreamEnded { kind, error } => {
            let matches_mode = matches!(
                (kind, &st.mode),
                (StreamKind::Monitor, Mode::Monitor { .. }) | (StreamKind::PubSub, Mode::Subscribe { .. })
            );
            if matches_mode {
                st.mode = Mode::Normal;
                st.want_focus = true;
                st.scroll_bottom = true;
                match error {
                    Some(msg) => st.push_lines(&render_error(msg), Style::Reply),
                    None => st.push_text(
                        if *kind == StreamKind::Monitor { "Monitor stopped." } else { "Subscription ended." },
                        COL_DIM,
                    ),
                }
            }
        }
        _ => {}
    }
}
