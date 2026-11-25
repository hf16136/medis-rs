//! Persistence: favorites, pattern store and pane sizes as JSON files under
//! `<config_dir>/medis/` (`favorites.json`, `patterns.json`, `sizes.json`).
//!
//! * Writes are atomic (temp file in the same directory, then rename).
//! * Loads never fail: a missing or corrupt file yields the default value.
//! * Every method takes `&self` / `&mut self` on a [`Storage`] rooted at an explicit directory so
//!   tests can use a temp dir; [`Storage::default_dir`] gives the platform location.
//! * To mirror the original "all windows share the store" behaviour, poll [`Storage::stamp`] and
//!   call the `load_*` functions again when it changes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::config::ConnectionConfig;

/// One saved key pattern (`name` and `value` both default to `*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pattern {
    /// Unique id (uuid).
    pub key: String,
    /// Display name.
    pub name: String,
    /// The glob pattern.
    pub value: String,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            key: uuid::Uuid::new_v4().simple().to_string(),
            name: "*".into(),
            value: "*".into(),
        }
    }
}

/// Patterns by `"<connectionKey>|<db>"` (see [`ConnectionConfig::pattern_key`]).
pub type PatternStore = BTreeMap<String, Vec<Pattern>>;

/// Persisted pane widths.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Sizes {
    /// Width of the table pane in the key-content split (default 200).
    pub content_bar_width: f32,
    /// Width of the list `index` column (default 60).
    pub index_bar_width: f32,
    /// Width of the zset `score` column (default 60).
    pub score_bar_width: f32,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            content_bar_width: 200.0,
            index_bar_width: 60.0,
            score_bar_width: 60.0,
        }
    }
}

/// Handle to the storage directory.
#[derive(Debug, Clone)]
pub struct Storage {
    dir: PathBuf,
}

const FAVORITES: &str = "favorites.json";
const PATTERNS: &str = "patterns.json";
const SIZES: &str = "sizes.json";
const HISTORY: &str = "terminal-history.json";

/// Most terminal commands kept per connection by [`Storage::save_history`].
pub const HISTORY_LIMIT: usize = 60;

