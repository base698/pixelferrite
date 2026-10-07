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
use std::sync::{Arc, Mutex};
use std::collections::HashSet;
use pf_core::io::atomic;

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
    /// API key explicitly saved for `api_key_origin`.
    pub api_key: String,
    /// Empty means the app's default model.
    pub model: String,
    /// "low", "medium", "high" or empty for the model's own choice.
    pub quality: String,
    /// Empty means OpenAI's own endpoint.
    pub base_url: String,
    /// Origin approved when the key was saved; legacy empty values mean OpenAI.
    pub api_key_origin: String,
    /// How many past requests to keep on disk.
    pub keep_history: usize,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self { api_key: String::new(), model: String::new(), quality: String::new(), base_url: String::new(), api_key_origin: String::new(), keep_history: 200 }
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
    /// The full text sent to the model, when the app added to the prompt.
    pub sent_prompt: String,
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
    /// Exact clipped result.png origin, present in version 2 records.
    pub result_origin: Option<[i32; 2]>,
    pub canvas_size: [u32; 2],
    pub document_session: u64,
    pub source_layer: Option<u64>,
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
            version: 2,
            id: String::new(),
            created: String::new(),
            prompt: String::new(),
            sent_prompt: String::new(),
            model: String::new(),
            quality: String::new(),
            size: [0, 0],
            document: String::new(),
            layer: String::new(),
            region: [0; 4],
            source: "layer".to_owned(),
            selection: false,
            result_origin: None,
            canvas_size: [0; 2],
            document_session: 0,
            source_layer: None,
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

#[derive(Default)]
struct HistoryState {
    generation: u64,
    deleted: HashSet<String>,
    keep: Option<usize>,
}

