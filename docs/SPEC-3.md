# SPEC-3: Terminal, Config tab, footers/TabBar/Modal/AddButton, utilities

"(verify)" means jquery.terminal/ioredis library behaviour recalled from memory. "Port:" marks a deliberate fix in the Rust version.

## 6. Terminal tab
### 6.1 Widget
- No greeting. The scrollback limit is 200 output lines.
- Prompt `redis> ` in #fff. The background is #000; the font is monospace; text is selectable.
- The input is focused when the Terminal tab becomes visible.
- `exit` goes to Redis; there is no client-side exit.
- A successful `SELECT n` (any case, quoted n allowed) updates the UI db, which refreshes the key list and resets the pattern.
- Port: the terminal uses a DEDICATED connection that mirrors the current db. The original shared the GUI connection, so MULTI, WATCH, AUTH and BLPOP leaked into the GUI.

### 6.2 Input
- Empty line → ignored.
- Tokenise with exact `sdssplitargs` semantics:
  - Split on whitespace.
  - "..." supports `\n \r \t \b \a \\ \" \xHH`.
  - '...' supports `\'`.
  - A closing quote must be followed by whitespace or the end of the line.
  - Unbalanced quotes → print error `Invalid argument(s)`.
  - Whitespace-only input → ignore.
- Echo `redis> <line>` into the scrollback.
- FLUSHALL / FLUSHDB (any args):
  - Nested prompt `Are you sure (y/n)? ` in #aac6e3.
  - Port: accept only trimmed, case-insensitive y/yes (run the command) or n/no (cancel). Anything else re-prompts. Ctrl+C cancels.
- No client-side clear/help. Port additions:
  - Cmd/Ctrl+L clears the scrollback.
  - Up/Down history.
  - Tab completion.

### 6.3 Execution
While a command runs, input is disabled.
- **`MONITOR`** (exactly 1 arg):
  - Open a separate monitor connection.
  - Echo `Enter monitor mode. Press Ctrl+C to exit.` in #aac6e3.
  - Stream lines (6.8). Typed input is ignored. Ctrl+C exits and disconnects the monitor connection.
- **`SUBSCRIBE` / `PSUBSCRIBE`** (more than 1 arg):
  - Open a separate pubsub connection.
  - Echo `Enter subscription mode. Press Ctrl+C to exit.` in #aac6e3. The prompt is empty in this mode.
  - Stream messages (6.8). Ctrl+C exits and disconnects.
  - Port: on failure, show the error and return to the normal prompt. Close the connection when the tab or instance closes.
- **Everything else:** run the raw command and render the reply (6.7). Errors are rendered too.
- Medis's own polling shows up in MONITOR. That is acceptable.

### 6.4 History
- Last ~60 commands per connectionKey (`<sshHost>|<host>|<port>`), persisted in app data and shared across tabs and restarts.
- Empty lines are not stored. Port: do not store lines starting with AUTH.

### 6.5 Keys
- Ctrl+C: in a nested mode (confirm, monitor, subscribe), exit it; at the top level, clear the input line.
- Copy (Cmd+C on macOS) works on a selection.
- App shortcuts Cmd/Ctrl + digits/T/W/N/R/Q must still reach the app while the terminal has focus.

### 6.6 Completion
- Tab completes the FIRST token only, against the lower-case list of top-level Redis command names.
- Matches are prefix matches. Keep the user's typed prefix as is. The completed suffix is upper-case if the whole typed prefix is upper-case (an empty prefix counts as upper-case), otherwise lower-case.
- Multiple matches → cycle or list them. No key or argument completion.

### 6.7 Reply rendering (Port: text is escaped / rendered as plain text, not HTML)
No redis-cli decorations: no `(integer)`, `(nil)`, quotes or `(error)` prefix.
- **Array:** one row per element, `<index>` then the item.
  - The index is 0-based, #848080, min-width 28px, right-aligned, with a 10px gap.
  - Nested arrays indent recursively. An empty array renders nothing.
- **Integer / double:** #78CF8A.
- **String** (bulk/simple/status, including `OK`):
  - #d6ec9c, with newlines kept as line breaks. An empty string renders an empty line.
  - Decode binary lossily as UTF-8.
- **Nil:** `null` in #cf7ea9.
- **Error:** the error message as is (e.g. `ERR unknown command ...`) in #ee6868.
- **RESP3 Map** (not reachable with RESP2): key rows, key bold #cda869.

