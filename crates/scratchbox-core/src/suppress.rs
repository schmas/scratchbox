//! The registry that stops the app from reacting to its own writes.
//!
//! Every save we make comes back as a filesystem event a moment later. Reloading the note
//! from that echo would overwrite whatever the user typed in the meantime — the worst
//! failure this design has, because it loses keystrokes silently.
//!
//! So each mutation is announced here *before* it touches the disk, and the watcher drops
//! the matching event when it arrives.

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::note::NoteId;
use crate::store::StoreEvent;

/// How long a registration stays live: the debounce window plus slack for a slow disk.
pub const DEFAULT_TTL: Duration = Duration::from_secs(2);

/// How much of a note is hashed.
///
/// A whole note is normally far smaller than this, so a normal fingerprint covers every
/// byte. The cap is what keeps a huge note on a slow cloud mount from stalling the event
/// thread on a full read.
const FINGERPRINT_PREFIX: u64 = 64 * 1024;

/// Identity of a file's contents: its length plus a hash of its opening bytes.
///
/// Length is doing real work here rather than padding the hash — every way of editing past
/// the hashed prefix (appending, truncating, inserting) moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    prefix_hash: u64,
}

impl Fingerprint {
    fn of_bytes(bytes: &[u8]) -> Self {
        let cut = bytes.len().min(FINGERPRINT_PREFIX as usize);
        Self::new(bytes.len() as u64, &bytes[..cut])
    }

    fn of_file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();

        let mut prefix = Vec::new();
        file.take(FINGERPRINT_PREFIX).read_to_end(&mut prefix)?;

        Ok(Self::new(len, &prefix))
    }

    fn new(len: u64, prefix: &[u8]) -> Self {
        let mut hasher = DefaultHasher::new();
        prefix.hash(&mut hasher);
        Self {
            len,
            prefix_hash: hasher.finish(),
        }
    }
}

#[derive(Debug)]
enum PendingKind {
    Write(Fingerprint),
    /// Registered against both names: the two platforms report the halves of a rename
    /// differently, and the debouncer may deliver one stitched event or a remove plus a
    /// create.
    Rename {
        from: NoteId,
        to: NoteId,
    },
}

#[derive(Debug)]
struct Pending {
    id: NoteId,
    kind: PendingKind,
    expires_at: Instant,
}

/// Pending self-writes, consulted by the watcher before it forwards anything.
#[derive(Debug)]
pub struct Suppressor {
    workspace: PathBuf,
    ttl: Duration,
    pending: Mutex<Vec<Pending>>,
}

impl Suppressor {
    pub fn new(workspace: PathBuf) -> Self {
        Self::with_ttl(workspace, DEFAULT_TTL)
    }

