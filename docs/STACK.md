# medis-rs stack notes (researched 2026-11-25; snippets from source reading, not compiled)

Toolchain: rustc 1.98 local; effective MSRV 1.95 (egui 0.37, edition 2024).

## Pins
| crate | version | notes |
|---|---|---|
| eframe / egui / egui_extras | 0.37 | features: `glow, default_fonts, accesskit, persistence, wayland, x11, links, clipboard` (default-features=false), optionally `system_fonts` |
| redis | 1.8 (NOT 2.0 rc) | features: `tokio-rustls-comp, tls-rustls-insecure, cluster-async, connection-manager` (defaults include script, streams, acl) |
| rustls | 0.23, features `["aws-lc-rs"]` | MUST call `rustls::crypto::aws_lc_rs::default_provider().install_default()` at startup |
| russh | 0.65 | keys are in `russh::keys` (russh-keys crate is dead); default aws-lc-rs |
| tokio | 1 (full) | |
| rfd | 0.18 | use AsyncFileDialog |
| muda | 0.21 | macOS-only native menu (`menu.init_for_nsapp()`); egui MenuBar elsewhere |
| futures | 0.3 | StreamExt for pubsub/monitor/scan iter |

## eframe 0.37 API (changed!)
- `impl eframe::App { fn logic(&mut self, ctx:&egui::Context, f:&mut Frame) {} fn ui(&mut self, ui:&mut egui::Ui, f:&mut Frame); }` — `update` is REMOVED.
- Panels: `egui::Panel::{left,right,top,bottom}(id).show(ui, |ui| ..)`; `egui::CentralPanel::default().show(ui, ..)` last. `SidePanel/TopBottomPanel/show_inside` deprecated.
- `Id::new` deprecated → `Id::unique` / `id_salt(..)`.
- Menu: `egui::MenuBar::new().ui(ui, |ui| ui.menu_button("File", |ui| ..))`.
- Table: `egui_extras::{TableBuilder, Column}`; `.sense(Sense::click())`, `body.rows(h, n, |mut row| { row.index(); row.col(..); row.response().clicked() })`; `.stick_to_bottom(true)`, `.scroll_to_row(i,None)`.
- Viewports: `ctx.show_viewport_immediate(ViewportId::from_hash_of("x"), ViewportBuilder::default()..., |ui, _class| ..)`.
- Shortcuts: `ui.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::N)))`.

## Async pattern
Build a multi-thread tokio Runtime in main, `rt.enter()`, store the runtime in App (keep it alive). UI→worker: `tokio::sync::mpsc::UnboundedSender<Req>`; worker→UI: `UnboundedSender<Evt>` + `ctx.request_repaint()` (Context is Clone+Send). Drain with `try_recv` in `App::logic`. Never `block_on` in ui.

## redis 1.8
- `AsyncConnectionConfig::new().set_connection_timeout(Some(5s)).set_response_timeout(Some(30s))` — **the default response timeout is 500ms**; `client.get_multiplexed_async_connection_with_config(&cfg)`.
- Programmatic info: `ConnectionAddr::Tcp(host,port)` / `ConnectionAddr::TcpTls{host,port,insecure,tls_params:None}` `.into_connection_info()?.set_redis_settings(RedisConnectionInfo::default().set_username(..).set_password(..).set_db(n).set_protocol(ProtocolVersion::RESP2))`.
- Custom CA / mTLS: `Client::build_with_tls("rediss://h:p", TlsCertificates{ root_cert: Some(pem), client_tls: Some(ClientTlsConfig{client_cert, client_key}) })` (the URL must be rediss://). Hostname-only relax: `ConnectionAddr::set_danger_accept_invalid_hostnames(true)`.
- Cluster: `redis::cluster::ClusterClientBuilder::new(nodes).username().password().certs(..)/.tls(TlsMode::Insecure)/.danger_accept_invalid_hostnames(true).connection_timeout().response_timeout().build()?.get_async_connection().await?`. SCAN is not auto-routed → find the primaries (CLUSTER NODES) and `con.route_command(&cmd, RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress{host,port}))` (redis::cluster_routing). `node_address_map` exists for SSH/NAT rewriting.
- Value variants: Nil, Int, BulkString(Vec<u8>), Array, SimpleString, Okay, Map, Attribute{data,attributes}, Set, Double, Boolean, VerbatimString{format,text}, BigNumber, Push{kind,data}, ServerError. Always add a `_` arm. Use RESP2 by default for parity with ioredis.
- Script: `redis::Script::new(lua).key(k).arg(a).invoke_async(&mut con)` (EVALSHA with a fallback).
- Manual SCAN: `cmd("SCAN").arg(cursor).arg("MATCH").arg(p).arg("COUNT").arg(n)` → `(u64, Vec<Vec<u8>>)`.
- Pub/sub: `client.get_async_pubsub()` → `psubscribe`, `on_message()`/`into_on_message()`; Monitor: `client.get_async_monitor()` → `on_message::<String>()`.

## russh 0.65 SSH local forward
```rust
struct H; impl russh::client::Handler for H { type Error = russh::Error;
  async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> { Ok(true) } }
let cfg = Arc::new(client::Config{ keepalive_interval: Some(30s), nodelay: true, ..Default::default() });
let mut s = client::connect(cfg, (host, port), H).await?;
s.authenticate_password(user, pw).await?.success();
// key: let k = russh::keys::load_secret_key(path, passphrase)?;  (or decode_secret_key(&str, pass))
//      let hash = s.best_supported_rsa_hash().await?.flatten();
//      s.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(k), hash)).await?.success();
let s = Arc::new(s); let l = TcpListener::bind("127.0.0.1:0").await?; let port = l.local_addr()?.port();
// per accept: s.channel_open_direct_tcpip(rhost, rport as u32, peer_ip, peer_port as u32).await?.into_stream();
//             tokio::io::copy_bidirectional(&mut sock, &mut stream)
```
TLS through a tunnel: the hostname won't match 127.0.0.1, so use danger_accept_invalid_hostnames. Cluster through SSH: use node_address_map (or don't support it; document this).

## rust-anything
It is limited to CLIs, HTTP servers and libraries; GUIs are out of scope. Use it (optionally) only to record golden cases for the pure JS helpers and diff them against the Rust ports.
