//! Filesystem watching, debounced and translated into [`StoreEvent`].

use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use crate::error::{Error, Result};
use crate::note::NoteId;
use crate::store::StoreEvent;
use crate::suppress::Suppressor;

/// How long changes are collected before being reported.
///
/// Long enough that a burst of writes lands as one event, short enough that a change made
/// in another window shows up while the user is still looking at it.
///
/// Known consequence: a file created and deleted inside one window can cancel out and be
/// reported not at all. In practice that needs an external deletion within half a second of
/// one of our own saves, and it leaves a stale row in the list until the next event. The
/// alternative is polling, which this design rejects outright.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

/// Owns the watcher thread. Dropping this stops watching.
pub struct WatcherHandle {
    debouncer: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
    translator: Option<JoinHandle<()>>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        // Dropping the debouncer closes the channel feeding the translator, which is what
        // tells it to finish. Order matters: joining first would wait forever.
        self.debouncer.take();
        if let Some(translator) = self.translator.take() {
            let _ = translator.join();
        }
    }
}

/// Start watching `workspace`, forwarding everything that is not our own doing to `out`.
pub fn spawn(
    workspace: &Path,
    suppressor: Arc<Suppressor>,
    out: Sender<StoreEvent>,
) -> Result<WatcherHandle> {
    // FSEvents reports canonical paths — `/private/var/...` where the workspace was given
    // as `/var/...`. Without canonicalizing here, every event on macOS looks like it came
    // from outside the workspace and is discarded.
    let root = workspace
        .canonicalize()
        .map_err(Error::io("inspect", workspace))?;

    let (tx, rx) = unbounded::<DebounceEventResult>();
    let mut debouncer = new_debouncer(DEBOUNCE_WINDOW, None, tx)?;
    debouncer.watch(&root, RecursiveMode::Recursive)?;

    let translator = thread::spawn(move || {
        let mut recent = RecentlyEmitted::default();

        for batch in rx {
            let events = match batch {
                Ok(events) => events,
                // The backend lost track — an overflow, or an error reading the queue.
                // Telling the caller to re-list is the honest response.
                Err(_) => {
                    tracing::warn!("backend lost track of the workspace; asking for a rescan");
                    if out.send(StoreEvent::Rescan).is_err() {
                        return;
                    }
                    continue;
                }
            };

            // One logical change can surface as several raw events: macOS reports a single
            // delete as both a metadata and a data event. A batch is the debouncer's own
            // unit of coalescing, so collapsing duplicates within it reports the change
            // once without merging changes that genuinely happened at different times.
            let raw = events.len();
            let mut batch = Vec::new();
            for debounced in events {
                // `?`, never `%`. `Debug` on a `Path` quotes and escapes; `Display` would
                // put a synced note's name into the file byte for byte, newlines and
                // terminal escapes included. See the note on the suppression event.
                tracing::trace!(
                    kind = ?debounced.event.kind,
                    paths = ?debounced.event.paths,
                    "raw event"
                );
                for event in translate(&debounced.event, &root) {
                    if !batch.contains(&event) {
                        batch.push(event);
                    }
                }
            }
            tracing::debug!(raw, collapsed = batch.len(), "batch translated");

            for event in batch {
                if suppressor.should_suppress(&event) {
                    continue;
                }
                // macOS notifies about the metadata and the data of one change separately,
                // and the two land in different batches. Reporting the same change twice
                // costs a redundant reload, so identical events inside one debounce window
                // collapse. This adds no delay — it is deduplication, not a second layer
                // of debouncing, which would make the UI feel sluggish for nothing.
                if recent.already_reported(&event) {
                    tracing::trace!(?event, "dropped as already reported");
                    continue;
                }
                tracing::debug!(?event, "forwarded");
                // The receiver is gone; nothing left to report to.
                if out.send(event).is_err() {
                    return;
                }
            }
        }
    });

    Ok(WatcherHandle {
        debouncer: Some(debouncer),
        translator: Some(translator),
    })
}

/// Events already sent, so a change the platform reports twice is not reported twice.
///
/// Bounded by the debounce window: anything older has been forgotten, so a note edited
/// again in a later window is reported again, as it should be.
#[derive(Default)]
struct RecentlyEmitted(Vec<(StoreEvent, std::time::Instant)>);

