//! Helpers shared by the content sub-views.

use egui::{Frame, Ui};
use medis_core::storage::Sizes;

use crate::instance::{Cx, Instance};
use crate::ui::modal::ModalSpec;

/// Title, button and body of the common delete-item confirm (SPEC-2 5.0).
pub const DELETE_TITLE: &str = "Delete selected item?";
pub const DELETE_BUTTON: &str = "Delete";
pub const DELETE_BODY: &str =
    "Are you sure you want to delete the selected item? This action cannot be undone.";
/// Alert for saving before the data has loaded (SPEC-2 5.0).
pub const WAIT_FOR_LOAD: &str = "Please wait for data been loaded before saving.";

/// The common delete confirm. `then` runs only when the user confirms.
pub fn confirm_delete(cx: &mut Cx, then: impl FnOnce(&mut Instance, &mut Cx) + 'static) {
    cx.modal(ModalSpec::confirm(DELETE_TITLE, DELETE_BUTTON, DELETE_BODY), move |inst, cx, out| {
        if out.is_ok() {
            then(inst, cx);
        }
    });
}

/// Row to select after deleting row `sel` when `len_after` rows remain:
/// `sel >= len_after - 1 ? sel - 1 : sel`, or `None` if the list is empty.
pub fn reselect_after_delete(sel: usize, len_after: usize) -> Option<usize> {
    if len_after == 0 {
        return None;
    }
    if sel + 1 >= len_after { Some(len_after - 1) } else { Some(sel) }
}

/// The persisted pane widths, read from disk once and then cached in egui memory.
pub fn sizes(cx: &Cx) -> Sizes {
    let id = egui::Id::unique("medis-sizes");
    if let Some(s) = cx.ctx.data(|d| d.get_temp::<Sizes>(id)) {
        return s;
    }
    let s = cx.storage.load_sizes();
    cx.ctx.data_mut(|d| d.insert_temp(id, s));
    s
}

/// Updates the cache and persists the pane widths.
pub fn set_sizes(cx: &Cx, s: Sizes) {
    cx.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("medis-sizes"), s));
    let _ = cx.storage.save_sizes(&s);
}

/// Horizontal split: the table on the left (width `contentBarWidth`, min 80, persisted after a
/// drag), the editor on the right. Pass `inst.id`.
pub fn split(
    ui: &mut Ui,
    cx: &mut Cx,
    id: u64,
    left: impl FnOnce(&mut Ui, &mut Cx),
    right: impl FnOnce(&mut Ui, &mut Cx),
) {
    let mut sizes = sizes(cx);
    let resp = egui::Panel::left(egui::Id::unique(("content-split", id)))
        .resizable(true)
        .default_size(sizes.content_bar_width)
        .size_range(80.0..=(ui.available_width() - 120.0).max(81.0))
        .frame(Frame::new().fill(cx.pal.content))
        .show(ui, |ui| left(ui, cx));
    let width = resp.response.rect.width();
    let released = ui.input(|i| i.pointer.any_released());
    if released && (width - sizes.content_bar_width).abs() > 0.5 {
        sizes.content_bar_width = width;
        set_sizes(cx, sizes);
    }
    egui::CentralPanel::default().frame(Frame::new().fill(cx.pal.content)).show(ui, |ui| right(ui, cx));
}
