# Medis behaviour spec, part 1 (sections 0–4.2)

This was distilled from the original JS in `../src`. Read the JS when you need more detail.
Path shorthand used below:
- `IC` = `src/renderer/windows/MainWindow/InstanceContent`
- `DC` = `IC/DatabaseContainer`

## 1. App shell
### Windows
- **Main window:** 960×600, min 840×400, background `#ececec`.
- **Pattern Manager window:** 600×300, title "Manage Patterns". It edits the patterns for `"<connectionKey>|<db>"`. The JS loses this argument (a bug). Implement the intended behaviour.
- **macOS lifecycle:** closing the last window does not quit the app.

### Menus
**App menu (macOS only):** About Medis, Services, Hide Medis Cmd+H, Hide Others Cmd+Shift+H, Show All, Quit Cmd+Q.

**File:**
- New Connection Window, CmdOrCtrl+N
- New Connection Tab, CmdOrCtrl+T
- separator
- Close Window, Shift+CmdOrCtrl+W
- Close Tab, CmdOrCtrl+W

**Edit:**
- Undo Z
- Redo Shift+Z
- Cut X
- Copy C
- Paste V
- Select All A

**View:** Toggle Full Screen (Ctrl+Cmd+F on macOS, F11 elsewhere).

**Window:** Minimize CmdOrCtrl+M.

**Help:**
- "Report an Issue..." opens `mailto:medis@zihua.li`.
- "Learn More" opens `http://getmedis.com`.

### Behaviour while a modal is open
Instance actions (new tab, close tab, select tab) are ignored.

### Window title
- Connected: `(Redis <version>) <instance title>`.
- Otherwise: `Medis`.

### Tab shortcuts
- Cmd+1..8 selects tab i.
- Cmd+9 selects the last tab.
- On non-mac, use Ctrl or Alt.

### Instances (tabs)
- Each instance has a key and a title (default `Medis`). It also holds a connection, config, version, `connectionKey` and `connectStatus`.
- `connectionKey = "<sshHost||''>|<host>|<port>"`.
- Title after connect: `<name>/` (falls back to `<sshHost>/`, or empty) followed by `<host>:<port>`.

**Tab strip:**
- Hidden when there is exactly one instance.
- Each tab shows its title and a close ×.
- A trailing `+` button creates a new instance and activates it.
- A tab is selected on mouse down.

**Closing a tab:**
- If the closed tab was active, activate the next tab, or else the previous one.
- If it was the only tab, close the window.
- The port must also DISCONNECT the connection. The JS leaks it.

**Reordering tabs:** dragging along the x axis reorders; the moved tab becomes active.

## 2. Persistence
The JS stores everything in localStorage. The port uses a JSON file in the config dir (see `PLAN.md`).

### favorites
An ordered array of favorite objects with these fields:
- `key`: `favorite-<rand>`. Use a uuid in the port.
- `name`: default `New Favorite`.
- `host`: effective default `localhost`.
- `port`: a string; effective default `6379`.
- `password`
- `ssl` (bool)
- `tlskey`, `tlscert`, `tlsca`: PEM file contents.
- `tlskeyFile`, `tlscertFile`, `tlscaFile`: the file paths.
- `ssh` (bool)
- `sshHost`, `sshUser`, `sshPassword`
- `sshKey`: key file contents, or false.
- `sshKeyFile`: path, or false.
- `sshKeyPassphrase`
- `sshPort`: effective default `22`.

Operations: create (push), remove (by key), update (merge), reorder(from, to).

### patternStore
- A map from `"<connectionKey>|<db>"` to `[{key, name, value}]`.
- `name` and `value` both default to `*`.
- Operations: create, remove(index), update(index, merge).

### sizes
- `contentBarWidth`: default 200. Width of the left table pane in the key-content split.
- `indexBarWidth`: default 60.
- `scoreBarWidth`: default 60.

### Not persisted
- The key-browser sidebar width (default 260, min 250).

### Sync
All windows share the store. A change in one window must reload the store in the others.

## 3. Connection
### UI
The left sidebar has two sections:

**QUICK CONNECT:** the top item; it is active when no favorite is selected.

**FAVORITES:**
- Click a favorite to select it. Double-click selects it and connects.
- Favorites can be reordered by dragging.
- Footer buttons:
  - `+` creates `New Favorite`.
  - `−` deletes the selected favorite after a confirm dialog. Its title is `Delete the bookmark?`, the button is `Delete`, and the text is `Are you sure you want to delete the selected bookmark? This action cannot be undone.` After deleting, select the previous favorite, or Quick Connect.

### Form
The form is 500px wide with right-aligned labels 140px wide. Fields:
- `Name:`, placeholder `Bookmark name`. Shown only when a favorite is selected.
- `Redis Host:`, placeholder `localhost`.
- `Port:`, placeholder `6379`.
- `Password:` (password field)
- `SSL:` checkbox. When checked, it shows:
  - `Private Key:`, read-only path plus a "…" picker. Placeholder `Select Private Key File (PEM)`.
  - `Certificate:`, placeholder `Select Certificate File (PEM)`.
  - `CA:`, placeholder `Select CA File (PEM)`.
  - Picking a file stores its contents in `tls*` and the path in `tls*File`.
- `SSH Tunnel:` checkbox. When checked, it shows:
  - `SSH Host:`
  - `SSH User:`
  - `SSH Password:`. Once a key is chosen, the label becomes `SSH Key:` and the field shows the key path read-only. A key button toggles between choosing a key and clearing it. The file dialog message is `Select a private key (Most often in the ~/.ssh)`.
  - `SSH Key Passphrase:`, shown only if the key contents contain `ENCRYPTED`.
  - `SSH Port:`, placeholder `22`.