### 6.8 Monitor and message lines
- **Monitor:** `<time> <COMMAND> <args joined by space>`.
  - The line base colour is #8f9d6a.
  - time: #3d3d3d in the original (Port: use a readable grey, and format as local `HH:MM:SS.ffffff`).
  - COMMAND: upper-case, #cf7ea9.
  - Key arguments: #cda869 (highlight the first arg for single-key commands; best effort).
  - Port: quote args redis-cli style when they contain spaces or non-printables.
- **Message:** `<channel> <message>`, the message in #8f9d6a.
  - Port: the channel uses a visible colour. For pmessage show `<pattern> <channel> <message>`.

## 7. Config tab
### 7.1 Loading
- `CONFIG GET *` is issued once when the content mounts. A Reload button re-runs it.
  - Port: on error, show the message with Reload still usable.
- Map the flat reply into name→value.
- **Grouping:**
  - Predefined groups, in order, keep only the names that are present.
  - The leftovers go to `Other` in server order.
  - Empty groups are dropped.
  - Port: deep-copy the groups on every load.
- **Groups** ([b]=bool yes/no, [n]=number, [a|b]=enum, untagged=text):
  - **General:** port[n] bind unixsocket unixsocketperm[n] daemonize[b] pidfile tcp-backlog[n] tcp-keepalive[n] timeout[n] databases[n]
  - **Logging:** loglevel[debug|verbose|notice|warning] logfile syslog-enabled[b] syslog-ident syslog-facility
  - **Snap Shotting:** dbfilename dir save stop-writes-on-bgsave-error[b] rdbcompression[b] rdbchecksum[b]
  - **Replication:** slaveof masterauth slave-serve-stale-data[b] slave-read-only[b] repl-diskless-sync[b] repl-diskless-sync-delay[n] repl-ping-slave-period[n] repl-timeout[n] repl-disable-tcp-nodelay[b] repl-backlog-size repl-backlog-ttl[n] slave-priority[n] min-slaves-to-write[n] min-slaves-max-lag[n]
    - Port: also include replicaof, replica-serve-stale-data[b], replica-read-only[b], repl-ping-replica-period[n], replica-priority[n], min-replicas-to-write[n], min-replicas-max-lag[n].
  - **Security:** requirepass rename-command
  - **Limits:** maxclients maxmemory maxmemory-policy[noeviction|volatile-lru|allkeys-lru|volatile-random|allkeys-random|volatile-ttl] maxmemory-samples[n]
    - Port: add volatile-lfu and allkeys-lfu to the policy enum.
  - **Append Only Mode:** appendonly[b] appendfilename appendfsync[everysec|always|no] no-appendfsync-on-rewrite[b] auto-aof-rewrite-percentage[n] auto-aof-rewrite-min-size aof-load-truncated[b]
    - Port: aof-load-truncated is [b]; the original's [n] was a typo.
  - **LUA Scripting:** lua-time-limit[n]
  - **Cluster:** cluster-enabled[b] cluster-config-file cluster-node-timeout[n] cluster-slave-validity-factor[n] cluster-migration-barrier[n] cluster-require-full-coverage[b]
  - **Slow Log:** slowlog-log-slower-than[n] slowlog-max-len[n]
  - **Latency Monitor:** latency-monitor-threshold[n]
  - **Event Notification:** notify-keyspace-events
  - **Advanced Config:** hash-max-ziplist-entries[n] hash-max-ziplist-value[n] list-max-ziplist-entries[n] list-max-ziplist-value[n] set-max-intset-entries[n] zset-max-ziplist-entries[n] zset-max-ziplist-value[n] hll-sparse-max-bytes[n] activerehashing[b] client-output-buffer-limit hz[n] aof-rewrite-incremental-fsync[b]
    - Port: add the hash/zset `*-listpack-*` and lazyfree-* [b] options.

### 7.2 Rows
Each row is a label (170px, right-aligned) followed by a control. The control is picked in this order:
1. [b] and the value is yes/no → checkbox.
2. [n] and the value is a plain integer string → numeric input.
3. Enum containing the value → dropdown.
4. Otherwise → text input.

