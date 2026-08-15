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
            let mut batch = Vec::new();
            for debounced in events {
                for event in translate(&debounced.event, &root) {
                    if !batch.contains(&event) {
                        batch.push(event);
                    }
                }
            }

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
                    continue;
                }
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
