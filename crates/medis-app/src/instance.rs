//! The contract between the app shell and the UI modules.
//!
//! * [`Instance`] is one tab (one connection). Every UI module keeps its own state in a field
//!   of it (`inst.keys`, `inst.content`, ...), plus a few shared fields (`db`, `selected_key`,
//!   `content_version`, `tab`).
//! * [`Cx`] is handed to every module call. It carries the egui context, the palette, storage,
//!   and the helpers for opening dialogs.
//! * Modules talk to Redis with [`Instance::send`] and get the answer back in their own
//!   `handle_evt` (the app routes it by the [`Module`] that sent the request).

use std::collections::HashMap;
use std::sync::Arc;

use egui::Context;
use medis_core::storage::Storage;
use medis_core::worker::{ConnectedInfo, Op, next_req_id};
use medis_core::{ConnectionConfig, Evt, Req, ReqId, Worker};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::ui::favorites::Favorites;
use crate::ui::modal::{Callback, ModalSpec, Modals, Outcome};
use crate::ui::theme::Palette;
use crate::ui::{config, connect, content, editor, footer, keys, terminal};

/// The UI modules that can send requests and receive their answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Module {
    Keys,
    Content,
    Editor,
    Terminal,
    Config,
    Footer,
}

impl Module {
    pub const ALL: [Module; 6] = [
        Module::Keys,
        Module::Content,
        Module::Editor,
        Module::Terminal,
        Module::Config,
        Module::Footer,
    ];
}

/// The three tabs of the connected right-hand pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MainTab {
    #[default]
    Content,
    Terminal,
    Config,
}

/// Where an instance is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// Showing the connection screen.
    #[default]
    Form,
    /// Connect request sent, waiting (see `Instance::status`).
    Connecting,
    /// Connected; the key browser and tabs are showing.
    Connected,
}

/// Things a module can ask the app to do.
#[derive(Debug)]
pub enum AppAction {
    /// Open a new instance (tab) and activate it.
    NewInstance,
    /// Close the instance with this id.
    CloseInstance(u64),
}

/// One tab of the app.
pub struct Instance {
    /// Unique, stable for the life of the tab.
    pub id: u64,
    /// Tab title: `Medis` while on the connection screen, the connection title once connected.
    pub title: String,
    pub phase: Phase,
    /// `SSH connecting...` / `Redis connecting...` while `phase == Connecting`.
    pub status: String,
    /// Filled when connected: title, connection key, server version, cluster flag, config.
    pub info: Option<Box<ConnectedInfo>>,

    // ---- shared, connected-phase state (any module may read; the owner writes) ----
    /// Active right-hand tab.
    pub tab: MainTab,
    /// Current database. Written by `keys` when the user changes it. The app also updates it when
    /// the worker reports `Evt::DbSelected` (a `SELECT` typed in the terminal).
    pub db: u32,
    /// The key selected in the key browser (written by `keys`, read by `content` and `footer`).
    pub selected_key: Option<String>,
    /// Bump after any save/add/delete/expire/rename on the selected key; the footer refreshes
    /// when it changes.
    pub content_version: u64,

    // ---- per-module state; each is re-created (`Default`) on every connect ----
    pub keys: keys::KeysState,
    pub content: content::ContentState,
    pub editor: editor::EditorState,
    pub terminal: terminal::TerminalState,
    pub config: config::ConfigState,
    pub footer: footer::FooterState,

    // ---- connection screen ----
    pub connect: connect::ConnectState,

    tx: Option<UnboundedSender<Req>>,
    rx: Option<UnboundedReceiver<Evt>>,
    owners: HashMap<ReqId, Module>,
}

