//! Visual snapshots of the connected layout. Active only when `MEDIS_SHOTS_DIR` is set (and a
//! test Redis is available); each shot is saved as a PNG for a human to look at.

use std::path::PathBuf;

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::MedisApp;
use crate::smoke_tests::{connected_sized, expect, filter_to, has, open_tab, select_key, wait_for};

fn dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var("MEDIS_SHOTS_DIR").ok()?);
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn settle(h: &mut Harness<'_, MedisApp>) {
    for _ in 0..6 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

fn shot(h: &mut Harness<'_, MedisApp>, dir: &std::path::Path, name: &str) {
    settle(h);
    let img = h.render().expect("render failed");
    img.save(dir.join(format!("{name}.png"))).expect("save png");
}

fn run(w: f32, h: f32, tag: &str) {
    let Some(dir) = dir() else { return };
    let Some(mut harness) = connected_sized(Vec2::new(w, h), true) else { return };
    let h = &mut harness;
    shot(h, &dir, &format!("{tag}-keys"));

    select_key(h, "{hangfire}:job:d38846ec-d8881234567890");
    expect(h, "ParameterTypes");
    shot(h, &dir, &format!("{tag}-content-hash"));
    // A JSON value: the editor bar shows Beautify/Minify next to Wrapping and the mode picker.
    h.get_by_label("ParameterTypes").click();
    settle(h);
    shot(h, &dir, &format!("{tag}-content-hash-json"));

    select_key(h, "json:doc");
    wait_for(h, "json", |h| has(h, "Hangfire") || has(h, "Save Changes"));
    shot(h, &dir, &format!("{tag}-content-string"));

    select_key(h, "leaderboard");
    expect(h, "alice");
    shot(h, &dir, &format!("{tag}-content-zset"));

    filter_to(h, "queue:jobs", "queue:jobs");
    select_key(h, "queue:jobs");
    expect(h, "job1");
    shot(h, &dir, &format!("{tag}-content-list"));

    open_tab(h, "Terminal");
    h.run();
    h.event(egui::Event::Text("aaaa".to_string()));
    shot(h, &dir, &format!("{tag}-terminal"));

    open_tab(h, "Config");
    expect(h, "maxmemory");
    shot(h, &dir, &format!("{tag}-config"));
}

#[test]
fn shots_1200x800() {
    run(1200.0, 800.0, "1200x800");
}

#[test]
fn shots_840x400() {
    run(840.0, 400.0, "840x400");
}
