//! What the app remembers between runs.
//!
//! ```text
//! ~/.config/pixelferrite/            ($XDG_CONFIG_HOME) hand-editable
//!   config.toml                      settings, including the AI key
//! ~/.local/share/pixelferrite/       ($XDG_DATA_HOME) written by the app
//!   recent.json                      recently opened files
//!   ai/<id>/                         one folder per AI request
//!     request.json                   prompt, model, status, timing
//!     input.png  mask.png  output.png
//! ```
//!
//! Settings are TOML because people edit them; the rest is JSON plus plain
//! PNGs so a request can be inspected with any file browser.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const RECENT_MAX: usize = 15;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ai: AiSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// OpenAI API key. A `.env` next to where the app is run takes precedence.
    pub api_key: String,
    /// Empty means the app's default model.
    pub model: String,
    /// "low", "medium", "high" or empty for the model's own choice.
    pub quality: String,
    /// Empty means OpenAI's own endpoint.
    pub base_url: String,
    /// How many past requests to keep on disk.
    pub keep_history: usize,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self { api_key: String::new(), model: String::new(), quality: String::new(), base_url: String::new(), keep_history: 200 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: PathBuf,
    /// UTC, ISO 8601.
    pub opened: String,
}

#[derive(Default, Serialize, Deserialize)]
struct RecentDoc {
    version: u32,
    files: Vec<RecentFile>,
}

/// `request.json`: everything about one AI request except the images.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiRecord {
    pub version: u32,
    pub id: String,
    /// UTC, ISO 8601.
    pub created: String,
    pub prompt: String,
    pub model: String,
    pub quality: String,
    /// Size of the image that was sent, in pixels.
    pub size: [u32; 2],
    /// File the layer came from, or empty for an unsaved image.
    pub document: String,
    pub layer: String,
    /// Area of the document that was sent: x0, y0, x1, y1.
    pub region: [i32; 4],
    /// "visible" (all visible layers, merged) or "layer" (that layer alone).
    pub source: String,
    /// Whether only a selection inside `region` was to be replaced.
    pub selection: bool,
    /// "running", "done", "error" or "cancelled".
    pub status: String,
    pub error: String,
    pub duration_ms: u64,
    /// Token counts as reported by the API, if any.
    pub usage: Option<serde_json::Value>,
}

impl Default for AiRecord {
    fn default() -> Self {
        Self {
            version: 1,
            id: String::new(),
            created: String::new(),
            prompt: String::new(),
            model: String::new(),
            quality: String::new(),
            size: [0, 0],
            document: String::new(),
            layer: String::new(),
            region: [0; 4],
            source: "layer".to_owned(),
            selection: false,
            status: "running".to_owned(),
            error: String::new(),
            duration_ms: 0,
            usage: None,
        }
    }
}

/// UTC time as `2026-10-06T14:03:22Z`.
pub fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Write via a temp file so a crash can't leave half a file behind.
fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = private;
    std::fs::rename(&tmp, path)
}

