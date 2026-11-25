//! UI modules. `connect`, `favorites`, `modal`, `theme` and `widgets` belong to the app shell.
//! `keys`, `content`, `editor`, `terminal`, `config` and `footer` are the connected-phase panels
//! (see `instance.rs` for the contract).

pub mod config;
pub mod connect;
pub mod content;
pub mod editor;
pub mod favorites;
pub mod footer;
pub mod keys;
pub mod modal;
pub mod patterns;
pub mod terminal;
pub mod theme;
pub mod widgets;
