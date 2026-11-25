//! String view (SPEC-2 5.1): the value editor fills the whole pane, no split.
//!
//! Load: `GET` as raw bytes into the editor. Save: `Op::StringSet` (the keep-TTL script runs in
//! the worker), then `editor::mark_saved` and a `content_version` bump so the footer refreshes.

use medis_core::worker::Op;
use medis_core::{CoreError, Reply, ReqId};

use crate::instance::{Cx, Instance, Module};
use crate::ui::editor;

pub struct State {
    pub key: String,
    pub len: u64,
    /// The `GET` request in flight.
    load: Option<ReqId>,
    /// The `SET` request in flight, with the bytes being written.
    save: Option<(ReqId, Vec<u8>)>,
    /// The value has arrived (saving earlier would overwrite it with nothing).
    loaded: bool,
}

impl State {
    pub fn new(key: String, len: u64) -> Self {
        Self { key, len, load: None, save: None, loaded: false }
    }
}

/// Sends the first load request(s).
pub fn start(st: &mut State, inst: &mut Instance, _cx: &mut Cx) {
    st.load = Some(inst.send(Module::Content, Op::StringGet(st.key.clone())));
}

/// Draws the view.
pub fn show(st: &mut State, ui: &mut egui::Ui, inst: &mut Instance, cx: &mut Cx) {
    if !st.loaded {
        ui.centered_and_justified(|ui| ui.spinner());
        return;
    }
    let Some(bytes) = editor::show(ui, inst, cx) else { return };
    if st.save.is_some() {
        // A write is already on its way; ignore a second save until it answers.
        return;
    }
    let id = inst.send(Module::Content, Op::StringSet { key: st.key.clone(), value: bytes.clone() });
    st.save = Some((id, bytes));
}

/// Handles a reply to a request this view sent (ignore unknown ids).
pub fn on_reply(st: &mut State, inst: &mut Instance, cx: &mut Cx, id: ReqId, result: &Result<Reply, CoreError>) {
    if st.load == Some(id) {
        st.load = None;
        match result {
            Ok(Reply::Bytes(value)) => {
                st.loaded = true;
                // A missing value (the key expired between TYPE and GET) shows as empty.
                let bytes = value.clone().unwrap_or_default();
                st.len = bytes.len() as u64;
                editor::load(&mut inst.editor, Some(bytes));
            }
            Ok(_) => {}
            Err(e) => cx.alert(e.message.clone()),
        }
        return;
    }
    if st.save.as_ref().is_some_and(|(sid, _)| *sid == id) {
        let Some((_, bytes)) = st.save.take() else { return };
        match result {
            Ok(_) => {
                st.len = bytes.len() as u64;
                editor::mark_saved(&mut inst.editor, bytes);
                inst.content_version += 1;
            }
            Err(e) => cx.alert(format!("Redis save failed: {}", e.message)),
        }
    }
}
