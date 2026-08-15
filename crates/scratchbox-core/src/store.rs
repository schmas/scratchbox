//! The storage seam.
//!
//! Everything above this trait works in terms of [`NoteId`] and [`NoteMeta`]; everything
//! filesystem-shaped lives below it. A future network-backed store implements this same
//! trait without the layers above noticing.

use crossbeam_channel::Receiver;

use crate::Result;
use crate::note::{Format, NoteId, NoteMeta};

/// A change to the stored notes.
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

    /// Receive store events.
    ///
    /// **Single-subscriber.** This is a `crossbeam` channel, which distributes messages
    /// among receivers rather than broadcasting to all of them, so a second subscriber
    /// silently steals events from the first. Implementations assert against a second call
    /// in debug builds.
    fn subscribe(&self) -> Receiver<StoreEvent>;
}