impl RecentlyEmitted {
    fn already_reported(&mut self, event: &StoreEvent) -> bool {
        let now = std::time::Instant::now();
        self.0
            .retain(|(_, sent_at)| now.duration_since(*sent_at) < DEBOUNCE_WINDOW);

        if self.0.iter().any(|(seen, _)| seen == event) {
            return true;
        }
        self.0.push((event.clone(), now));
        false
    }
}

/// Map one filesystem event onto store events, dropping anything that is not a note.
///
/// The reported kind is treated as a hint and checked against the disk. macOS is the
/// reason: FSEvents reports a deleted file as `Modify(Data(Content))`, so trusting the
/// kind leaves a note in the list that is no longer there. The path is the part every
/// platform gets right.
fn translate(event: &notify::Event, root: &Path) -> Vec<StoreEvent> {
    classify(event, root)
        .into_iter()
        .map(|event| match event {
            StoreEvent::Created(id) | StoreEvent::Modified(id)
                if !root.join(id.as_str()).exists() =>
            {
                StoreEvent::Removed(id)
            }
            other => other,
        })
        .collect()
}

/// The event as the platform described it, before checking that description.
fn classify(event: &notify::Event, root: &Path) -> Vec<StoreEvent> {
    let note = |index: usize| {
        event
            .paths
            .get(index)
            .and_then(|path| note_in_root(path, root))
    };

    match event.kind {
        // Opening and reading a file changes nothing.
        EventKind::Access(_) => Vec::new(),

        EventKind::Create(_) => note(0).map(StoreEvent::Created).into_iter().collect(),
        EventKind::Remove(_) => note(0).map(StoreEvent::Removed).into_iter().collect(),

        // Both halves correlated, which is what the debouncer's file-id tracking is for.
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            match (note(0), note(1)) {
                (Some(from), Some(to)) => vec![StoreEvent::Renamed { from, to }],
                // An editor saving through a temp file: the source is not a note, so what
                // the user sees is the destination note changing.
                (None, Some(to)) => vec![StoreEvent::Modified(to)],
                (Some(from), None) => vec![StoreEvent::Removed(from)],
                (None, None) => Vec::new(),
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            note(0).map(StoreEvent::Removed).into_iter().collect()
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            note(0).map(StoreEvent::Created).into_iter().collect()
        }
        // An uncorrelated rename — which half this is gets settled by the existence check
        // in `translate`, not guessed at here.
        EventKind::Modify(ModifyKind::Name(_)) => {
            note(0).map(StoreEvent::Created).into_iter().collect()
        }

        EventKind::Modify(_) => note(0).map(StoreEvent::Modified).into_iter().collect(),

        // Deliberately not guessed. `Any` is the backend saying it does not know what
        // happened, and re-listing is cheaper than being wrong.
        EventKind::Any | EventKind::Other => vec![StoreEvent::Rescan],
    }
}