**Writeable names** (everything else is read-only/disabled). Port: add the replica-* equivalents, the *-listpack-* names and the lazyfree-* names.
- **Persistence:** dbfilename, dir, save, rdbcompression, stop-writes-on-bgsave-error, appendonly, appendfsync, aof-rewrite-incremental-fsync, aof-load-truncated, auto-aof-rewrite-percentage, auto-aof-rewrite-min-size
- **Security:** requirepass, masterauth
- **Clients and memory:** maxclients, maxmemory, maxmemory-policy, maxmemory-samples, client-output-buffer-limit, timeout, tcp-keepalive, lazyfree-lazy-eviction, lazyfree-lazy-expire, lazyfree-lazy-server-del
- **Replication:** repl-disable-tcp-nodelay, repl-diskless-sync, repl-diskless-sync-delay, repl-ping-slave-period, repl-timeout, repl-backlog-ttl, repl-backlog-size, slave-serve-stale-data, slave-read-only, slave-lazy-flush, slave-priority, min-slaves-to-write, min-slaves-max-lag
- **Cluster:** cluster-require-full-coverage, cluster-node-timeout, cluster-migration-barrier, cluster-slave-validity-factor
- **Encoding thresholds:** hash-max-ziplist-entries, hash-max-ziplist-value, list-max-ziplist-entries, list-max-ziplist-value, list-max-ziplist-size, list-compress-depth, set-max-intset-entries, zset-max-ziplist-entries, zset-max-ziplist-value, hll-sparse-max-bytes
- **Monitoring:** lua-time-limit, slowlog-log-slower-than, slowlog-max-len, latency-monitor-threshold, watchdog-period, notify-keyspace-events
- **Other:** activerehashing, hz, loglevel

Any edit marks the field dirty for both "apply" and "rewrite". Port: reverting to the original value clears the dirty flag.

**Layout:** a centred 430px-wide column with h3 group headers (20px), and a right-aligned button group with a 20px vertical margin.

### 7.3 Buttons: `Reload` (always), `Save To Config File` (rewrite-dirty), `Apply` (primary, apply-dirty)
- **Reload** (if dirty, confirm first):
  - title `Reload config?`
  - button `Reload`
  - content `Are you sure you want to reload the config? Your changes will be lost if you reload the config.`
  - It then reloads and clears the dirty state.
- **Apply** (confirm):
  - title `Apply the changes`
  - button `Apply`
  - content `Are you sure you want to apply the changes? The changes are only valid for the current session and will be lost when Redis restarts.`
  - It then pipelines `CONFIG SET name value` for each dirty name and clears apply-dirty. The rewrite-dirty flags are kept.
  - Port: check each reply, alert the first error and keep the failed names dirty.
- **Save** (confirm):
  - title `Save the changes`
  - button `Save`
  - content `Are you sure you want to apply the changes and save the changes to the config file?`
  - It then runs the apply pipeline (no second modal), then `CONFIG REWRITE`, then clears rewrite-dirty.
  - On error, alert `err.message` (e.g. `ERR The server is running without a config file`). Cancel is silent.

## 8. Footers, TabBar, Modal, AddButton
### 8.1 Content footer
- Always visible at the bottom of the right pane, on all three tabs. It describes the selected key.
- One pipeline: `PTTL`, `OBJECT ENCODING`, and the size command: STRLEN → `Bytes`; HLEN, LLEN, SCARD, ZCARD → `Members`.
- Spans, each with 5px horizontal margin, in this order:
  - `<Unit>: N`, only if N > 0.
  - `Encoding: <enc>`.
  - `TTL: <fmt>`, only if pttl ≥ 0.
- **TTL format:**
  - Units: ms=1, s=1000, min=60000, h=3600000, d=86400000.
  - Pick the largest unit ≤ the value, round to at most 2 decimals, strip trailing zeros, no space.
  - Examples: `500ms`, `1.5s`, `2min`, `3.25h`, `7d`. 0 → `0ms`.
- **Refresh triggers:** a key or type change, and any content save, add, delete or expire. There is no timer.
- Port: also refresh after a rename. Empty for no key or type none.

### 8.2 Key-browser footer
`Keys: N` on the left; the `DB:` select on the right (SPEC-2).

### 8.3 TabBar (right pane header, right-aligned)
- Tabs: `Content`, `Terminal`, `Config`, with icons book, window and cog.
- Layout: padding 12px; the active tab has background #dbdfe1; a 1px bottom border #d3d3d3.
- All three panes keep their state while hidden.
- Defaults to Content per connection. No shortcuts.

