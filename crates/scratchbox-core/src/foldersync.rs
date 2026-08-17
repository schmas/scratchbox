//! [`Store`] over a plain directory of plain files.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use jiff::Zoned;

use crate::atomic::{self, TEMP_PREFIX};
use crate::config::APP_SUBDIR;
use crate::error::{Error, Result};
use crate::naming;
use crate::note::{Format, NoteId, NoteMeta};
use crate::order::OrderStore;
use crate::store::{Store, StoreEvent, WorkspaceHealth};
use crate::suppress::Suppressor;
use crate::watcher::{self, WatcherHandle};

/// Bound on collision-suffix probing, so a pathological directory cannot spin forever.
const MAX_NAME_ATTEMPTS: usize = 1000;

pub struct FolderSync {
    workspace: PathBuf,
    trash: PathBuf,
    order: OrderStore,
    events: Sender<StoreEvent>,
    inbox: Receiver<StoreEvent>,
    subscribed: AtomicBool,
    suppressor: Arc<Suppressor>,
    watcher: Option<WatcherHandle>,
}

impl FolderSync {
    /// Open a workspace, creating it, its app dir, and the trash dir if needed.
    pub fn new(workspace: PathBuf, trash: PathBuf) -> Result<Self> {
        for dir in [&workspace, &workspace.join(APP_SUBDIR), &trash] {
            fs::create_dir_all(dir).map_err(|source| Error::CreateDir {
                path: dir.clone(),
                source,
            })?;
        }

        let (events, inbox) = unbounded();
        Ok(Self {
            order: OrderStore::new(&workspace.join(APP_SUBDIR)),
            suppressor: Arc::new(Suppressor::new(workspace.clone())),
            workspace,
            trash,
            events,
            inbox,
            subscribed: AtomicBool::new(false),
            watcher: None,
        })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn trash(&self) -> &Path {
        &self.trash
    }

    /// The order manifest for this workspace.
    pub fn order(&self) -> &OrderStore {
        &self.order
    }

    /// The registry of writes this store is about to make. Exposed for tests.
    pub fn suppressor(&self) -> &Arc<Suppressor> {
        &self.suppressor
    }

    /// The sending half of the event channel, for feeding synthetic events in tests.
    pub fn events(&self) -> Sender<StoreEvent> {
        self.events.clone()
    }

    /// Empty the trash, returning how many entries were removed.
    ///
    /// There is no automatic purge — deleting things behind the user's back is a non-goal —
    /// so this is the only way trash is ever emptied. It reads nothing but the trash
    /// directory, and so cannot touch the workspace.
    pub fn purge_trash(&self) -> Result<usize> {
        let entries = fs::read_dir(&self.trash).map_err(Error::io("read", &self.trash))?;

        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(Error::io("read", &self.trash))?;
            let path = entry.path();
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            result.map_err(Error::io("remove", &path))?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Turn a note name into a path, refusing anything that resolves outside the workspace.
    ///
    /// [`NoteId`] already stops a *name* from escaping. This stops the *filesystem* from
    /// doing it: a symlink in the workspace is followed only when it lands on a file
    /// directly inside the workspace, and a dangling one is refused outright rather than
    /// created on write.
    fn resolve(&self, id: &NoteId) -> Result<PathBuf> {
        let path = self.workspace.join(id.as_str());

        let meta = match fs::symlink_metadata(&path) {
            // Not there yet: create() and write() legitimately target a path that does not
            // exist, and a genuinely missing note fails later with a real not-found error.
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(source) => return Err(Error::io("inspect", &path)(source)),
            Ok(meta) => meta,
        };

        if !meta.file_type().is_symlink() {
            return Ok(path);
        }

        let root = self
            .workspace
            .canonicalize()
            .map_err(Error::io("inspect", &self.workspace))?;
        let escaped = |resolved: PathBuf| Error::EscapesWorkspace {
            name: id.as_str().to_owned(),
            resolved,
        };

        match path.canonicalize() {
            Ok(real) if real.parent() == Some(root.as_path()) => Ok(path),
            Ok(real) => Err(escaped(real)),
            // A dangling symlink: writing through it would create the target it points at.
            Err(_) => Err(escaped(path)),
        }
    }

    /// First unused name in `dir`, suffixing `-2`, `-3`, … until one is free.
    fn free_name(dir: &Path, name: &str) -> Result<String> {
        for attempt in 1..=MAX_NAME_ATTEMPTS {
            let candidate = if attempt == 1 {
                name.to_owned()
            } else {
                naming::with_suffix(name, attempt)
            };
            if !dir.join(&candidate).exists() {
                return Ok(candidate);
            }
        }
        Err(Error::NoFreeName {
            name: name.to_owned(),
            tried: MAX_NAME_ATTEMPTS,
        })
    }

    /// Cross-device half of [`Store::delete`].
    ///
    /// Ordered so an interruption duplicates the note rather than destroying it: copy,
    /// flush to disk, move into place, and only then unlink the original. The reverse
    /// order is faster and can lose a note; this one's worst case is a stray temp file.
    fn copy_then_unlink(&self, source: &Path, target: &Path) -> Result<()> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let staging = self.trash.join(format!("{TEMP_PREFIX}{name}"));

        let result = (|| {
            let mut input = File::open(source).map_err(Error::io("read", source))?;
            let mut staged = atomic::create(&staging).map_err(Error::io("write", &staging))?;
            io::copy(&mut input, &mut staged).map_err(Error::io("copy", &staging))?;
            staged
                .sync_all()
                .map_err(Error::io("flush", &staging))
                .map(|()| drop(staged))
        })();

        if let Err(error) = result {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }

        fs::rename(&staging, target).map_err(Error::io("move to trash", &staging))?;
        // The note is safely in the trash; only now is losing the original acceptable.
        fs::remove_file(source).map_err(Error::io("remove", source))
    }
}

impl Store for FolderSync {
    /// Every regular, non-hidden file in the workspace root, unordered.
    ///
    /// Hidden files are excluded by [`NoteId`] refusing them, which sweeps up `.scratchbox`,
    /// cloud sidecars like `.DS_Store`, and our own in-flight temp files in one rule.
    /// Conflict copies such as `note (1).md` are not hidden and so appear as ordinary notes.
    fn list(&self) -> Result<Vec<NoteMeta>> {
        let entries = fs::read_dir(&self.workspace).map_err(Error::io("read", &self.workspace))?;

        let mut notes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(Error::io("read", &self.workspace))?;

            // A name that is not UTF-8, not a plain file name, or hidden is not a note.
            let Some(id) = entry.file_name().to_str().and_then(|n| NoteId::new(n).ok()) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }

            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            notes.push(NoteMeta::from_id(id, modified));
        }
        Ok(notes)
    }

    fn read(&self, id: &NoteId) -> Result<String> {
        let path = self.resolve(id)?;
        let bytes = fs::read(&path).map_err(Error::io("read", &path))?;
        String::from_utf8(bytes).map_err(|_| Error::NotUtf8 { path })
    }

    /// Write through a temp file in the same directory, then rename over the target.
    ///
    /// Same directory, not the system temp dir: the final rename has to stay on one
    /// filesystem to be atomic, and a workspace on a cloud mount is not on the same
    /// filesystem as `/tmp`.
    fn write(&self, id: &NoteId, content: &str) -> Result<()> {
        let target = self.resolve(id)?;

        // `?`, never `%`. `tracing-subscriber` ANSI-escapes exactly one field — "message" —
        // and writes every other with a bare `{:?}`. A `%` field becomes a `DisplayValue`
        // whose `Debug` delegates to `Display`, and `NoteId`'s `Display` is `write_str`, so
        // the name would reach the file byte for byte. `NoteId::new` rejects NUL, separators,
        // and a leading dot; it does *not* reject `\n` or `\x1b`. A note arriving by folder
        // sync named "a\n2026-…INFO…forwarded event=Removed(x.md).md" would otherwise forge
        // a whole log line, and an OSC/CSI sequence would execute in the terminal of whoever
        // reads the file or opens the CI artifact. `str`'s `Debug` escapes both.
        //
        // The byte count, never the content: a scratchpad's text does not belong in a log.
        tracing::debug!(id = ?id.as_str(), bytes = content.len(), "write");

        // Announced before the write rather than after: the watcher can deliver the event
        // before the write has even returned, and an unannounced echo reloads the note
        // over whatever the user has typed since.
        self.suppressor.register_write(id, content);

        atomic::write_atomically(&target, content.as_bytes())
    }

    fn create(&self, format: Format) -> Result<NoteId> {
        let base = naming::new_note_name(&Zoned::now(), format.extension());

        for attempt in 1..=MAX_NAME_ATTEMPTS {
            let name = if attempt == 1 {
                base.clone()
            } else {
                naming::with_suffix(&base, attempt)
            };
            let id = NoteId::new(&name)?;
            let path = self.workspace.join(&name);

            // `create_new` both tests and claims the name, so two notes made in the same
            // minute cannot race through a look-then-create gap onto one file.
            match atomic::create_new(&path) {
                Ok(_) => {
                    // `landed`, because the name that lands can differ from the base: two
                    // notes made in the same minute collide and the second gets a suffix.
                    tracing::debug!(landed = ?id.as_str(), attempt, "create");
                    return Ok(id);
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(Error::io("create", &path)(source)),
            }
        }

        Err(Error::NoFreeName {
            name: base,
            tried: MAX_NAME_ATTEMPTS,
        })
    }

    fn rename(&self, from: &NoteId, to: &NoteId) -> Result<NoteId> {
        if from == to {
            return Ok(to.clone());
        }

        let source = self.resolve(from)?;
        let name = Self::free_name(&self.workspace, to.as_str())?;
        let id = NoteId::new(&name)?;
        let target = self.workspace.join(&name);

        // `landed` separately from `to`: renaming is collision-aware, so a caller tracking
        // notes by name needs to see which of the two the disk actually got.
        tracing::debug!(
            from = ?from.as_str(),
            to = ?to.as_str(),
            landed = ?id.as_str(),
            "rename"
        );

        // The name that will actually land, so the echo is recognized whichever half of
        // the rename the platform reports.
        self.suppressor.register_rename(from, &id);

        fs::rename(&source, &target).map_err(Error::io("rename", &source))?;

        // The note has already moved, so a manifest that cannot be updated must not fail
        // the rename. Nothing is lost either way: reconciliation repairs a stale entry by
        // matching the timestamp prefix, which is precisely this situation.
        let _ = self.order.record_rename(from, &id);
        Ok(id)
    }

    fn delete(&self, id: &NoteId) -> Result<()> {
        let source = self.resolve(id)?;
        let name = Self::free_name(&self.trash, id.as_str())?;
        let target = self.trash.join(&name);

        // `landed` is the name inside the trash, which is suffixed when two same-named notes
        // are deleted into one trash directory. The trash path itself is not logged: it is
        // configurable and may name a directory the user would rather not see recorded.
        tracing::debug!(id = ?id.as_str(), landed = ?name, "delete");

        // Same reasoning as in `rename`: reconciliation prunes an entry whose file is
        // gone, so a failed manifest update cannot lose anything.
        let _ = self.order.record_removal(id);

        match fs::rename(&source, &target) {
            Ok(()) => Ok(()),
            // The trash lives under the user's data dir while the workspace may sit on a
            // cloud mount, so these are routinely different filesystems.
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                self.copy_then_unlink(&source, &target)
            }
            Err(source_error) => Err(Error::io("move to trash", &source)(source_error)),
        }
    }

    /// Opt-in rather than automatic: the CLI appends a line and exits, with no use for a
    /// watcher thread.
    fn start_watching(&mut self) -> Result<()> {
        if self.watcher.is_some() {
            return Ok(());
        }
        self.watcher = Some(watcher::spawn(
            &self.workspace,
            Arc::clone(&self.suppressor),
            self.events.clone(),
        )?);
        Ok(())
    }

    /// Is the workspace still there and writable?
    fn health(&self) -> WorkspaceHealth {
        match fs::metadata(&self.workspace) {
            Err(_) => WorkspaceHealth::Missing,
            Ok(meta) if !meta.is_dir() => WorkspaceHealth::Missing,
            Ok(meta) if meta.permissions().readonly() => WorkspaceHealth::ReadOnly,
            Ok(_) => WorkspaceHealth::Ok,
        }
    }

    fn subscribe(&self) -> Receiver<StoreEvent> {
        let _already_subscribed = self.subscribed.swap(true, Ordering::Relaxed);
        debug_assert!(
            !_already_subscribed,
            "Store::subscribe is single-subscriber: a second receiver steals events from the first"
        );
        self.inbox.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-device path is exercised directly: forcing a real `EXDEV` needs two
    /// filesystems, which no portable test can count on having.
    #[test]
    fn copy_then_unlink_moves_the_note_and_removes_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FolderSync::new(tmp.path().join("notes"), tmp.path().join("trash")).unwrap();

        let source = store.workspace().join("note.md");
        fs::write(&source, "secrets").unwrap();
        let target = store.trash().join("note.md");

        store.copy_then_unlink(&source, &target).unwrap();

        assert!(!source.exists(), "original should be gone");
        assert_eq!(fs::read_to_string(&target).unwrap(), "secrets");
    }

    /// The ordering guarantee: if the note never reaches the trash, the original survives.
    #[test]
    fn a_failed_copy_leaves_the_original_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FolderSync::new(tmp.path().join("notes"), tmp.path().join("trash")).unwrap();

        let source = store.workspace().join("note.md");
        fs::write(&source, "secrets").unwrap();

        // Staging cannot be created because a directory already occupies its name.
        let staging = store.trash().join(format!("{TEMP_PREFIX}note.md"));
        fs::create_dir(&staging).unwrap();

        let result = store.copy_then_unlink(&source, &store.trash().join("note.md"));

        assert!(result.is_err(), "copy should have failed");
        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            "secrets",
            "a failed delete must never destroy the note"
        );
    }
}