#[derive(Clone)]
pub struct Store {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Store {
    /// The standard locations. `PIXELFERRITE_CONFIG_DIR` / `PIXELFERRITE_DATA_DIR`
    /// override them outright.
    #[cfg_attr(test, allow(dead_code))]
    pub fn standard() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default());
        let dir = |own: &str, xdg: &str, fallback: &str| {
            let set = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
            set(own).or_else(|| set(xdg).map(|d| d.join("pixelferrite"))).unwrap_or_else(|| home.join(fallback).join("pixelferrite"))
        };
        Self {
            config_dir: dir("PIXELFERRITE_CONFIG_DIR", "XDG_CONFIG_HOME", ".config"),
            data_dir: dir("PIXELFERRITE_DATA_DIR", "XDG_DATA_HOME", ".local/share"),
        }
    }

    /// Everything under one folder; used by tests.
    #[cfg(test)]
    pub fn at(root: &Path) -> Self {
        Self { config_dir: root.join("config"), data_dir: root.join("data") }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    // ---- settings ----

    /// Missing or unreadable settings are just the defaults; the error (if
    /// the file exists but doesn't parse) is returned so it can be shown.
    pub fn load_settings(&self) -> (Settings, Option<String>) {
        match std::fs::read_to_string(self.config_file()) {
            Ok(text) => match toml::from_str(&text) {
                Ok(s) => (s, None),
                Err(e) => (Settings::default(), Some(format!("{}: {e}", self.config_file().display()))),
            },
            Err(_) => (Settings::default(), None),
        }
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        let body = toml::to_string_pretty(s).map_err(std::io::Error::other)?;
        let text = format!("# Pixelferrite settings. Safe to edit by hand while the app is closed.\n\n{body}");
        // Owner-only: this file can hold an API key.
        write_atomic(&self.config_file(), text.as_bytes(), true)
    }

    // ---- recent files ----

    fn recent_file(&self) -> PathBuf {
        self.data_dir.join("recent.json")
    }

    /// Most recent first.
    pub fn recent(&self) -> Vec<RecentFile> {
        std::fs::read_to_string(self.recent_file())
            .ok()
            .and_then(|t| serde_json::from_str::<RecentDoc>(&t).ok())
            .map(|d| d.files)
            .unwrap_or_default()
    }

    fn write_recent(&self, files: Vec<RecentFile>) {
        let doc = RecentDoc { version: 1, files };
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = write_atomic(&self.recent_file(), text.as_bytes(), false);
        }
    }

    pub fn add_recent(&self, path: &Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let mut files = self.recent();
        files.retain(|f| f.path != path);
        files.insert(0, RecentFile { path, opened: timestamp() });
        files.truncate(RECENT_MAX);
        self.write_recent(files);
    }

    pub fn remove_recent(&self, path: &Path) {
        let mut files = self.recent();
        files.retain(|f| f.path != path);
        self.write_recent(files);
    }

    pub fn clear_recent(&self) {
        self.write_recent(Vec::new());
    }

    // ---- AI request history ----

    pub fn ai_dir(&self) -> PathBuf {
        self.data_dir.join("ai")
    }

    pub fn ai_path(&self, id: &str) -> PathBuf {
        self.ai_dir().join(id)
    }

    /// A fresh, sortable id such as `20261006-140322-3f9a`.
    pub fn new_ai_id(&self) -> String {
        let t: String = timestamp().chars().filter(|c| c.is_ascii_digit()).collect();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let mut n = nanos ^ std::process::id().rotate_left(16);
        loop {
            let id = format!("{}-{}-{:04x}", &t[..8], &t[8..14], n & 0xffff);
            if !self.ai_path(&id).exists() {
                return id;
            }
            n = n.wrapping_add(1);
        }
    }

    pub fn write_ai_record(&self, r: &AiRecord) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(r).map_err(std::io::Error::other)?;
        write_atomic(&self.ai_path(&r.id).join("request.json"), text.as_bytes(), false)
    }

    pub fn write_ai_file(&self, id: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        write_atomic(&self.ai_path(id).join(name), bytes, false)
    }

    /// All recorded requests, newest first.
    pub fn ai_records(&self) -> Vec<AiRecord> {
        let mut out: Vec<AiRecord> = std::fs::read_dir(self.ai_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path().join("request.json")).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        out.sort_by(|a: &AiRecord, b| b.id.cmp(&a.id));
        out
    }

    pub fn delete_ai(&self, id: &str) {
        // Ids are generated here, but never let one walk out of the folder.
        if !id.is_empty() && !id.contains(['/', '\\', '.']) {
            let _ = std::fs::remove_dir_all(self.ai_path(id));
        }
    }

    /// Drop the oldest requests beyond `keep`.
    pub fn prune_ai(&self, keep: usize) {
        for r in self.ai_records().into_iter().skip(keep.max(1)) {
            self.delete_ai(&r.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_recent_and_ai_log() {
        let root = std::env::temp_dir().join(format!("pf-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(&root);

        // Settings: defaults when missing, round trip, partial files, owner-only.
        assert_eq!(store.load_settings(), (Settings::default(), None));
        let mut s = Settings::default();
        s.ai.api_key = "sk-test".into();
        s.ai.model = "gpt-image-2".into();
        store.save_settings(&s).unwrap();
        assert_eq!(store.load_settings().0, s);
        let text = std::fs::read_to_string(store.config_file()).unwrap();
        assert!(text.contains("[ai]") && text.contains("api_key = \"sk-test\""), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(store.config_file()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(store.config_file(), "[ai]\nquality = \"low\"\n").unwrap();
        let (partial, err) = store.load_settings();
        assert_eq!((partial.ai.quality.as_str(), partial.ai.keep_history, err), ("low", 200, None));
        std::fs::write(store.config_file(), "not toml [").unwrap();
        assert!(store.load_settings().1.is_some());

        // Recent files: newest first, no duplicates, capped.
        for i in 0..20 {
            store.add_recent(&root.join(format!("f{i}.ora")));
        }
        store.add_recent(&root.join("f3.ora"));
        let recent = store.recent();
        assert_eq!(recent.len(), RECENT_MAX);
        assert!(recent[0].path.ends_with("f3.ora") && recent[1].path.ends_with("f19.ora"));
        assert_eq!(recent.iter().filter(|f| f.path.ends_with("f3.ora")).count(), 1);
        store.remove_recent(&recent[0].path.clone());
        assert!(store.recent()[0].path.ends_with("f19.ora"));
        store.clear_recent();
        assert!(store.recent().is_empty());

        // AI log: records list newest first and prune from the old end.
        let ids: Vec<String> = (0..4)
            .map(|i| {
                let id = format!("20260101-00000{i}-aaaa");
                store.write_ai_record(&AiRecord { id: id.clone(), prompt: format!("p{i}"), ..Default::default() }).unwrap();
                store.write_ai_file(&id, "input.png", b"png").unwrap();
                id
            })
            .collect();
        let recs = store.ai_records();
        assert_eq!(recs.iter().map(|r| r.prompt.as_str()).collect::<Vec<_>>(), ["p3", "p2", "p1", "p0"]);
        assert_eq!(recs[0].status, "running");
        store.prune_ai(2);
        assert_eq!(store.ai_records().len(), 2);
        assert!(!store.ai_path(&ids[0]).exists() && store.ai_path(&ids[3]).join("input.png").exists());
        store.delete_ai("../config");
        assert!(store.config_dir.exists());
        assert_ne!(store.new_ai_id(), store.new_ai_id().replace('-', ""));

        let ts = timestamp();
        assert!(ts.len() == 20 && ts.starts_with("20") && ts.ends_with('Z'), "{ts}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
