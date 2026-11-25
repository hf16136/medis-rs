//! Headless GUI smoke tests (egui_kittest) against a live Redis.
//!
//! Gated on `MEDIS_TEST_REDIS=redis://host:port`; each test returns early when it is unset.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};

use crate::app::MedisApp;

fn redis_addr() -> Option<(String, String)> {
    let url = std::env::var("MEDIS_TEST_REDIS").ok()?;
    let hp = url.strip_prefix("redis://").unwrap_or(&url).trim_end_matches('/');
    let (h, p) = hp.rsplit_once(':')?;
    Some((h.to_string(), p.to_string()))
}

fn make_harness() -> Harness<'static, MedisApp> {
    medis_core::install_crypto_provider();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().unwrap();
    let dir = std::env::temp_dir().join(format!("medis-smoke-{}", uuid::Uuid::new_v4().simple()));
    let storage = medis_core::storage::Storage::new(dir);
    Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 700.0))
        .with_accessibility_check(false)
        .build_eframe(move |cc| MedisApp::with_storage(&cc.egui_ctx, rt, storage))
}


const TIMEOUT: Duration = Duration::from_secs(10);

/// Steps the harness until `cond` holds (or the timeout), sleeping between frames so the async
/// worker can answer. Returns whether the condition was met.
fn wait_for(h: &mut Harness<'_, MedisApp>, what: &str, mut cond: impl FnMut(&Harness<'_, MedisApp>) -> bool) -> bool {
    let start = Instant::now();
    loop {
        h.step();
        if cond(h) {
            return true;
        }
        if start.elapsed() > TIMEOUT {
            println!("timed out waiting for: {what}");
            dump_tree(h);
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn dump_tree(h: &Harness<'_, MedisApp>) {
    fn walk(n: &egui_kittest::Node<'_>, d: usize) {
        let a = n.accesskit_node();
        println!("{}{:?} label={:?} value={:?}", "  ".repeat(d), a.role(), a.label(), a.value());
        for c in n.children() {
            walk(&c, d + 1);
        }
    }
    walk(&h.root(), 0);
}


/// Is there any node whose label (or, for text labels, value) contains `text`?
fn has(h: &Harness<'_, MedisApp>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

fn expect(h: &mut Harness<'_, MedisApp>, text: &str) {
    assert!(wait_for(h, text, |h| has(h, text)), "\"{text}\" never appeared");
}

fn expect_gone(h: &mut Harness<'_, MedisApp>, text: &str) {
    assert!(wait_for(h, &format!("{text} to disappear"), |h| !has(h, text)), "\"{text}\" never went away");
}

/// Types into the widget that currently has keyboard focus.
fn type_focused(h: &mut Harness<'_, MedisApp>, text: &str) {
    h.event(egui::Event::Text(text.to_string()));
    h.step();
}

fn press(h: &mut Harness<'_, MedisApp>, key: egui::Key) {
    h.key_press(key);
    h.step();
    h.step();
}

/// Replaces the text of the `n`-th single-line text input.
fn set_text_input(h: &mut Harness<'_, MedisApp>, n: usize, text: &str) {
    {
        let inputs: Vec<_> = h.query_all_by_role(Role::TextInput).collect();
        inputs[n].focus();
    }
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    type_focused(h, text);
}

fn connect(h: &mut Harness<'_, MedisApp>, host: &str, port: &str) {
    h.run();
    set_text_input(h, 0, host);
    set_text_input(h, 1, port);
    h.get_by_label("Connect").click();
    h.step();
}

/// Starts an app and connects to the test server; waits for the connected UI.
fn connected() -> Option<Harness<'static, MedisApp>> {
    let Some((host, port)) = redis_addr() else {
        eprintln!("MEDIS_TEST_REDIS is not set; skipping the GUI smoke test");
        return None;
    };
    let mut h = make_harness();
    connect(&mut h, &host, &port);
    expect(&mut h, "Keys: ");
    Some(h)
}

/// Filters the key list with `pattern` (the first text input of the connected UI) and waits
/// for `key` to show up. The first key of a fresh scan is selected automatically.
fn filter_to(h: &mut Harness<'_, MedisApp>, pattern: &str, key: &str) {
    set_text_input(h, 0, pattern);
    press(h, egui::Key::Enter);
    expect(h, key);
}

fn select_key(h: &mut Harness<'_, MedisApp>, key: &str) {
    filter_to(h, key, key);
    h.get_by_label(key).click();
    h.step();
}

fn open_tab(h: &mut Harness<'_, MedisApp>, tab: &str) {
    h.get_by_role_and_label(Role::Tab, tab).click();
    h.step();
}

#[test]
fn a_connect_and_browse() {
    let Some(mut h) = connected() else { return };
    // The connect form is gone; the key browser has the key count and the DB selector.
    assert!(!has(&h, "Quick Connect"));
    expect(&mut h, "DB:");
    assert!(h.query_all_by_role(Role::ComboBox).next().is_some(), "no DB selector");
    // Wait for the INFO reply: the db shows its key count, e.g. `0 (1509)`.
    assert!(wait_for(&mut h, "db key count", |h| {
        h.query_all_by_role(Role::ComboBox).any(|n| n.accesskit_node().value().is_some_and(|v| v.contains('(')))
    }));
    // The key list lists keys; "greeting" is one of them.
    filter_to(&mut h, "greeting", "greeting");
}

#[test]
fn b_select_keys_of_each_type() {
    let Some(mut h) = connected() else { return };

    select_key(&mut h, "greeting");
    // The string editor shows the value.
    assert!(
        wait_for(&mut h, "hello world in editor", |h| {
            h.query_all_by_role(Role::MultilineTextInput)
                .any(|n| n.accesskit_node().value().is_some_and(|v| v.contains("hello world")))
        }),
        "editor did not show the string value"
    );

    select_key(&mut h, "user:1");
    expect(&mut h, "email");

    select_key(&mut h, "tags");
    expect(&mut h, "rust");
}

#[test]
fn c_terminal() {
    let Some(mut h) = connected() else { return };
    open_tab(&mut h, "Terminal");
    h.run();
    // The terminal input takes the focus when the tab opens.
    type_focused(&mut h, "PING");
    press(&mut h, egui::Key::Enter);
    expect(&mut h, "PONG");

    type_focused(&mut h, "SET smoke:t 1");
    press(&mut h, egui::Key::Enter);
    expect(&mut h, "OK");

    type_focused(&mut h, "GET smoke:t");
    press(&mut h, egui::Key::Enter);
    // The echoed `redis> GET smoke:t` line is already there; the reply is a row that is exactly `1`.
    assert!(wait_for(&mut h, "GET reply", |h| h.query_all_by_label("1").next().is_some()), "GET reply never appeared");

    type_focused(&mut h, "DEL smoke:t");
    press(&mut h, egui::Key::Enter);
    // Integer replies print bare, so now there are two rows that are exactly `1` (GET and DEL).
    assert!(
        wait_for(&mut h, "DEL reply", |h| h.query_all_by_label("1").count() >= 2),
        "DEL reply never appeared"
    );
}

#[test]
fn d_config_tab() {
    let Some(mut h) = connected() else { return };
    open_tab(&mut h, "Config");
    expect(&mut h, "maxmemory");
}

#[test]
fn e_pattern_filters_keys() {
    let Some(mut h) = connected() else { return };
    filter_to(&mut h, "greeting", "greeting");
    set_text_input(&mut h, 0, "bulk:key:1*");
    press(&mut h, egui::Key::Enter);
    expect_gone(&mut h, "greeting");
    expect(&mut h, "bulk:key:1");
    // A pattern without matches empties the list.
    set_text_input(&mut h, 0, "smoke:nothing:*");
    press(&mut h, egui::Key::Enter);
    // ("No keys found" is painted without an AccessKit node, so check that no key row is left.)
    assert!(
        wait_for(&mut h, "empty key list", |h| h.query_all_by_role(Role::ListBoxOption).next().is_none()),
        "the key list was not emptied"
    );
}

#[test]
fn f_bad_port_shows_an_error() {
    if redis_addr().is_none() {
        eprintln!("MEDIS_TEST_REDIS is not set; skipping the GUI smoke test");
        return;
    }
    let mut h = make_harness();
    connect(&mut h, "127.0.0.1", "1");
    // The failed connect opens an alert dialog with an OK button.
    assert!(wait_for(&mut h, "alert", |h| h.query_all_by_label("OK").next().is_some()), "no error alert");
    h.get_by_label("OK").click();
    h.step();
    // Back on the connect form, still alive.
    expect(&mut h, "Quick Connect");
    assert!(h.query_all_by_label("Connect").next().is_some());
}

#[test]
fn g_list_view() {
    let Some(mut h) = connected() else { return };
    select_key(&mut h, "queue:jobs");
    expect(&mut h, "job1");
}

#[test]
fn h_zset_view() {
    let Some(mut h) = connected() else { return };
    select_key(&mut h, "leaderboard");
    expect(&mut h, "alice");
}
