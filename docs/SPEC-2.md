# medis-rs behaviour spec — part 2 (sections 4.3–5)

Source: original JS under ../src (KeyList = src/browser/.../KeyList/index.jsx; CT = Content; KeyContent/*). The "Port:" notes are the decisions to implement; otherwise match the original behaviour.

## 4.3 Key list rows
- A virtualised table: 24px header, 24px rows. Two columns:
  - **type** (40px): a badge 32×16, 11px uppercase white text. The label is `str` for string, otherwise the type name. Colours: str `#5dc936`, list `#fca32a`, hash `#b865d0`, zset `#fa5049`, set `#239ff2`, other `#60d4ca`. It is empty until the type has loaded.
  - **name** (rest): the key name, nowrap with ellipsis, inline-editable when it is the `editableKey`. The header holds an AddButton titled `name` with a reload icon (→ refresh) and `+` (→ add key, 4.6).
- There are no TTL or size columns.
- **Row look:** text `#606061`, monospace 12px. A selected row is `#dcdcdc` when the list is unfocused and `#116cd6` with white text when focused. The "loading/scan more" row uses a sans font.
- **Mouse:**
  - Click selects (a no-op if already selected, unless forced).
  - Double-click selects and starts the inline rename.
  - Scrolling cancels the rename.
  - Right-click selects and opens the context menu.
- **Selection:**
  - An invalid index deselects.
  - Original quirk: Up at row 0 or Down at the last row deselects. **Port:** clamp instead.
  - Selecting → content pane: `TYPE key` → the view for that type (5.0). A forced re-select bumps `version` and reloads.

## 4.4 Key list keyboard (list focused, not renaming)
- Backspace (and **Port:** Delete as well) → the delete flow (4.8).
- Up / Down move the selection.
- Cmd+C (Ctrl+C on other platforms) copies the key name; it is a no-op if nothing is selected.
- Cmd+R → refresh.

## 4.5 Key context menu (verbatim, in order)
`Copy to Clipboard` | `Reload` (forced re-select, full reload) | separator | `Set expiration` | `Rename Key...` | `Duplicate Key...` | `Delete`.
- Nothing appears for the loading/"scan more" row.

## 4.6 Add key
- A modal with no title, button `Create Key`. Fields:
  - `Key Name:` (string, min length 1)
  - `Type:` (enum string/hash/list/set/zset, default string)
- **Steps:**
  1. `EXISTS key`. If it exists, alert `The key already exists`.
  2. Otherwise create the key with a placeholder:
     - string → `SET key ''`
     - list → `LPUSH key 'New Item'`
     - hash → `HSET key 'New Key' 'New Value'`
     - set → `SADD key 'New Member'`
     - zset → `ZADD key 0 'New Member'`
  3. Then `onCreateKey(key)`: set the pattern input to the key name. That refreshes the list (200ms debounce; plain-key mode lists the exact key first) and auto-selects row 0, which loads the content.
- Cancel → nothing happens.

## 4.7 Rename (inline)
- **Start:** double-click, or the menu `Rename Key...`. The field becomes editable with all text selected.
- **Commit:**
  - Enter or blur commits. Escape reverts.
  - It acts only if the new name is non-empty and differs from the old one.
- **Steps:**
  1. `EXISTS new`. If it exists, show a modal: title `Overwrite the key?`, button `Overwrite`, body `Key "<new>" already exists. Are you sure you want to overwrite this key?`. Cancel → abort.
  2. `RENAME old new`.
  3. Update the row name. If another loaded row has the new name, remove it and adjust the selection index.
  4. Clear the edit state and refocus the list.
- **Port:** await RENAME and alert on error. Re-select the renamed key so the content pane follows it; the original left it stale.

## 4.8 Delete key
- A modal: title `Delete selected key?`, button `Delete`, body `Are you sure you want to delete the selected key? This action cannot be undone.`
- **On confirm:**
  1. Remove the row and send `DEL key`.
  2. Selection: if `index >= len_after - 1`, use `index - 1`; otherwise keep the index (which is now the next key).
  3. Force-select. An index of -1 deselects.

## 4.9 Duplicate key
- A modal with no title, button `Duplicate Key`. Fields:
  - `Target Key:` (string, min 1)
  - `Keep TTL:` (boolean checkbox, default false)
- Run the duplicateKey Lua script (SPEC-1 §3.4) with `source target TTL|NOTTL`, then `onCreateKey(target)`.
- **Port:**
  - Await the script and alert the error message.
  - Use ttl 0 when `pttl < 0`; the original passed -1 and RESTORE failed.
  - Keep the original's non-REPLACE behaviour, so an existing target gives a BUSYKEY error, which is shown.

## 4.10 Set expiration
1. `PTTL key`.
2. A modal with no title, button `Set Expiration`, number field `PTTL (ms):` defaulting to the current pttl (which may be -1 or -2).
3. On submit:
   - ttl >= 0 → `PEXPIRE key ttl`. A reply <= 0 → alert `Update Failed`.
   - ttl < 0 → `PERSIST key`.
4. Then bump the meta version, which makes the content footer re-fetch the TTL.
- The menu label is `Set expiration` (lowercase e).

## 4.11 Key browser footer
- **Left:** `Keys: <N>`, the key count of the current DB.
- **Right:** `DB:` plus a select with options `0..databases-1`. The label is `"<db> (<count>)"` if count > 0, else `"<db>"`. Before the database count is known there is only `0`.
- **Changing the DB:** `SELECT db`, reset the pattern to `''`, and refresh after 200ms. The content pane is cleared.
- **INFO polling:**
  - Runs on mount, on DB change, and every 10s.
  - Parse `INFO`: split lines on `\r\n`, then on `:`.
  - For `db<N>`, match `keys=(\d+)`; otherwise the count is 0.
- **Database count** (once):
  1. Try `CONFIG GET databases` → `Number(res[1])`.
  2. Else probe on a separate connection: try `SELECT 15`, 16, … upward, cap 30, and return the last success.
  3. If `SELECT 15` fails, probe `SELECT 1`, 2, … and return the last success; if `SELECT 1` fails, return 0.
  4. `databases = result + 1`.
  - **Port:** close the probe connection. A cluster has databases = 1.

## 5.0 Content pane dispatch
- On key select: set `keyType=null`, then run `TYPE key`. Apply the result only if the key is still current.
- **Views by type:**
  - string → String
  - list / set / hash / zset → their own views
  - `none` → a centered trash icon (62px) and `The key has been deleted` (22px, `#ccc`)
  - other (stream, …) → empty. **Port, optional:** a read-only `Unsupported type: <type>` placeholder.
- **Layout for hash/list/set/zset:** a horizontal split.
  - Left: the table, width `contentBarWidth` (default 200, min 80, persisted when dragged).
  - Right: the Editor (5.7).
  - String has no split, just the Editor.
- **Persisted sizes:** `contentBarWidth` 200, `indexBarWidth` 60, `scoreBarWidth` 60 (localStorage `sizes`; the port uses the JSON store).
- **Init:**
  1. Fetch the length: `STRLEN` / `LLEN` / `SCARD` / `ZCARD` / `HLEN`.
  2. Table rows = length (the scrollbar shows the full size).
  3. Rows load lazily in batches until the deepest visible row is loaded. A not-yet-loaded row shows `Loading...`.
  4. Auto-select row 0 once data arrives.
- **Table keyboard:** Backspace (and **Port:** Delete as well) deletes the item. Up / Down move within the loaded rows. Both are ignored while an inline edit is active.
- **Common delete modal:** title `Delete selected item?`, button `Delete`, body `Are you sure you want to delete the selected item? This action cannot be undone.`
  - Afterwards: length−1 and bump `contentVersion` (the footer refreshes).
  - Re-select `sel >= len_after-1 ? sel-1 : sel`.
  - **Port:** clear the editor if the list becomes empty.
- **Saving before the data has loaded** → alert `Please wait for data been loaded before saving.`

## 5.1 String
- **Load:** `STRLEN` plus `GET` as raw bytes. The Editor shows the bytes.
- **Save:** the setKeepTTL Lua script (SPEC-1 §3.4) with a binary-safe value, then bump `contentVersion`.

## 5.2 Hash
- **Members:** `(field: String lossy-utf8, value: Vec<u8>)`. One column, header AddButton `key` (no reload). The field name is inline-editable.
- **Load:** `HSCAN key cursor MATCH * COUNT n`, with n = 500 on the first call and 10000 after. Append in scan order. Continue while `loaded-1 < maxRow` and `cursor != 0`.
  - **Port:** de-duplicate fields and don't restart from cursor 0 once the scan is complete.
- **Select:** the Editor shows the value.
- **Double-click:** rename the field.
- **Context menu:** `Copy to Clipboard` (field) | separator | `Rename Key` | `Delete`.
- **Add:**
  1. A modal with button `Insert Member` and field `Key:` (string).
  2. `HSETNX key field 'New Member'`. If the reply is 0, alert `The field already exists`.
  3. Otherwise append `(field, "New Member")`, length+1, bump the version, and select the last row.
- **Rename field:**
  1. `HEXISTS key target`. If it exists, show the modal: title `Overwrite the field?`, button `Overwrite`, body `Field "<target>" already exists. Are you sure you want to overwrite this field?`. On confirm, remove the loaded duplicate row and length−1.
  2. `MULTI; HDEL key src; HSET key target <value>; EXEC`.
  3. Update the row.
- **Save value:** `HSET key field value`, then bump the version.
- **Delete:** `HDEL key field`. **Port:** the original wrongly also sent the value.

## 5.3 List
- **Members:** strings. **Port:** store bytes and display them lossy.
- **Columns:**
  - `index` (resizable, width `indexBarWidth`, persisted on resize end): a sortable header, `index`. The cell is a grey pill showing `desc ? length-1-row : row`.
  - `item`: header AddButton `item`. No inline editing.
- **Sort:** clicking the index header toggles `desc`, clears the members and selection, and reloads.
- **Load:**
  1. `from = loaded`; `to = min(from==0 ? 200 : from+1000, length-1)`. If `to < from`, stop.
  2. If `desc`: `(from,to) = (-1-to, -1-from)`, and reverse the results.
  3. `LRANGE key from to`. `diff = expected - got`, and `length -= diff`. Discard the result if `desc` changed in flight.
  4. Continue while `loaded-1 < maxRow && diff == 0`.
- **Redis index** = `desc ? -1-row : row`.
- **Save:** `LSET key redisIndex value`.
- **Add:**
  1. A modal with button `Insert Item` and field `Insert To:` (enum head/tail).
  2. `LPUSH` or `RPUSH key 'New Item'`.
  3. Insert locally: head → top, tail → bottom. **Port:** take `desc` into account. Length+1 and bump the version. After a head insert, select row 0.
- **Delete:** the lremindex Lua script with `key redisIndex`.
- **Context menu:** `Delete` only.

## 5.4 Set
- **Members:** strings. **Port:** store bytes. One column, header AddButton `member`.
- **Load:** `SSCAN key cursor COUNT n` (500, then 10000). When the cursor reaches 0, set length = the loaded count.
- **Context menu:** `Delete` only.
- **Add:**
  1. A modal with button `Insert Member` and field `Value:` (string).
  2. `SISMEMBER`. If it exists, alert `Member already exists`.
  3. Otherwise `SADD`, append, length+1, bump the version, and select the last row.
- **Save:**
  1. `SISMEMBER key new`. If it exists, the error is `The value already exists in the set`; the Editor shows it as `Redis save failed: The value already exists in the set`.
  2. Otherwise `MULTI; SREM key old; SADD key new; EXEC`, and update the member.
- **Delete:** `SREM key member`.

## 5.5 ZSet
- **Members:** `(member, score_string)`.
- **Columns:**
  - `score` (resizable, width `scoreBarWidth`, persisted): a sortable header, `score`. The cell is an inline-editable score.
  - `member`: header AddButton `member`.
- **Sort:** clicking the score header toggles `desc`, clears the members, and reloads.
- **Load:**
  1. `from` / `to` as for lists (201 items first, then 1001). **Port:** add the `to < from` guard.
  2. `ZRANGE` (or `ZREVRANGE` when `desc`) `key from to WITHSCORES`.
  3. Discard if `desc` changed or `loaded != from`. `diff` adjusts the length.
  4. Continue as for lists.
- **Score edit:**
  1. Triggered by double-click or the menu `Edit Score`.
  2. `ZADD key newScore member`. On success, update the score text with no re-sort. On error, alert the message.
- **Context menu:** `Copy Score to Clipboard` | separator | `Edit Score` | `Delete`.
- **Add:**
  1. A modal with button `Insert Member` and fields `Value:` (string) and `Score:` (number).
  2. `ZSCORE key value`. If it is non-null, alert `Member already exists`.
  3. `ZADD key score value`.
  4. Insert at the sorted lower bound of `score * (desc ? -1 : 1)`. Length+1, bump the version, and select the new row.
  5. Alert `Added successfully`. **Port:** always insert locally when the position falls within the loaded range or all rows are loaded. Keep the alert.
- **Save member:** `MULTI; ZREM key old; ZADD key <score> new; EXEC`.
- **Delete:** `ZREM key member`. **Port:** the original wrongly also sent the score.

## 5.7 Editor (shared value editor)
- **Input:** an optional `Vec<u8>`. None → an empty pane.
- **Mode detection:**
  - `raw` = lossy UTF-8.
  - `json` = pretty JSON (TAB indent) if the bytes parse as a JSON object or array (not a primitive).
  - `messagepack` = only if not json: a msgpack decode of the bytes that yields a map or array, rendered as tab-pretty JSON. Binary values render as `{"type":"Buffer","data":[..]}`.
  - The initial mode is json if available, else messagepack, else raw.
- **Controls (bottom bar):**
  - Button `Save Changes` (bottom-left), enabled only when changed.
  - Checkbox `Wrapping` (default on), hidden when the editor is narrower than 372px.
  - Mode select `Raw` / `JSON` / `MessagePack` (bottom-right). An unavailable mode is disabled.
- **Mode switching:** each mode keeps its own edit buffer, and saving writes the active mode's buffer.
- **Text area:** monospace, with line numbers in a `#f7f7f7` gutter. The find and jump-to-line addons are nice-to-have.
  - **Port:** an egui code editor with line numbers. JSON is validated on save.
- **Save (button, or Cmd+S even if unchanged):**
  - raw → the text as bytes.
  - json → it must parse to an object or array, else alert `The json is invalid. Please check again.`. Store minified JSON.
  - messagepack → the same JSON validation (same alert), then msgpack-encode and store the bytes.
  - An error → alert `Redis save failed: <msg>`.
  - On success, re-init the Editor from the saved bytes (re-detect the mode, `changed=false`).

## 5.8 Inline edit (ContentEditable)
- Used for key names, hash fields and zset scores.
- On start, focus and select all.
- Enter or blur commits the text; Escape reverts.
- The editing look: white background, `#333` text.

## Numbers
| Item | Value |
|---|---|
| Key SCAN COUNT | 100, then 5000/2000/1000 (SPEC-1) |
| Hash/Set scan COUNT | 500, then 10000 |
| List/ZSet batch | 201 first, then 1001 |
| Row height | 24 |
| Splitter min | 80 |
| Default widths | content 200, index 60, score 60 |