impl Storage {
    /// Storage rooted at `dir` (created lazily on first save).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `dirs::config_dir()/medis` (falls back to `./medis` when no config dir exists).
    pub fn default_dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("medis")
    }

    /// Storage at [`Storage::default_dir`].
    pub fn open_default() -> Self {
        Self::new(Self::default_dir())
    }

    /// The directory used.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A cheap change stamp (sum of the modification times, in ms, of the three files).
    /// Poll it to detect writes by other windows/processes.
    pub fn stamp(&self) -> u128 {
        [FAVORITES, PATTERNS, SIZES]
            .iter()
            .filter_map(|f| std::fs::metadata(self.dir.join(f)).ok())
            .filter_map(|m| m.modified().ok())
            .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .sum()
    }

    fn load<T: DeserializeOwned + Default>(&self, file: &str) -> T {
        std::fs::read(self.dir.join(file))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save<T: Serialize>(&self, file: &str, value: &T) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
        write_atomic(&self.dir.join(file), &data)
    }

    // ---- favorites -------------------------------------------------------------------------

    /// Loads the ordered favorites list (empty when missing or corrupt).
    pub fn load_favorites(&self) -> Vec<ConnectionConfig> {
        self.load(FAVORITES)
    }

    /// Saves the whole favorites list.
    pub fn save_favorites(&self, favorites: &[ConnectionConfig]) -> std::io::Result<()> {
        self.save(FAVORITES, &favorites)
    }

    /// Appends a favorite. A fresh `favorite-<uuid>` key is assigned when `fav.key` is empty.
    /// Returns the stored key.
    pub fn create_favorite(&self, mut fav: ConnectionConfig) -> std::io::Result<String> {
        if fav.key.is_empty() {
            fav.key = format!("favorite-{}", uuid::Uuid::new_v4().simple());
        }
        let key = fav.key.clone();
        let mut all = self.load_favorites();
        all.push(fav);
        self.save_favorites(&all)?;
        Ok(key)
    }

    /// Removes the favorite with `key` (no-op when absent).
    pub fn remove_favorite(&self, key: &str) -> std::io::Result<()> {
        let mut all = self.load_favorites();
        all.retain(|f| f.key != key);
        self.save_favorites(&all)
    }

    /// Replaces the favorite whose key equals `fav.key` (the caller merges the overlay first).
    /// No-op when absent.
    pub fn update_favorite(&self, fav: &ConnectionConfig) -> std::io::Result<()> {
        let mut all = self.load_favorites();
        if let Some(slot) = all.iter_mut().find(|f| f.key == fav.key) {
            *slot = fav.clone();
            self.save_favorites(&all)?;
        }
        Ok(())
    }

    /// Moves the favorite at index `from` to index `to` (clamped).
    pub fn reorder_favorite(&self, from: usize, to: usize) -> std::io::Result<()> {
        let mut all = self.load_favorites();
        if from < all.len() {
            let item = all.remove(from);
            let to = to.min(all.len());
            all.insert(to, item);
            self.save_favorites(&all)?;
        }
        Ok(())
    }

    // ---- patterns --------------------------------------------------------------------------

    /// Loads the whole pattern store.
    pub fn load_patterns(&self) -> PatternStore {
        self.load(PATTERNS)
    }

    /// Saves the whole pattern store.
    pub fn save_patterns(&self, store: &PatternStore) -> std::io::Result<()> {
        self.save(PATTERNS, store)
    }

    /// Patterns for a `"<connectionKey>|<db>"` key.
    pub fn patterns_for(&self, pattern_key: &str) -> Vec<Pattern> {
        self.load_patterns().remove(pattern_key).unwrap_or_default()
    }

    /// Appends a default pattern (`*`/`*`) for `pattern_key`.
    pub fn create_pattern(&self, pattern_key: &str) -> std::io::Result<Pattern> {
        let mut store = self.load_patterns();
        let p = Pattern::default();
        store.entry(pattern_key.to_string()).or_default().push(p.clone());
        self.save_patterns(&store)?;
        Ok(p)
    }

    /// Removes the pattern at `index`.
    pub fn remove_pattern(&self, pattern_key: &str, index: usize) -> std::io::Result<()> {
        let mut store = self.load_patterns();
        if let Some(list) = store.get_mut(pattern_key)
            && index < list.len()
        {
            list.remove(index);
            self.save_patterns(&store)?;
        }
        Ok(())
    }

    /// Updates name/value of the pattern at `index` (each only when `Some`).
    pub fn update_pattern(
        &self,
        pattern_key: &str,
        index: usize,
        name: Option<&str>,
        value: Option<&str>,
    ) -> std::io::Result<()> {
        let mut store = self.load_patterns();
        if let Some(p) = store.get_mut(pattern_key).and_then(|l| l.get_mut(index)) {
            if let Some(n) = name {
                p.name = n.to_string();
            }
            if let Some(v) = value {
                p.value = v.to_string();
            }
            self.save_patterns(&store)?;
        }
        Ok(())
    }

    // ---- terminal history ------------------------------------------------------------------

    /// Terminal command history of one connection (`"<sshHost>|<host>|<port>"`), oldest first.
    /// Empty when missing or corrupt.
    pub fn load_history(&self, connection_key: &str) -> Vec<String> {
        let mut all: BTreeMap<String, Vec<String>> = self.load(HISTORY);
        all.remove(connection_key).unwrap_or_default()
    }

    /// Replaces the history of one connection, keeping only the newest [`HISTORY_LIMIT`] entries.
    /// Other connections' histories are left as they are.
    pub fn save_history(&self, connection_key: &str, history: &[String]) -> std::io::Result<()> {
        let mut all: BTreeMap<String, Vec<String>> = self.load(HISTORY);
        let skip = history.len().saturating_sub(HISTORY_LIMIT);
        all.insert(connection_key.to_string(), history[skip..].to_vec());
        self.save(HISTORY, &all)
    }

    // ---- sizes -----------------------------------------------------------------------------

    /// Loads the pane sizes (defaults when missing or corrupt).
    pub fn load_sizes(&self) -> Sizes {
        self.load(SIZES)
    }

    /// Saves the pane sizes.
    pub fn save_sizes(&self, sizes: &Sizes) -> std::io::Result<()> {
        self.save(SIZES, sizes)
    }
}