/// The note this path refers to, if it is one.
///
/// Everything else falls out here: the workspace is flat, so anything nested — including
/// `.scratchbox/order` — has the wrong parent, and [`NoteId`] rejects hidden names, which
/// covers cloud sidecars and our own in-flight temp files.
fn note_in_root(path: &Path, root: &Path) -> Option<NoteId> {
    if path.parent() != Some(root) {
        return None;
    }
    NoteId::new(path.file_name()?.to_str()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, RemoveKind};
    use std::fs;
    use std::path::PathBuf;

    /// A raw event as a backend would hand one over.
    fn raw(kind: EventKind, paths: &[PathBuf]) -> notify::Event {
        paths.iter().fold(notify::Event::new(kind), |event, path| {
            event.add_path(path.clone())
        })
    }

    /// Canonicalized, because that is what `spawn` hands to `translate` and what every path
    /// comparison below depends on: on macOS a temp dir under `/var` resolves to
    /// `/private/var`, and an uncanonicalized root would make every event look external.
    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    fn id(name: &str) -> NoteId {
        NoteId::new(name).unwrap()
    }

    #[test]
    fn only_a_plain_file_directly_in_the_root_is_a_note() {
        let (_tmp, root) = root();

        assert_eq!(
            note_in_root(&root.join("note.md"), &root),
            Some(id("note.md"))
        );

        // The workspace is flat, so anything nested has the wrong parent — which is what
        // keeps `.scratchbox/order` from ever being read as a note.
        assert_eq!(note_in_root(&root.join(".scratchbox/order"), &root), None);
        assert_eq!(note_in_root(&root.join("sub/note.md"), &root), None);

        // Hidden names cover cloud sidecars and our own in-flight temp files in one rule.
        assert_eq!(note_in_root(&root.join(".DS_Store"), &root), None);
        assert_eq!(note_in_root(&root.join(".tmp-note.md"), &root), None);

        assert_eq!(note_in_root(Path::new("/elsewhere/note.md"), &root), None);
    }

    /// The macOS rule the module doc states: FSEvents reports a deleted file as a content
    /// change, so the reported kind is a hint and the disk is the authority.
    #[test]
    fn a_change_to_a_file_that_is_not_there_is_reported_as_a_removal() {
        let (_tmp, root) = root();
        let gone = root.join("gone.md");

        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
        ] {
            assert_eq!(
                translate(&raw(kind, std::slice::from_ref(&gone)), &root),
                vec![StoreEvent::Removed(id("gone.md"))],
                "a {kind:?} for a missing file should read as a removal"
            );
        }
    }

    #[test]
    fn a_creation_of_a_file_that_is_there_stays_a_creation() {
        let (_tmp, root) = root();
        fs::write(root.join("note.md"), "body").unwrap();

        assert_eq!(
            translate(
                &raw(EventKind::Create(CreateKind::File), &[root.join("note.md")]),
                &root
            ),
            vec![StoreEvent::Created(id("note.md"))]
        );
    }

    /// An editor saving through a temp file: the source is not a note, so what the user sees
    /// is the destination note changing rather than a note appearing from nowhere.
    #[test]
    fn an_editor_saving_through_a_temp_file_reads_as_the_note_changing() {
        let (_tmp, root) = root();
        fs::write(root.join("note.md"), "body").unwrap();

        let stitched = raw(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[root.join(".tmp-note.md"), root.join("note.md")],
        );

        assert_eq!(
            translate(&stitched, &root),
            vec![StoreEvent::Modified(id("note.md"))]
        );
    }

    #[test]
    fn a_real_rename_between_two_notes_keeps_both_halves() {
        let (_tmp, root) = root();
        fs::write(root.join("new.md"), "body").unwrap();

        let stitched = raw(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[root.join("old.md"), root.join("new.md")],
        );

        assert_eq!(
            translate(&stitched, &root),
            vec![StoreEvent::Renamed {
                from: id("old.md"),
                to: id("new.md")
            }]
        );
    }

    #[test]
    fn an_event_about_something_that_is_not_a_note_produces_nothing() {
        let (_tmp, root) = root();

        assert!(
            translate(
                &raw(
                    EventKind::Remove(RemoveKind::File),
                    &[root.join(".scratchbox/order")]
                ),
                &root
            )
            .is_empty(),
            "a manifest write must not reach the caller, or it would spin"
        );
    }

    #[test]
    fn opening_and_reading_a_note_changes_nothing() {
        let (_tmp, root) = root();
        fs::write(root.join("note.md"), "body").unwrap();

        assert!(
            translate(
                &raw(EventKind::Access(AccessKind::Read), &[root.join("note.md")]),
                &root
            )
            .is_empty()
        );
    }

    /// `Any` is the backend saying it does not know what happened. Re-listing is cheaper
    /// than being wrong, so it is deliberately not guessed at.
    #[test]
    fn an_uninformative_event_asks_for_a_rescan() {
        let (_tmp, root) = root();

        assert_eq!(
            translate(&raw(EventKind::Any, &[]), &root),
            vec![StoreEvent::Rescan]
        );
        assert_eq!(
            translate(&raw(EventKind::Other, &[root.join("note.md")]), &root),
            vec![StoreEvent::Rescan]
        );
    }

    #[test]
    fn an_identical_event_inside_one_window_is_reported_once() {
        let mut recent = RecentlyEmitted::default();
        let modified = StoreEvent::Modified(id("note.md"));

        assert!(
            !recent.already_reported(&modified),
            "the first report should go through"
        );
        assert!(
            recent.already_reported(&modified),
            "macOS reports metadata and data separately; the second is redundant"
        );

        // A different note is a different change, not a repeat of this one.
        assert!(!recent.already_reported(&StoreEvent::Modified(id("other.md"))));
        // And so is a different kind of change to the same note.
        assert!(!recent.already_reported(&StoreEvent::Removed(id("note.md"))));
    }
}
