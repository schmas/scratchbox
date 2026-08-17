//! Note order, persisted as one filename per line.
//!
//! The manifest is a *hint*; the disk is the truth. A missing, corrupt, or hand-mangled
//! manifest costs the user their chosen ordering and nothing else — every note is still
//! there, and reconciliation rebuilds a sensible order from modification times.
//!
//! Plain text, one name per line, top line first. Greppable and editable in any editor,
//! which is the same reason the notes themselves are plain files.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::atomic;
use crate::error::Result;
use crate::naming;
use crate::note::{NoteId, NoteMeta};

const MANIFEST_FILE: &str = "order";

/// Merge the remembered order with what is actually on disk.
///
/// Pure: no filesystem access, so a hostile manifest line cannot reach a path here even in
/// principle. Every line is parsed through [`NoteId::new`] and anything that is not a plain
/// note name — `../../.ssh/id_rsa`, an absolute path, a NUL byte — is dropped before it can
/// be resolved against anything.
///
/// New files go on top, newest first. Everything the manifest knows about keeps its place.
///
/// `disk` is expected to hold each id at most once, which is what a directory listing gives.
/// A repeated id is returned repeatedly rather than merged — this function reorders what it is
/// given, it does not deduplicate it.
///
/// **A note whose name carries leading or trailing whitespace is not matched by its own
/// manifest line**: the line is trimmed here and the name is not. Such a note is read as new on
/// every call, so it loses its position, cannot be reordered, and makes this function
/// non-idempotent on its first application. See issue #19, which also covers a second ordering
/// defect: a repaired id is pushed at the *stale* line's position, so it can overtake a later
/// line that names it directly.
pub fn reconcile(manifest: &[String], disk: &[NoteMeta]) -> Vec<NoteId> {
    let mut unclaimed: Vec<&NoteMeta> = disk.iter().collect();
    let mut remembered = Vec::new();
    let mut seen = HashSet::new();

    for line in manifest {
        // Hand-edited files pick up whitespace, and sync brings in whatever another device
        // wrote. Both are untrusted input.
        let Ok(id) = NoteId::new(line.trim()) else {
            continue;
        };
        // A duplicated line names one note; the first occurrence is where it sits.
        //
        // `seen` is not what prevents a duplicate in the *output* — `unclaimed.remove` below is.
        // Removing this guard reds no property: a repeated line finds nothing left in
        // `unclaimed` and falls through to `renamed_to`, which returns `None` or claims a
        // different id. It skips redundant work on a repeated line and guards a future
        // refactor; it is not load-bearing today, and it is worth knowing which of the two is.
        if !seen.insert(id.clone()) {
            continue;
        }

        if let Some(index) = unclaimed.iter().position(|note| note.id == id) {
            unclaimed.remove(index);
            remembered.push(id);
        } else if let Some(index) = renamed_to(&id, &unclaimed) {
            remembered.push(unclaimed.remove(index).id.clone());
        }
        // Otherwise the note is genuinely gone, and leaving it out prunes the entry.
    }

    // Newest first, with the name as a tiebreak so two notes written in the same instant
    // do not swap places between runs.
    unclaimed.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));

    let mut order: Vec<NoteId> = unclaimed.into_iter().map(|note| note.id.clone()).collect();
    order.extend(remembered);
    order
}

/// The file a missing entry most likely turned into.
///
/// A note is renamed once, on its first save with content, and a crash between that rename
/// and the manifest write leaves the manifest naming a file that no longer exists while an
/// apparently new file sits on disk. Without this, that note is treated as new and jumps to
/// the top — a crash silently rewriting the order the user chose.
///
/// The correlation is the timestamp prefix, which the rename never changes. Two candidates
/// sharing a prefix are not guessed between: an ambiguous repair is worse than none.
fn renamed_to(missing: &NoteId, unclaimed: &[&NoteMeta]) -> Option<usize> {
    let wanted = rename_key(missing.as_str())?;

    let mut candidate = None;
    for (index, note) in unclaimed.iter().enumerate() {
        if rename_key(note.id.as_str()) == Some(wanted) {
            if candidate.is_some() {
                return None;
            }
            candidate = Some(index);
        }
    }
    candidate
}

/// The part of a name a rename leaves alone.
fn rename_key(name: &str) -> Option<(&str, Option<&str>)> {
    let (_, extension) = naming::split_extension(name);
    Some((naming::timestamp_prefix(name)?, extension))
}

/// Move the note at `index` one place towards the top. A no-op at the top.
pub fn move_up(order: &mut [NoteId], index: usize) {
    if index > 0 && index < order.len() {
        order.swap(index, index - 1);
    }
}

/// Move the note at `index` one place towards the bottom. A no-op at the bottom.
pub fn move_down(order: &mut [NoteId], index: usize) {
    if index + 1 < order.len() {
        order.swap(index, index + 1);
    }
}

/// Point an entry at a note's new name, in place.
///
/// In place, not remove-and-prepend: a note the user put at position four stays at position
/// four when it acquires its title.
pub fn rename_entry(order: &mut [NoteId], from: &NoteId, to: &NoteId) {
    for entry in order.iter_mut() {
        if entry == from {
            *entry = to.clone();
            return;
        }
    }
}

/// The `order` file inside the workspace's app directory.
#[derive(Debug, Clone)]
pub struct OrderStore {
    path: PathBuf,
}

impl OrderStore {
    /// `<app_dir>/order`, where `app_dir` is `<workspace>/.scratchbox`.
    pub fn new(app_dir: &Path) -> Self {
        Self {
            path: app_dir.join(MANIFEST_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The remembered order, as raw lines.
    ///
    /// Returns lines rather than a `Result` on purpose: a manifest that is missing, binary,
    /// or full of nonsense is a cache miss, not a failure. Losing the ordering is a small
    /// cost; an error dialog on startup because a hint file got mangled is a larger one.
    pub fn load(&self) -> Vec<String> {
        match fs::read_to_string(&self.path) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Persist the order, atomically so a crash never leaves half a manifest.
    pub fn save(&self, order: &[NoteId]) -> Result<()> {
        let lines: Vec<String> = order.iter().map(|id| id.as_str().to_owned()).collect();
        self.save_lines(&lines)
    }

    /// Follow a note to its new name, keeping its position.
    ///
    /// Edits the file line by line rather than rewriting it from a parsed order, so
    /// whatever else the user has in there survives a rename untouched.
    pub fn record_rename(&self, from: &NoteId, to: &NoteId) -> Result<()> {
        let mut lines = self.load();
        let Some(entry) = lines.iter_mut().find(|line| line.trim() == from.as_str()) else {
            return Ok(());
        };
        *entry = to.as_str().to_owned();
        self.save_lines(&lines)
    }

    /// Drop a note's entry. Writes nothing if it was not listed.
    pub fn record_removal(&self, id: &NoteId) -> Result<()> {
        let mut lines = self.load();
        let before = lines.len();
        lines.retain(|line| line.trim() != id.as_str());

        if lines.len() == before {
            return Ok(());
        }
        self.save_lines(&lines)
    }

    fn save_lines(&self, lines: &[String]) -> Result<()> {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        atomic::write_atomically(&self.path, text.as_bytes())
    }
}
