# medis-rs port plan

Docs: SPEC-1.md (shell, persistence, connect, scan), SPEC-2.md (rows, key ops, footer, content views, editor), SPEC-3.md (terminal, config tab, misc; pending), STACK.md (crate pins and APIs).

## Workspace layout (medis-rs/, cargo workspace, edition 2024, rust-version 1.95)
```
medis-rs/
  Cargo.toml                 # [workspace] members = ["crates/*"], shared [workspace.dependencies]
  crates/medis-core/         # lib, no egui dependency; tokio-based
    src/lib.rs
    src/config.rs            # ConnectionConfig (all SPEC-1 form fields + cluster/TLS additions), connection_key()
    src/storage.rs           # favorites / pattern store / sizes JSON in dirs::config_dir()/medis/; atomic write
    src/ssh.rs               # russh local-forward tunnel (password / key + passphrase), returns local port + handle
    src/conn.rs              # RedisConn enum { Single(MultiplexedConnection), Cluster(ClusterConnection) }, connect(cfg) with the verbatim SPEC-1 error strings
    src/scan.rs              # SPEC-1 SCAN algorithm (per primary for cluster), key type + TTL batching (pipeline)
    src/scripts.rs           # verbatim Lua: setKeepTTL, lremindex, duplicateKey
    src/ops.rs               # typed ops used by the UI: get/set/hash/list/set/zset CRUD, rename, delete, expire, duplicate, db count probe, INFO
    src/format.rs            # reply formatting for the terminal (redis-cli style), value encode/decode (json pretty, msgpack)
    src/cmdline.rs           # terminal arg splitting (splitargs semantics; SPEC-3)
    src/worker.rs            # Worker: mpsc Req -> Evt loop per connection tab; on_event callback for repaint
    tests/                   # unit tests + integration tests (gated on MEDIS_TEST_REDIS=redis://127.0.0.1:6379)
  crates/medis-app/          # bin "medis", eframe 0.37
    src/main.rs              # rustls provider install, tokio runtime, eframe::run_native
    src/app.rs               # tabs (instances), menus (muda on macOS, egui MenuBar elsewhere), shortcuts
    src/ui/{connect,favorites,keys,content,editor,terminal,config,footer,modal,theme}.rs
```

## Feature additions beyond parity
- TLS: enable flag, CA PEM path, client cert + key paths, "allow insecure (skip verify)", "skip hostname check" (auto on when tunneling over SSH).
- Cluster mode: a seed-node list; SCAN over all primaries; db selector hidden (db 0 only); SSH+cluster unsupported (shown as an error).
- RESP2 by default.

## Lanes
1. core (fixer): medis-core except cmdline/format-terminal → builds, unit tests pass, integration tests pass against local redis.
2. format+cmdline (fixer, after SPEC-3): format.rs + cmdline.rs, golden tests (optionally via rust-anything against the JS helpers).
3. app UI (designer): medis-app against the medis-core API (worker Req/Evt).
4. Verify: cargo build/clippy/test; manual smoke against redis-server (brew).