**NEW in the port:**
- A `Cluster:` checkbox.
- A `TLS: verify certificate` option. The original never verifies (rejectUnauthorized false), so the default is off.

### Buttons
- **Left side:**
  - When a favorite is selected: `Duplicate`. Copies the favorite plus any unsaved edits into a new favorite with the same name.
  - On Quick Connect: `Add to Favorite`. Creates a favorite from the form with name `Quick Connect`.
- **`Save Changes`:** shown only when the form has changed.
- **Primary button:**
  - While connecting, it shows the status text (`SSH connecting...` or `Redis connecting...`) and is disabled.
  - Otherwise it reads `Save and Connect` if the form has changed, or `Connect`.

### Edit state
- Edits are kept in an overlay on top of the stored favorite.
- `changed` is only tracked for favorites.
- Switching to a different item discards the overlay.
- Connecting:
  - Merges the overlay into the favorite.
  - Applies the defaults host `localhost`, port `6379`, sshPort `22`.
  - Connects, then saves the edits if anything changed.

### Connect flow
1. **SSH step (if enabled):**
   - Set the status to `SSH connecting...`.
   - Connect to sshHost:sshPort as sshUser, using the key plus passphrase, or else the password.
   - Forward to host:port. Then Redis connects to 127.0.0.1:<local forward port>.
   - On an SSH error, show the alert `SSH Error: <msg>` and return to the form.
2. **Redis step:**
   - Set the status to `Redis connecting...`.
   - Apply TLS if ssl is set (ca, key and cert when they are non-empty).
   - No retries, no auto-reconnect. Connect timeout is 10s. Start on db 0.
   - Run AUTH, then INFO, then PING.
   - Errors:
     - A NOAUTH error shows `Redis Error: Access denied. Please double-check your password.`
     - A connection failure, or a dropped connection later, shows `Redis Error: Connection failed. (<error>)` and returns the tab to the form.
   - Version check: if `(major, minor) < (2, 8)`, show this alert and disconnect:
     `Medis only supports Redis >= 2.8 because servers older than 2.8 don't support SCAN command, which means it not possible to access keys without blocking Redis.`
   - Alerts are modal OK dialogs.

### Lua commands
These are the verbatim scripts. Use EVALSHA with an EVAL fallback (`redis::Script`).
- **setKeepTTL**, 1 key, ARGV [value]:
  `local ttl = redis.call("pttl", KEYS[1]) if ttl > 0 then return redis.call("SET", KEYS[1], ARGV[1], "PX", ttl) else return redis.call("SET", KEYS[1], ARGV[1]) end`
- **lremindex**, 1 key, ARGV [index]:
  `local FLAG = "$$#__@DELETE@_REDIS_@PRO@__#$$" redis.call("lset", KEYS[1], ARGV[1], FLAG) redis.call("lrem", KEYS[1], 1, FLAG)`
- **duplicateKey**, 2 keys, ARGV ['TTL' or 'NOTTL']:
  `local dump = redis.call("dump", KEYS[1]) local pttl = 0 if ARGV[1] == "TTL" then pttl = redis.call("pttl", KEYS[1]) end return redis.call("restore", KEYS[2], pttl, dump)`
  - PORT FIX: use 0 when pttl < 0.

### Sessions
- One shared connection per instance.
- The current DB is connection state. A `SELECT` typed in the terminal must update the UI's db.
- SUBSCRIBE and MONITOR use dedicated extra connections.

## 4. Key browser
### Layout
A sidebar split pane (default 260, min 250) with a pattern input, then the key table, then a footer.

### 4.1 Pattern input
- Placeholder `Key name or patterns (e.g. user:*)`.
- A chevron opens a dropdown of the saved patterns for `connKey|db`. Clicking one puts its `value` in the input. The last item is `Manage Patterns...`, which opens the Pattern Manager.
- Changing the db resets the pattern to `''`.
- Creating a key sets both the pattern and the selection to the new key.

### 4.2 SCAN
**Triggers:**
- Refresh 200ms (debounced) after a db, pattern or connection change.
- Changing the db issues `SELECT db`.
- `refresh` clears the keys, resets the cursor to 0, deselects, and scans.
- The reload icon in the header, or Cmd+R, refreshes.
- `Scan more` continues from the stored cursor.

**Pattern handling:** the effective pattern is the input, or `*` when it is empty.
- If the input contains neither `*` nor `?`, it is a plain key:
  - Scan `pattern*` with filterKey = the input.
  - If the key list is empty, run `TYPE input` first. If the type is not `none`, put the key at the top.
  - On the first run, if the exact key exists, do a single SCAN with COUNT 1 and stop.

**`iter(count, times)`:**
1. Run `SCAN cursor MATCH p COUNT count`.
2. Drop filterKey from the results.
3. Pipeline a `TYPE` for every key.
4. Stop when the exact key exists on the first run, the cursor is 0, the total reaches 100 or more, or `total > 0 && times > 200`.
5. If continuing, the next COUNT is 5000 when the total is under 10, 2000 when it is under 50, and 1000 otherwise.

**After the first batch:** auto-select row 0 if nothing is selected.

**The "more" row** appears when the cursor is not 0:
- While scanning: `Scanning...(cursor <c>)` in #ccc.
- Otherwise: `Scan more` in #666.

**Concurrency:** if the pattern changes mid-scan, restart.

### 4.3 Rows
- Row height 24.
- **Type badge column**, width 40. The badge is 32×16, 11px uppercase white text. Label `str` for string; otherwise the type name. Colors:
  - str `#5dc936`
  - list `#fca32a`
  - hash `#b865d0`
  - zset `#fa5049`
  - set `#239ff2`
  - other `#60d4ca`
- **Name column.** Its header holds a reload button and a `+` button.

(Sections 4.3 onward continue in SPEC-2.md.)