    pub fn with_ttl(workspace: PathBuf, ttl: Duration) -> Self {
        Self {
            workspace,
            ttl,
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Announce a write before making it, fingerprinting the content from memory so
    /// registration costs no I/O.
    pub fn register_write(&self, id: &NoteId, content: &str) {
        // The byte count, never the content: this registry sees every note body in the
        // session, and a diagnostic log is not where a scratchpad's contents belong.
        tracing::trace!(
            id = ?id.as_str(),
            bytes = content.len(),
            ttl_ms = self.ttl.as_millis(),
            "registered a write"
        );
        self.push(Pending {
            id: id.clone(),
            kind: PendingKind::Write(Fingerprint::of_bytes(content.as_bytes())),
            expires_at: Instant::now() + self.ttl,
        });
    }

    /// Announce a rename before making it. Both names are registered.
    pub fn register_rename(&self, from: &NoteId, to: &NoteId) {
        // Both halves, because the platforms disagree about how many events a rename is and
        // reading that disagreement in the log is the point of recording it at all.
        tracing::trace!(
            from = ?from.as_str(),
            to = ?to.as_str(),
            ttl_ms = self.ttl.as_millis(),
            "registered a rename"
        );
        let kind = || PendingKind::Rename {
            from: from.clone(),
            to: to.clone(),
        };
        let expires_at = Instant::now() + self.ttl;

        self.push(Pending {
            id: from.clone(),
            kind: kind(),
            expires_at,
        });
        self.push(Pending {
            id: to.clone(),
            kind: kind(),
            expires_at,
        });
    }

    /// Should this event be dropped as an echo of our own work?
    ///
    /// **One snapshot for both checks, and no log write under the guard.** The decision is made
    /// inside a single `lock()` scope exactly as it was before instrumentation, then the guard is
    /// given back and only then is anything logged.
    ///
    /// Both halves of that matter. `matches` and `is_stale_write` each read a file under the
    /// guard already, so logging there would put a second I/O operation inside a mutex the
    /// watcher's translator thread needs to make progress. And splitting the two checks into
    /// separate lock scopes would leave a window for a concurrent `sweep` — which
    /// `Suppressor::len` performs, and which `the_registry_empties_itself_when_nothing_arrives`
    /// calls in a tight loop — to evict between them, turning a suppression into a forwarded
    /// echo. Neither is a trade worth making for a log line.
    pub fn should_suppress(&self, event: &StoreEvent) -> bool {
        self.sweep();

        let (stale, matched) = {
            let mut pending = self.lock();

            // A write whose file has become unreadable spends its entry without suppressing
            // anything: the content it stood for is gone, so the event describes a real change
            // the UI has to see.
            match pending
                .iter()
                .position(|entry| self.is_stale_write(entry, event))
            {
                Some(index) => {
                    pending.remove(index);
                    (true, false)
                }
                // Matching does not spend the entry; the TTL does.
                //
                // A registration is a statement about a state of the disk, not about one event:
                // "the file holding exactly this content is our doing", "these two names
                // changing places is our doing". Both stay true for as long as they are true,
                // and platforms report one logical change as several events — a rename on macOS
                // arrives as a removal of the old name and a creation of the new one, sometimes
                // with a stale create alongside. Spending the entry on the first would let the
                // rest through, which is the whole failure this registry exists to prevent.
                None => (false, pending.iter().any(|e| self.matches(e, event))),
            }
        };

        // Guard released. Only now is a write to the log safe.
        if stale {
            tracing::debug!(?event, matched = false, "spent a stale write entry");
            return false;
        }
        tracing::debug!(?event, matched, "suppression");
        matched
    }

    /// Live entries. Exposed so a test can prove the registry does not grow without bound.
    pub fn len(&self) -> usize {
        self.sweep();
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop expired entries. Cheap, and called on every event.
    ///
    /// Every eviction is announced at `warn`, and that level is deliberate. Slack between
    /// the TTL and the debounce window is 1.5s; synchronous log writes on the translator
    /// thread advance the clock while `expires_at` does not, and on a CI runner those writes
    /// are contended by sibling watcher tests. An entry evicted a moment early forwards an
    /// event the app made itself, which reds `expect_silence` with a message *identical to a
    /// genuine suppression-window race*. Making the eviction visible is the only thing that
    /// tells a reader of the log which of the two they are looking at.
    pub fn sweep(&self) {
        let now = Instant::now();

        // Collected under the guard, logged after giving it back.
        let mut evicted = Vec::new();
        {
            let mut pending = self.lock();
            pending.retain(|entry| {
                if entry.expires_at > now {
                    return true;
                }
                // Age since registration. `expires_at` is always `registered + ttl`, so this
                // is `(now - expires_at) + ttl` — done in `Duration` arithmetic, since
                // subtracting from an `Instant` can panic.
                evicted.push((
                    entry.id.clone(),
                    now.duration_since(entry.expires_at) + self.ttl,
                ));
                false
            });
        }

        for (id, age) in evicted {
            tracing::warn!(
                id = ?id.as_str(),
                age_ms = age.as_millis(),
                "registration evicted by TTL"
            );
        }
    }

    fn push(&self, entry: Pending) {
        self.lock().push(entry);
    }

    /// A poisoned lock must not take the watcher down with it: a panic elsewhere should
    /// cost at most a missed suppression, not every live update for the rest of the session.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn matches(&self, entry: &Pending, event: &StoreEvent) -> bool {
        match (&entry.kind, event) {
            // Matching on content, not just on path: a path-only match would swallow a
            // genuine external edit that lands inside the TTL window. Matching the
            // fingerprint cannot, because an entry only silences an event whose resulting
            // content is exactly what we wrote — which makes a false match a no-op, since
            // the file already holds what a reload would have fetched. Do not "simplify"
            // this to a path comparison.
            (PendingKind::Write(expected), StoreEvent::Modified(id) | StoreEvent::Created(id))
                if id == &entry.id =>
            {
                Fingerprint::of_file(&self.workspace.join(id.as_str()))
                    .is_ok_and(|actual| actual == *expected)
            }

            (PendingKind::Rename { from, to }, StoreEvent::Renamed { from: a, to: b }) => {
                (from == a && to == b) || (from == b && to == a)
            }
            // The same rename arriving as two halves rather than one stitched event.
            (PendingKind::Rename { from, .. }, StoreEvent::Removed(id)) => from == id,
            (PendingKind::Rename { to, .. }, StoreEvent::Created(id)) => to == id,

            _ => false,
        }
    }

    /// A write entry whose file can no longer be read.
    ///
    /// Failing open is deliberate: a spurious reload is cosmetic, a swallowed event is
    /// data the user never sees.
    fn is_stale_write(&self, entry: &Pending, event: &StoreEvent) -> bool {
        let (PendingKind::Write(_), StoreEvent::Modified(id) | StoreEvent::Created(id)) =
            (&entry.kind, event)
        else {
            return false;
        };
        id == &entry.id && Fingerprint::of_file(&self.workspace.join(id.as_str())).is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn id(name: &str) -> NoteId {
        NoteId::new(name).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, Suppressor) {
        let tmp = tempfile::tempdir().unwrap();
        let suppressor = Suppressor::new(tmp.path().to_path_buf());
        (tmp, suppressor)
    }

    /// One save can arrive as several events — macOS sends a create and a modify for a
    /// single atomic write — so every event describing the content we wrote is an echo.
    #[test]
    fn every_event_describing_our_own_content_is_suppressed() {
        let (tmp, suppressor) = fixture();
        fs::write(tmp.path().join("note.md"), "ours").unwrap();
        suppressor.register_write(&id("note.md"), "ours");

        assert!(suppressor.should_suppress(&StoreEvent::Created(id("note.md"))));
        assert!(suppressor.should_suppress(&StoreEvent::Modified(id("note.md"))));
        assert!(suppressor.should_suppress(&StoreEvent::Modified(id("note.md"))));
    }

    #[test]
    fn an_external_write_with_different_content_survives() {
        let (tmp, suppressor) = fixture();
        suppressor.register_write(&id("note.md"), "ours");
        // Somebody else got there first with different content.
        fs::write(tmp.path().join("note.md"), "theirs").unwrap();

        assert!(!suppressor.should_suppress(&StoreEvent::Modified(id("note.md"))));
    }

    #[test]
    fn a_write_to_a_file_that_vanished_is_not_suppressed() {
        let (_tmp, suppressor) = fixture();
        suppressor.register_write(&id("note.md"), "ours");

        // The file never appeared, or was deleted before the event arrived.
        assert!(!suppressor.should_suppress(&StoreEvent::Modified(id("note.md"))));
        assert!(suppressor.is_empty(), "the spent entry should be consumed");
    }

    #[test]
    fn a_rename_is_suppressed_whether_stitched_or_split() {
        let (_tmp, suppressor) = fixture();
        let (from, to) = (id("old.md"), id("new.md"));

        suppressor.register_rename(&from, &to);
        assert!(suppressor.should_suppress(&StoreEvent::Renamed {
            from: from.clone(),
            to: to.clone()
        }));

        suppressor.register_rename(&from, &to);
        assert!(suppressor.should_suppress(&StoreEvent::Removed(from.clone())));
        assert!(suppressor.should_suppress(&StoreEvent::Created(to.clone())));
    }

    #[test]
    fn a_large_note_is_fingerprinted_without_reading_all_of_it() {
        let (tmp, suppressor) = fixture();
        let body = "x".repeat(10 * 1024 * 1024);
        fs::write(tmp.path().join("big.md"), &body).unwrap();
        suppressor.register_write(&id("big.md"), &body);

        assert!(suppressor.should_suppress(&StoreEvent::Modified(id("big.md"))));
    }

    #[test]
    fn a_large_note_edited_past_the_hashed_prefix_still_survives() {
        let (tmp, suppressor) = fixture();
        let ours = "x".repeat(10 * 1024 * 1024);
        suppressor.register_write(&id("big.md"), &ours);
        // An external edit that changes the tail: the length moves, so the fingerprint does.
        fs::write(tmp.path().join("big.md"), "x".repeat(10 * 1024 * 1024 - 1)).unwrap();

        assert!(!suppressor.should_suppress(&StoreEvent::Modified(id("big.md"))));
    }

    #[test]
    fn overlapping_entries_for_one_path_are_both_honored() {
        let (tmp, suppressor) = fixture();
        // Phase 7 writes and then renames in one breath.
        fs::write(tmp.path().join("note.md"), "body").unwrap();
        suppressor.register_write(&id("note.md"), "body");
        suppressor.register_rename(&id("note.md"), &id("note-titled.md"));

        assert!(suppressor.should_suppress(&StoreEvent::Modified(id("note.md"))));
        assert!(suppressor.should_suppress(&StoreEvent::Renamed {
            from: id("note.md"),
            to: id("note-titled.md")
        }));
    }

    #[test]
    fn entries_expire_so_the_registry_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let suppressor = Suppressor::with_ttl(tmp.path().to_path_buf(), Duration::from_millis(20));

        for n in 0..50 {
            suppressor.register_write(&id(&format!("note-{n}.md")), "body");
        }
        assert_eq!(suppressor.len(), 50);

        std::thread::sleep(Duration::from_millis(60));

        assert!(
            suppressor.is_empty(),
            "entries outlived their TTL; the registry would grow with session length"
        );
    }

    #[test]
    fn a_rescan_is_never_suppressed() {
        let (_tmp, suppressor) = fixture();
        suppressor.register_write(&id("note.md"), "ours");

        assert!(!suppressor.should_suppress(&StoreEvent::Rescan));
    }
}