/// Writes `data` to `path` atomically: temp file next to it, fsync, rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Storage) {
        let d = tempfile::tempdir().unwrap();
        let s = Storage::new(d.path().join("medis"));
        (d, s)
    }

    #[test]
    fn missing_files_give_defaults() {
        let (_d, s) = store();
        assert!(s.load_favorites().is_empty());
        assert!(s.load_patterns().is_empty());
        assert_eq!(s.load_sizes(), Sizes::default());
        assert_eq!(s.load_sizes().content_bar_width, 200.0);
    }

    #[test]
    fn favorites_round_trip_and_ops() {
        let (_d, s) = store();
        let mut a = ConnectionConfig::new_favorite();
        a.name = "A".into();
        a.host = "h1".into();
        a.ssh_key = Some("KEY".into());
        let b = ConnectionConfig::new_favorite();
        let ka = s.create_favorite(a.clone()).unwrap();
        let kb = s.create_favorite(b).unwrap();
        let loaded = s.load_favorites();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], a);
        s.reorder_favorite(0, 1).unwrap();
        assert_eq!(s.load_favorites()[1].key, ka);
        let mut upd = s.load_favorites()[0].clone();
        upd.name = "renamed".into();
        s.update_favorite(&upd).unwrap();
        assert_eq!(s.load_favorites()[0].name, "renamed");
        s.remove_favorite(&kb).unwrap();
        assert_eq!(s.load_favorites().len(), 1);
    }

    #[test]
    fn corrupt_files_tolerated() {
        let (_d, s) = store();
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(s.dir().join(FAVORITES), b"{not json").unwrap();
        std::fs::write(s.dir().join(PATTERNS), b"[1,2").unwrap();
        std::fs::write(s.dir().join(SIZES), b"\xff\xfe").unwrap();
        assert!(s.load_favorites().is_empty());
        assert!(s.load_patterns().is_empty());
        assert_eq!(s.load_sizes(), Sizes::default());
        // and we can write over the corrupt file
        s.create_favorite(ConnectionConfig::new_favorite()).unwrap();
        assert_eq!(s.load_favorites().len(), 1);
    }

    #[test]
    fn patterns_and_sizes() {
        let (_d, s) = store();
        let k = "|localhost|6379|0";
        let p = s.create_pattern(k).unwrap();
        assert_eq!((p.name.as_str(), p.value.as_str()), ("*", "*"));
        s.update_pattern(k, 0, Some("users"), Some("user:*")).unwrap();
        let got = s.patterns_for(k);
        assert_eq!(got[0].name, "users");
        assert_eq!(got[0].value, "user:*");
        s.remove_pattern(k, 0).unwrap();
        assert!(s.patterns_for(k).is_empty());
        let sz = Sizes {
            content_bar_width: 321.0,
            ..Default::default()
        };
        s.save_sizes(&sz).unwrap();
        assert_eq!(s.load_sizes(), sz);
        let raw = std::fs::read_to_string(s.dir().join(SIZES)).unwrap();
        assert!(raw.contains("contentBarWidth"));
    }

    #[test]
    fn history_round_trip_per_connection_and_capped() {
        let (_d, s) = store();
        assert!(s.load_history("|h|6379").is_empty());
        s.save_history("|h|6379", &["PING".into(), "GET a".into()]).unwrap();
        s.save_history("|other|6379", &["INFO".into()]).unwrap();
        assert_eq!(s.load_history("|h|6379"), ["PING", "GET a"]);
        assert_eq!(s.load_history("|other|6379"), ["INFO"]);
        let many: Vec<String> = (0..HISTORY_LIMIT + 15).map(|i| format!("GET k{i}")).collect();
        s.save_history("|h|6379", &many).unwrap();
        let got = s.load_history("|h|6379");
        assert_eq!(got.len(), HISTORY_LIMIT);
        assert_eq!(got.first().map(String::as_str), Some("GET k15"));
        assert_eq!(got.last(), many.last());
        assert_eq!(s.load_history("|other|6379"), ["INFO"]);
    }

    #[test]
    fn no_temp_files_left() {
        let (_d, s) = store();
        s.save_sizes(&Sizes::default()).unwrap();
        s.save_sizes(&Sizes::default()).unwrap();
        let n = std::fs::read_dir(s.dir()).unwrap().count();
        assert_eq!(n, 1);
        assert!(s.stamp() > 0);
    }
}
