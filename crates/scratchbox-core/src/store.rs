//! The storage seam.
//!
//! Everything above this trait works in terms of [`NoteId`] and [`NoteMeta`]; everything
//! filesystem-shaped lives below it. A future network-backed store implements this same
//! trait without the layers above noticing.

use crossbeam_channel::Receiver;

use crate::Result;
use crate::note::{Format, NoteId, NoteMeta};

/// A change to the stored notes.
///
/// **`Created` and `Modified` are not reliably distinguishable.** macOS FSEvents keeps
/// reporting a path as created for as long as its created flag is set, so an edit to an
/// existing note arrives as `Created` there and as `Modified` on Linux. Both mean the same
/// thing to a caller — this note exists and its contents may have changed — so treat them
/// alike and never branch on which one arrived.
///
/// `Removed` is trustworthy: the watcher confirms every event against the disk before
/// reporting it, precisely because the reported kind cannot be trusted on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreEvent {
    Created(NoteId),
    Modified(NoteId),
    Removed(NoteId),
    Renamed {
        from: NoteId,
        to: NoteId,
    },
    /// The event stream lost track — re-list instead of trusting deltas.
    ///
    /// Filesystem watchers drop events under load and `notify` reports the overflow. This
    /// variant exists so callers handle that honestly rather than assuming the stream is
    /// lossless and silently drifting out of sync with the disk.
    Rescan,
}

/// Whether the workspace is still usable.
///
/// A cloud-drive mount can go offline and a user can remove the workspace while the app
/// runs. Callers check this to enter a degraded state instead of failing an autosave
/// every few hundred milliseconds forever.
///
/// `ReadOnly` is best-effort: it reads permission bits, so a directory that is writable in
/// principle but not by this user still reports `Ok`. A failing write remains the
/// authoritative answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceHealth {
    Ok,
    Missing,
    ReadOnly,
}

/// Note storage.
///
/// Object-safe on purpose — callers hold a `Box<dyn Store>` so the backend can be swapped.
/// Keep it that way: no generic methods, no `Self` in return position.
pub trait Store: Send {
    /// Every note in the workspace, in no particular order. Ordering belongs to the layer
    /// that owns the manifest, not here.
    fn list(&self) -> Result<Vec<NoteMeta>>;

    /// Read a note's contents. Notes are UTF-8; anything else is an error rather than a
    /// lossy conversion.
    fn read(&self, id: &NoteId) -> Result<String>;

    /// Replace a note's contents atomically — a crash mid-save never truncates the note.
    fn write(&self, id: &NoteId, content: &str) -> Result<()>;

    /// Create an empty note and return the name it got.
    fn create(&self, format: Format) -> Result<NoteId>;

    /// Rename a note, returning the name it actually received.
    ///
    /// The return value is not decoration: renaming is collision-aware, so the result can
    /// differ from `to`. Callers that track notes by name — the order manifest above all —
    /// need the name that landed on disk, not the one they asked for.
    fn rename(&self, from: &NoteId, to: &NoteId) -> Result<NoteId>;

    /// Move a note to the trash. Never unlinks a note that did not reach the trash first.
    fn delete(&self, id: &NoteId) -> Result<()>;

    /// Is the store still usable?
    ///
    /// Part of the seam because every backend can become unreachable — a cloud mount goes
    /// offline, a network store loses its connection — and the caller needs to degrade
    /// instead of retrying a doomed save every few hundred milliseconds forever.
    fn health(&self) -> WorkspaceHealth;

    /// Begin reporting changes made outside the app.
    ///
    /// Separate from construction because it can be slow: bringing up an FSEvents stream
    /// on macOS takes roughly 140ms, and a user should be looking at their notes well
    /// before that. Callers start it once the first frame is on screen.
    fn start_watching(&mut self) -> Result<()>;

    /// Receive store events.
    ///
    /// **Single-subscriber.** This is a `crossbeam` channel, which distributes messages
    /// among receivers rather than broadcasting to all of them, so a second subscriber
    /// silently steals events from the first. Implementations assert against a second call
    /// in debug builds.
    fn subscribe(&self) -> Receiver<StoreEvent>;
}