impl Instance {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            title: "Medis".into(),
            phase: Phase::Form,
            status: String::new(),
            info: None,
            tab: MainTab::Content,
            db: 0,
            selected_key: None,
            content_version: 0,
            keys: Default::default(),
            content: Default::default(),
            editor: Default::default(),
            terminal: Default::default(),
            config: Default::default(),
            footer: Default::default(),
            connect: Default::default(),
            tx: None,
            rx: None,
            owners: HashMap::new(),
        }
    }

    /// `true` for a cluster connection (db 0 only, no db selector).
    pub fn is_cluster(&self) -> bool {
        self.info.as_deref().is_some_and(|i| i.cluster)
    }

    /// The key of the saved patterns for the current db: `"<sshHost>|<host>|<port>|<db>"`.
    pub fn pattern_key(&self) -> String {
        let conn = self.info.as_deref().map(|i| i.connection_key.as_str()).unwrap_or("||");
        format!("{conn}|{}", self.db)
    }

    /// Sends `op` to the worker. The answer (`Evt::Reply`, or `Evt::ScanBatch` /
    /// `Evt::ScanDone` for scans) is delivered to `owner`'s `handle_evt`, carrying the returned
    /// id. Does nothing useful (the id is still returned) when not connected.
    pub fn send(&mut self, owner: Module, op: Op) -> ReqId {
        let id = next_req_id();
        if let Some(tx) = &self.tx
            && tx.send(Req::new(id, op)).is_ok()
        {
            self.owners.insert(id, owner);
        }
        id
    }

    /// Starts connecting with `cfg` (defaults are applied). Switches to `Phase::Connecting`.
    pub fn start_connect(&mut self, cx: &Cx, cfg: &ConnectionConfig) {
        self.teardown();
        let _guard = cx.rt.enter();
        let ctx = cx.ctx.clone();
        let (tx, rx) = Worker::spawn(cfg.with_defaults(), Arc::new(move || ctx.request_repaint()));
        self.tx = Some(tx);
        self.rx = Some(rx);
        self.phase = Phase::Connecting;
        self.status = "Redis connecting...".into();
        if cfg.ssh {
            self.status = "SSH connecting...".into();
        }
    }

    /// Closes the connection and goes back to the connection screen (tab title `Medis`).
    pub fn disconnect(&mut self) {
        self.teardown();
        self.phase = Phase::Form;
    }

    fn teardown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Req::new(next_req_id(), Op::Disconnect));
        }
        self.rx = None;
        self.owners.clear();
        self.info = None;
        self.title = "Medis".into();
        self.status.clear();
        self.tab = MainTab::Content;
        self.db = 0;
        self.selected_key = None;
        self.content_version = 0;
        self.keys = Default::default();
        self.content = Default::default();
        self.editor = Default::default();
        self.terminal = Default::default();
        self.config = Default::default();
        self.footer = Default::default();
    }

    /// The text for the native window title.
    pub fn window_title(&self) -> String {
        match (&self.info, self.phase) {
            (Some(info), Phase::Connected) => format!("(Redis {}) {}", info.version, info.title),
            _ => "Medis".into(),
        }
    }

    /// Pulls every pending event from the worker, routing each one. Call once per frame.
    pub fn drain_events(&mut self, cx: &mut Cx) {
        loop {
            let evt = match self.rx.as_mut().map(|rx| rx.try_recv()) {
                Some(Ok(evt)) => evt,
                _ => break,
            };
            self.dispatch(cx, evt);
        }
    }

    fn dispatch(&mut self, cx: &mut Cx, evt: Evt) {
        match &evt {
            Evt::Status(s) => {
                self.status = s.clone();
                return;
            }
            Evt::Connected(info) => {
                self.title = info.title.clone();
                self.info = Some(info.clone());
                self.phase = Phase::Connected;
                self.status.clear();
                self.connect.on_connected(cx);
            }
            Evt::ConnectFailed(msg) => {
                self.disconnect();
                cx.alert(msg.clone());
                return;
            }
            Evt::ConnectionLost(msg) => {
                self.disconnect();
                cx.alert(format!("Redis Error: Connection failed. ({msg})"));
                return;
            }
            Evt::Closed => {
                // Only reached when the worker stops on its own.
                if self.phase != Phase::Form {
                    self.disconnect();
                }
                return;
            }
            Evt::DbSelected(n) => self.db = *n,
            _ => {}
        }

        let target = match &evt {
            Evt::Reply { id, .. } | Evt::ScanDone { id, .. } => Some(self.owners.remove(id)),
            Evt::ScanBatch { id, .. } => Some(self.owners.get(id).copied()),
            _ => None,
        };
        match target {
            Some(Some(module)) => self.route(module, cx, &evt),
            Some(None) => {} // nobody is waiting for it any more
            None => {
                for module in Module::ALL {
                    self.route(module, cx, &evt);
                }
            }
        }
    }

    fn route(&mut self, module: Module, cx: &mut Cx, evt: &Evt) {
        match module {
            Module::Keys => keys::handle_evt(self, cx, evt),
            Module::Content => content::handle_evt(self, cx, evt),
            Module::Editor => editor::handle_evt(self, cx, evt),
            Module::Terminal => terminal::handle_evt(self, cx, evt),
            Module::Config => config::handle_evt(self, cx, evt),
            Module::Footer => footer::handle_evt(self, cx, evt),
        }
    }
}

/// Per-call context for every module.
pub struct Cx<'a> {
    pub ctx: &'a Context,
    /// Colours of the active theme.
    pub pal: Palette,
    /// Favorites, patterns and pane sizes on disk.
    pub storage: &'a Storage,
    /// Shared favorites list (all tabs).
    pub favs: &'a mut Favorites,
    /// Dialog queue. Prefer the helpers below.
    pub modals: &'a mut Modals,
    /// The tokio runtime (needed to start workers).
    pub rt: &'a tokio::runtime::Handle,
    /// The instance this call is for.
    pub inst_id: u64,
    /// `true` while a dialog is open. Key events are already stripped from the input then, but
    /// modules should not start new work from other triggers (timers are fine).
    pub blocked: bool,
}

impl Cx<'_> {
    /// An OK-only message box.
    pub fn alert(&mut self, message: impl Into<String>) {
        self.modals.alert(self.inst_id, message);
    }

    /// Opens a confirm / form / alert dialog. `then` runs when it closes, with the instance that
    /// opened it. For a confirm dialog the outcome is `Outcome::Ok(vec![])` or `Outcome::Cancel`.
    pub fn modal(
        &mut self,
        spec: ModalSpec,
        then: impl FnOnce(&mut Instance, &mut Cx, Outcome) + 'static,
    ) {
        let cb: Callback = Box::new(then);
        self.modals.open(self.inst_id, spec, Some(cb));
    }
}
