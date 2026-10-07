//! Private, per-window crash recovery. A held file lock prevents one window
//! from offering or deleting another running window's recovery data.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};

use pf_core::{Document, io};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Info {
    path: Option<PathBuf>,
    revision: u64,
}

pub struct Candidate {
    pub dir: PathBuf,
    pub image: PathBuf,
    pub original: Option<PathBuf>,
    _lease: File,
}

impl Candidate {
    pub fn load(&self) -> Result<Document, String> {
        if !regular_file(&self.image) {
            return Err("Recovery image must be a regular file".to_owned());
        }
        let mut doc = io::open(&self.image).map_err(|e| e.to_string())?;
        doc.path = self.original.clone();
        doc.modified = true;
        Ok(doc)
    }
    pub fn discard(&self) {
        let _ = set_session_active(&self._lease, false);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Pending {
    rx: Receiver<Result<(), String>>,
    cancel: Arc<AtomicBool>,
    revision: u64,
}

pub struct Recovery {
    dir: PathBuf,
    _lease: Arc<File>,
    pending: Option<Pending>,
    saved_revision: Option<u64>,
    last_attempt: f64,
    discarded: bool,
    error: Option<String>,
    #[cfg(test)]
    writer_gate: Option<Arc<std::sync::Barrier>>,
}

fn lock_file(path: &Path, new: bool) -> std::io::Result<File> {
    if !new && !regular_file(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recovery lock must be a regular file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock().map_err(std::io::Error::other)?;
    Ok(file)
}

fn regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

// A persistent tombstone closes the gap between an in-flight writer's final
// rename and cancellation cleanup if the process crashes during that gap.
fn set_session_active(mut lease: &File, active: bool) -> std::io::Result<()> {
    lease.seek(SeekFrom::Start(0))?;
    lease.write_all(if active { b"A" } else { b"D" })?;
    lease.sync_all()
}

fn session_discarded(mut lease: &File) -> std::io::Result<bool> {
    lease.seek(SeekFrom::Start(0))?;
    let mut state = [0];
    Ok(lease.read(&mut state)? == 1 && state[0] == b'D')
}

fn read_original(path: &Path) -> Option<PathBuf> {
    if !regular_file(path) {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(65_537)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 65_536 {
        return None;
    }
    let original = serde_json::from_slice::<Info>(&bytes).ok()?.path?;
    // Old or tampered relative paths must never turn Save into a write under
    // the next process's working directory. Recover those documents untitled.
    (original.is_absolute() && io::is_ora(&original)).then_some(original)
}

impl Recovery {
    pub fn discover(root: &Path) -> Vec<Candidate> {
        let mut found = Vec::new();
        if !std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_dir()) {
            return found;
        }
        for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir = entry.path();
            let Ok(lease) = lock_file(&dir.join("session.lock"), false) else {
                continue;
            };
            if !matches!(session_discarded(&lease), Ok(false)) {
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
            let image = dir.join("recovery.ora");
            if !regular_file(&image) {
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
            let original = read_original(&dir.join("info.json"));
            found.push(Candidate {
                dir,
                image,
                original,
                _lease: lease,
            });
        }
        found.sort_by_key(|c| {
            std::cmp::Reverse(
                std::fs::metadata(&c.image)
                    .ok()
                    .and_then(|m| m.modified().ok()),
            )
        });
        found
    }

    pub fn new(root: &Path) -> std::io::Result<Self> {
        io::atomic::create_private_dir(root)?;
        let dir = loop {
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                pf_core::document::next_id()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let lease = Arc::new(lock_file(&dir.join("session.lock"), true)?);
        set_session_active(&lease, false)?;
        Ok(Self {
            dir,
            _lease: lease,
            pending: None,
            saved_revision: None,
            last_attempt: -30.0,
            discarded: true,
            error: None,
            #[cfg(test)]
            writer_gate: None,
        })
    }

    pub fn poll(&mut self) -> Option<String> {
        if let Some(error) = self.error.take() {
            return Some(error);
        }
        let pending = self.pending.as_ref()?;
        let result = match pending.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("Recovery writer stopped unexpectedly".to_owned())
            }
        };
        let pending = self.pending.take().unwrap();
        if !pending.cancel.load(Ordering::Acquire) && result.is_ok() {
            self.saved_revision = Some(pending.revision);
        }
        result.err()
    }

    pub fn tick(&mut self, doc: &Document, stable: bool, now: f64) {
        if !doc.modified {
            self.clear();
            return;
        }
        if !stable
            || self.pending.is_some()
            || self.saved_revision == Some(doc.revision())
            || now - self.last_attempt < 30.0
        {
            return;
        }
        self.last_attempt = now;
        if let Err(e) = set_session_active(&self._lease, true) {
            self.error = Some(format!("Could not prepare crash recovery: {e}"));
            return;
        }
        self.discarded = false;
        let path = doc
            .path
            .as_ref()
            .filter(|p| io::is_ora(p))
            .and_then(|p| std::path::absolute(p).ok());
        let (state, revision, dir) = (doc.state.clone(), doc.revision(), self.dir.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let lease = self._lease.clone();
        #[cfg(test)]
        let gate = self.writer_gate.take();
        std::thread::spawn(move || {
            // The worker may outlive its window. Keep the lock until its last
            // possible write/cleanup so another window cannot remove its data.
            let _lease = lease;
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.wait();
            }
            let result = (|| {
                if flag.load(Ordering::Acquire) {
                    return Ok(());
                }
                let metadata =
                    serde_json::to_vec(&Info { path, revision }).map_err(|e| e.to_string())?;
                io::atomic::write(&dir.join("info.json"), &metadata, true)
                    .map_err(|e| e.to_string())?;
                io::ora::save_private(&state, &dir.join("recovery.ora")).map_err(|e| e.to_string())
            })();
            if flag.load(Ordering::Acquire) {
                let _ = std::fs::remove_file(dir.join("recovery.ora"));
                let _ = std::fs::remove_file(dir.join("info.json"));
            }
            let _ = tx.send(result);
        });
        self.pending = Some(Pending {
            rx,
            cancel,
            revision,
        });
    }

    pub fn clear(&mut self) {
        if !self.discarded {
            match set_session_active(&self._lease, false) {
                Ok(()) => self.discarded = true,
                Err(e) => self.error = Some(format!("Could not clear crash recovery: {e}")),
            }
        }
        self.last_attempt = -30.0;
        if let Some(p) = &self.pending {
            p.cancel.store(true, Ordering::Release);
        }
        self.saved_revision = None;
        let _ = std::fs::remove_file(self.dir.join("recovery.ora"));
        let _ = std::fs::remove_file(self.dir.join("info.json"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recover_unsaved_document_but_never_a_live_window() {
        let root = std::env::temp_dir().join(format!(
            "pf-recovery-test-{}-{}",
            std::process::id(),
            pf_core::document::next_id()
        ));
        let mut recovery = Recovery::new(&root).unwrap();
        let mut doc = Document::new(8, 8, Some([255; 4]));
        doc.add_empty_layer();
        recovery.tick(&doc, true, 0.0);
        for _ in 0..200 {
            assert!(recovery.poll().is_none());
            if recovery.pending.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(recovery.pending.is_none());
        assert!(
            Recovery::discover(&root).is_empty(),
            "active session is locked"
        );
        drop(recovery); // Simulate crash: release lock but retain snapshot.
        let candidates = Recovery::discover(&root);
        assert_eq!(candidates.len(), 1);
        let restored = candidates[0].load().unwrap();
        assert_eq!(restored.state.layers.len(), 2);
        assert!(restored.modified);
        assert!(restored.path.is_none());
        candidates[0].discard();
        drop(candidates);
        assert!(Recovery::discover(&root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn successful_save_or_discard_removes_recovery() {
        let root = std::env::temp_dir().join(format!(
            "pf-recovery-clear-{}-{}",
            std::process::id(),
            pf_core::document::next_id()
        ));
        let mut recovery = Recovery::new(&root).unwrap();
        let mut doc = Document::new(8, 8, None);
        doc.add_empty_layer();
        recovery.tick(&doc, true, 0.0);
        recovery.clear(); // Also covers a save racing an in-flight snapshot.
        for _ in 0..200 {
            recovery.poll();
            if recovery.pending.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(recovery.pending.is_none());
        drop(recovery);
        assert!(Recovery::discover(&root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pf-recovery-{name}-{}-{}",
            std::process::id(),
            pf_core::document::next_id()
        ))
    }

    fn modified_doc() -> Document {
        let mut doc = Document::new(8, 8, Some([255; 4]));
        doc.add_empty_layer();
        doc
    }

    fn finish(recovery: &mut Recovery) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while recovery.pending.is_some() {
            assert!(recovery.poll().is_none());
            assert!(
                std::time::Instant::now() < end,
                "recovery writer did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn wait_candidates(root: &Path, expected: usize) -> Vec<Candidate> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let candidates = Recovery::discover(root);
            if candidates.len() == expected {
                return candidates;
            }
            assert!(
                std::time::Instant::now() < end,
                "recovery sessions did not unlock"
            );
            drop(candidates);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn unfinished_writer_keeps_session_locked_after_its_window_drops() {
        let root = test_root("writer-lock");
        let mut recovery = Recovery::new(&root).unwrap();
        let gate = Arc::new(std::sync::Barrier::new(2));
        recovery.writer_gate = Some(gate.clone());
        recovery.tick(&modified_doc(), true, 0.0);
        let dir = recovery.dir.clone();
        drop(recovery);
        assert!(Recovery::discover(&root).is_empty());
        assert!(
            dir.exists(),
            "discovery must not delete an unfinished writer's directory"
        );
        gate.wait();
        let candidates = wait_candidates(&root, 1);
        assert_eq!(candidates[0].load().unwrap().state.layers.len(), 2);
        candidates[0].discard();
        drop(candidates);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discarded_marker_rejects_snapshot_left_by_crash_after_late_rename() {
        let root = test_root("tombstone");
        let mut recovery = Recovery::new(&root).unwrap();
        let doc = modified_doc();
        recovery.tick(&doc, true, 0.0);
        finish(&mut recovery);
        recovery.clear();
        // Simulate a writer that committed after clear and then crashed before
        // its cancellation cleanup. The persistent marker must still dominate.
        io::ora::save_private(&doc.state, &recovery.dir.join("recovery.ora")).unwrap();
        drop(recovery);
        assert!(Recovery::discover(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovered_source_survives_a_second_crash_before_new_snapshot_finishes() {
        let root = test_root("second-crash");
        let mut first = Recovery::new(&root).unwrap();
        first.tick(&modified_doc(), true, 0.0);
        finish(&mut first);
        drop(first);
        let candidates = Recovery::discover(&root);
        let restored = candidates[0].load().unwrap();
        let mut second = Recovery::new(&root).unwrap();
        let gate = Arc::new(std::sync::Barrier::new(2));
        second.writer_gate = Some(gate.clone());
        second.tick(&restored, true, 0.0);
        drop(second);
        drop(candidates); // A crash drops the retained original; it must not delete it.
        let again = Recovery::discover(&root);
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].load().unwrap().state.layers.len(),
            restored.state.layers.len()
        );
        drop(again);
        gate.wait();
        let final_candidates = wait_candidates(&root, 2);
        for candidate in &final_candidates {
            candidate.discard();
        }
        drop(final_candidates);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_metadata_is_bounded_and_saved_paths_are_absolute() {
        let root = test_root("metadata");
        let mut recovery = Recovery::new(&root).unwrap();
        let mut doc = modified_doc();
        doc.path = Some(PathBuf::from("relative-project/document.ora"));
        recovery.tick(&doc, true, 0.0);
        finish(&mut recovery);
        let info = recovery.dir.join("info.json");
        drop(recovery);
        let candidates = Recovery::discover(&root);
        assert_eq!(
            candidates[0].original,
            Some(std::path::absolute(doc.path.unwrap()).unwrap())
        );
        drop(candidates);
        // Sparse fixture: physically tiny, advertised as a gigabyte. Discovery
        // reads at most 64KiB+1 and offers the image as an untitled recovery.
        let file = File::create(&info).unwrap();
        file.set_len(1 << 30).unwrap();
        drop(file);
        let candidates = Recovery::discover(&root);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].original.is_none());
        drop(candidates);
        std::fs::write(&info, br#"{"path":"relative.ora","revision":1}"#).unwrap();
        let candidates = Recovery::discover(&root);
        assert!(candidates[0].original.is_none());
        candidates[0].discard();
        drop(candidates);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_symlinked_session_children_without_touching_targets() {
        use std::os::unix::fs::symlink;
        let root = test_root("symlinks");
        for name in ["session.lock", "recovery.ora", "info.json"] {
            let mut recovery = Recovery::new(&root).unwrap();
            recovery.tick(&modified_doc(), true, 0.0);
            finish(&mut recovery);
            let target = recovery
                .dir
                .with_extension(format!("external-{}", name.replace('.', "-")));
            let child = recovery.dir.join(name);
            std::fs::rename(&child, &target).unwrap();
            symlink(&target, &child).unwrap();
            let original = std::fs::read(&target).unwrap();
            drop(recovery);
            let candidates = Recovery::discover(&root);
            if name == "info.json" {
                assert_eq!(candidates.len(), 1);
                assert!(candidates[0].original.is_none());
                candidates[0].discard();
            } else {
                assert!(candidates.is_empty());
            }
            drop(candidates);
            assert_eq!(std::fs::read(&target).unwrap(), original);
            std::fs::remove_file(target).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