### 8.4 Modal
- **API:** one modal at a time per window. The result is OK(value) or Cancel (None). It blocks ALL input to the window, including app shortcuts (Port: block Cmd+1..9 too).
- **Props:** title, content text, button label (default `OK`), and an optional form schema.
- **Form fields:**
  - The label has a trailing colon (e.g. `Key Name:`) and is left of the input (80px, right-aligned). Inputs are full width.
  - All fields are required. Port: require non-empty strings everywhere, and parse numbers.
  - Show validation errors (red #ff2a1c, 12px) only after a failed submit.
- **Form modals:**

| Purpose | Button | Fields |
|---|---|---|
| Create key | `Create Key` | `Key Name:` (string); `Type:` (enum string, hash, list, set, zset; default first) |
| Duplicate key | `Duplicate Key` | `Target Key:` (string); `Keep TTL:` (bool) |
| Set expiration | `Set Expiration` | `PTTL (ms):` (number, default = current PTTL) |
| Hash add | `Insert Member` | `Key:` (string) |
| List add | `Insert Item` | `Insert To:` (enum head, tail) |
| Set add | `Insert Member` | `Value:` (string) |
| ZSet add | `Insert Member` | `Value:` (string); `Score:` (number) |

- **Confirm modals** (warning icon + content paragraph). Title / button / content:
  - `Delete the bookmark?` / `Delete` / `Are you sure you want to delete the selected bookmark? This action cannot be undone.`
  - `Delete selected key?` / `Delete` / `Are you sure you want to delete the selected key? This action cannot be undone.`
  - `Delete selected item?` / `Delete` / `Are you sure you want to delete the selected item? This action cannot be undone.`
  - `Overwrite the key?` / `Overwrite` / `Key "<name>" already exists. Are you sure you want to overwrite this key?`
  - `Overwrite the field?` / `Overwrite` / `Field "<name>" already exists. Are you sure you want to overwrite this field?`
  - The config modals listed in 7.3.
- **Buttons:**
  - Order: `Cancel`, then the action. Right-aligned, with 20px above.
  - In confirm modals, Cancel is primary and focused, and Enter = Cancel (destructive safety; keep this).
  - In form modals, the action is primary, the first input is focused, and Enter submits.
- **Keys:** Esc cancels. Tab cycles the fields and buttons (Port: Shift+Tab goes backward).
- **Visual:** a sheet panel, 420px wide, background #efefef, border 1px #a3a3a3 (no top border), padding 18px 20px 18px 100px (the left gutter holds the icon or labels). The title is bold 14px; the body text is 12px. It slides down from the top over 150ms. The overlay has no dim (a light dim is acceptable).
- **Alerts** (an OK-only message box), verbatim:
  - `The key already exists`
  - `Update Failed`
  - `The field already exists`
  - `Member already exists`
  - `Added successfully`
  - `Please wait for data been loaded before saving.`
  - `The json is invalid. Please check again.`
  - `Redis save failed: <msg>`
  - `SSH Error: <msg>`
  - `Redis Error: Access denied. Please double-check your password.`
  - `Redis Error: Connection failed. (<err>)`
  - `Save Successfully`
  - Raw redis error messages.

### 8.5 AddButton
- A small 16×16 `+` box: 1px #ccc border, radius 2, gradient #fff→#efefef, text #888.
- Placed at the top-right of a list header; an optional reload (↻) box sits 20px to its left.
- Header titles: key list `name` (with reload, which calls refresh); hash `key`; list `item`; set `member`; zset `member`.

### 8.6 Status
- There is no global status bar.
- **Connect button labels:** `SSH connecting...` / `Redis connecting...`.
- **Key list:** `Scanning...(cursor N)` / `Scan more`.
- **Footers.**

## 9. Utilities
- **Instance ids:** any unique ID.
- **Instance title:** `remote + host:port`.
  - `remote = name + '/'` if a favorite name is set; else `sshHost + '/'` if ssh is enabled (Port: only when enabled); else ''.
- **connectionKey:** `${sshHost||''}|${host}|${port}`. Pattern store key: `${connectionKey}|${db}`.
- **Window title:** connected → `(Redis <redis_version>) <title>`; otherwise `Medis`. The Pattern Manager window is titled `Manage Patterns`.
- **Disconnect:** the tab title goes back to `Medis`.
- **Pattern Manager** (a 600×300 window or modal viewport, resizable):
  - Left list (210×236, white, border #c5c5c5) of pattern names; the active row is #116cd6 with white text. Below it, `+` and `-` buttons (23×18).
    - `+` adds `{name:'*', value:'*'}` and selects it.
    - `-` removes the selection and selects the previous one (or 0). It is disabled when nothing is selected.
  - Right form (328×236): `Name:` and `Pattern:` text fields and a `Save` button. Save is disabled if either field is empty; it saves, then alerts `Save Successfully`.
  - Port: the manager is opened for the active connectionKey|db, and the main dropdown updates live.
- **ZSet insert position:** `partition_point` by score (f64, ±inf), negated when sorting descending. Ties go before existing equal elements.
- **Key list TYPE failure** → an empty type badge.