/// A recording lease invalidated when history is cleared or its request deleted.
#[derive(Clone)]
pub struct HistoryToken {
    generation: u64,
    id: String,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[derive(Clone)]
pub struct Store {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    history: Arc<Mutex<HistoryState>>,
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
            history: Arc::default(),
        }
    }

    /// Everything under one folder; used by tests.
    #[cfg(test)]
    pub fn at(root: &Path) -> Self {
        Self { config_dir: root.join("config"), data_dir: root.join("data"), history: Arc::default() }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    // ---- settings ----

    /// Missing or unreadable settings are just the defaults; the error (if
    /// the file exists but doesn't parse) is returned so it can be shown.
    pub fn load_settings(&self) -> (Settings, Option<String>) {
        // Restrict older installations before exposing their stored credentials or images.
        for dir in [&self.config_dir, &self.data_dir] {
            if std::fs::symlink_metadata(dir).is_ok() {
                if let Err(e) = atomic::create_private_dir(dir) {
                    return (Settings::default(), Some(format!("Could not protect {}: {e}", dir.display())));
                }
            }
        }
        match std::fs::read_to_string(self.config_file()) {
            Ok(text) => match toml::from_str(&text) {
                Ok(s) => (s, None),
                Err(e) => (Settings::default(), Some(format!("{}: {e}", self.config_file().display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
            Err(e) => (Settings::default(), Some(format!("Could not read {}: {e}", self.config_file().display()))),
        }
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        let body = toml::to_string_pretty(s).map_err(std::io::Error::other)?;
        let text = format!("# Pixelferrite settings. Safe to edit by hand while the app is closed.\n\n{body}");
        // Owner-only: this file can hold an API key.
        atomic::create_private_dir(&self.config_dir)?;
        atomic::write(&self.config_file(), text.as_bytes(), true)
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
            if atomic::create_private_dir(&self.data_dir).is_ok() {
                let _ = atomic::write(&self.recent_file(), text.as_bytes(), true);
            }
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

    fn check_ai_root(&self) -> std::io::Result<()> {
        for dir in [&self.data_dir, &self.ai_dir()] {
            match std::fs::symlink_metadata(dir) {
                Ok(meta) if !meta.is_dir() => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "AI history directories must not be symbolic links")),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// A regular history image under the owned request directory; never follow links.
    pub fn ai_file(&self, id: &str, name: &str) -> Option<PathBuf> {
        if !valid_id(id) || !matches!(name, "input.png" | "mask.png" | "output.png" | "result.png") { return None; }
        self.check_ai_root().ok()?;
        let dir = self.ai_path(id);
        if !std::fs::symlink_metadata(&dir).ok()?.is_dir() { return None; }
        let path = dir.join(name);
        std::fs::symlink_metadata(&path).ok()?.is_file().then_some(path)
    }

    /// A fresh, sortable id, distinct across processes and requests.
    pub fn new_ai_id(&self) -> String {
        let t: String = timestamp().chars().filter(|c| c.is_ascii_digit()).collect();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        loop {
            let id = format!("{}-{}-{:x}-{nanos:08x}-{:x}", &t[..8], &t[8..14], std::process::id(), pf_core::document::next_id());
            if !self.ai_path(&id).exists() { return id; }
        }
    }

    fn prepare_ai_dir(&self, id: &str) -> std::io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid AI request id"));
        }
        atomic::create_private_dir(&self.data_dir)?;
        atomic::create_private_dir(&self.ai_dir())?;
        let dir = self.ai_path(id);
        atomic::create_private_dir(&dir)?;
        Ok(dir)
    }

    pub fn write_ai_record(&self, r: &AiRecord) -> std::io::Result<()> {
        let dir = self.prepare_ai_dir(&r.id)?;
        let text = serde_json::to_string_pretty(r).map_err(std::io::Error::other)?;
        atomic::write(&dir.join("request.json"), text.as_bytes(), true)
    }

    pub fn write_ai_file(&self, id: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        if !matches!(name, "input.png" | "mask.png" | "output.png" | "result.png") {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid AI history filename"));
        }
        atomic::write(&self.prepare_ai_dir(id)?.join(name), bytes, true)
    }

    pub fn begin_ai_record(&self, r: &AiRecord, keep: usize) -> std::io::Result<Option<HistoryToken>> {
        if keep == 0 { return Ok(None); }
        let mut state = self.history.lock().unwrap_or_else(|e| e.into_inner());
        state.keep = Some(keep);
        self.write_ai_record(r)?;
        Ok(Some(HistoryToken { generation: state.generation, id: r.id.clone() }))
    }

    /// The lock makes clear/delete and in-flight recording mutually exclusive.
    pub fn record_ai(&self, token: Option<&HistoryToken>, save: impl FnOnce(&Self) -> std::io::Result<()>) -> std::io::Result<()> {
        let Some(token) = token else { return Ok(()) };
        let state = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if token.generation != state.generation || state.deleted.contains(&token.id) { return Ok(()); }
        save(self)
    }

    /// All recorded requests, newest first. Never trust ids stored inside JSON.
    pub fn ai_records(&self) -> Vec<AiRecord> {
        if self.check_ai_root().is_err() { return Vec::new(); }
        let mut out: Vec<AiRecord> = std::fs::read_dir(self.ai_dir())
            .into_iter().flatten().flatten()
            .filter_map(|e| {
                if !e.file_type().ok()?.is_dir() { return None; }
                let id = e.file_name().into_string().ok()?;
                if !valid_id(&id) { return None; }
                let file = e.path().join("request.json");
                let meta = std::fs::symlink_metadata(&file).ok()?;
                if !meta.is_file() || meta.len() > 1 << 20 { return None; }
                let r: AiRecord = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
                (r.id == id).then_some(r)
            }).collect();
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    }

    pub fn delete_ai(&self, id: &str) -> std::io::Result<()> {
        self.check_ai_root()?;
        if !valid_id(id) { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid AI request id")); }
        let mut state = self.history.lock().unwrap_or_else(|e| e.into_inner());
        state.deleted.insert(id.to_owned());
        match std::fs::remove_dir_all(self.ai_path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn clear_ai(&self) -> std::io::Result<()> {
        self.check_ai_root()?;
        let mut state = self.history.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.deleted.clear();
        match std::fs::remove_dir_all(self.ai_dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Apply the current retention preference, including to in-flight recordings.
    pub fn prune_ai(&self, keep: usize) -> std::io::Result<()> {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).keep = Some(keep);
        self.prune_ai_current()
    }

    pub fn prune_ai_current(&self) -> std::io::Result<()> {
        let keep = self.history.lock().unwrap_or_else(|e| e.into_inner()).keep.unwrap_or(200);
        if keep == 0 { return self.clear_ai(); }
        for r in self.ai_records().into_iter().skip(keep) { self.delete_ai(&r.id)?; }
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_store() -> Store {
        Store::at(&std::env::temp_dir().join(format!("pf-history-{}-{}", std::process::id(), pf_core::document::next_id())))
    }

    #[test]
    fn history_off_clear_and_delete_do_not_resurrect_requests() {
        let store = private_store();
        let record = AiRecord { id: "fixture-request".into(), ..Default::default() };
        assert!(store.begin_ai_record(&record, 0).unwrap().is_none());
        store.record_ai(None, |_| panic!("disabled history must never write")).unwrap();
        assert!(!store.ai_dir().exists());
        let token = store.begin_ai_record(&record, 2).unwrap().unwrap();
        store.clear_ai().unwrap();
        store.record_ai(Some(&token), |_| panic!("cleared recording must not resume")).unwrap();
        assert!(!store.ai_dir().exists());
        let token = store.begin_ai_record(&record, 2).unwrap().unwrap();
        store.delete_ai(&record.id).unwrap();
        store.record_ai(Some(&token), |_| panic!("deleted request must not resume")).unwrap();
        assert!(!store.ai_path(&record.id).exists());
        store.write_ai_record(&record).unwrap();
        store.prune_ai(0).unwrap();
        assert!(!store.ai_dir().exists());
    }

    #[test]
    fn history_rejects_paths_and_mismatched_record_ids() {
        let store = private_store();
        let mut record = AiRecord { id: "fixture-request".into(), ..Default::default() };
        store.write_ai_record(&record).unwrap();
        record.id = "../outside".into();
        assert!(store.write_ai_record(&record).is_err());
        assert!(store.write_ai_file("fixture-request", "../../outside", b"x").is_err());
        atomic::write(&store.ai_path("fixture-request").join("request.json"), &serde_json::to_vec(&record).unwrap(), true).unwrap();
        assert!(store.ai_records().is_empty(), "record ids must agree with their directory");
    }

    #[cfg(unix)]
    #[test]
    fn config_and_all_history_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let store = private_store();
        store.save_settings(&Settings::default()).unwrap();
        let r = AiRecord { id: "permissions".into(), ..Default::default() };
        store.write_ai_record(&r).unwrap();
        for name in ["input.png", "mask.png", "output.png", "result.png"] {
            store.write_ai_file(&r.id, name, b"private fixture").unwrap();
        }
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for dir in [&store.config_dir, &store.data_dir, &store.ai_dir(), &store.ai_path(&r.id)] { assert_eq!(mode(dir), 0o700); }
        assert_eq!(mode(&store.config_file()), 0o600);
        for name in ["request.json", "input.png", "mask.png", "output.png", "result.png"] { assert_eq!(mode(&store.ai_path(&r.id).join(name)), 0o600); }
    }

    #[cfg(unix)]
    #[test]
    fn history_links_cannot_read_or_delete_outside_owned_storage() {
        use std::os::unix::fs::symlink;
        let store = private_store();
        let outside = store.config_dir.join("outside");
        std::fs::create_dir_all(outside.join("ai")).unwrap();
        std::fs::write(outside.join("ai/valuable"), b"keep me").unwrap();
        symlink(&outside, &store.data_dir).unwrap();
        assert!(store.clear_ai().is_err());
        assert!(store.delete_ai("valuable").is_err());
        assert!(outside.join("ai/valuable").exists());
        assert!(store.ai_records().is_empty());
        std::fs::remove_file(&store.data_dir).unwrap();
        let r = AiRecord { id: "link-fixture".into(), ..Default::default() };
        store.write_ai_record(&r).unwrap();
        symlink(outside.join("ai/valuable"), store.ai_path(&r.id).join("output.png")).unwrap();
        assert!(store.ai_file(&r.id, "output.png").is_none());
    }

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
        store.prune_ai(2).unwrap();
        assert_eq!(store.ai_records().len(), 2);
        assert!(!store.ai_path(&ids[0]).exists() && store.ai_path(&ids[3]).join("input.png").exists());
        assert!(store.delete_ai("../config").is_err());
        assert!(store.config_dir.exists());
        assert_ne!(store.new_ai_id(), store.new_ai_id().replace('-', ""));

        let ts = timestamp();
        assert!(ts.len() == 20 && ts.starts_with("20") && ts.ends_with('Z'), "{ts}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
