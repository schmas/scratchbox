//! Getting the buffer onto the disk.
//!
//! Small and separate because the order of its four steps is the whole point: a save that
//! writes before it announces itself, or renames before it records what it wrote, comes
//! back through the watcher looking like somebody else's change.

use scratchbox_core::order::OrderStore;
use scratchbox_core::{NoteId, Result, Store, naming};

use crate::editor::EditorPane;

/// What a save did, beyond putting bytes on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to write: no note open, or the buffer already matches the disk.
    Unchanged,
    Written,
    /// Written, and the note took its name from its first line.
    Renamed {
        from: NoteId,
        to: NoteId,
    },
}

/// Persist the buffer, renaming the note if this is the save that gave it a title.
///
/// A free function over three borrows rather than a method, so the caller can hand it the
/// store, the manifest, and the buffer without lending out all of itself.
pub fn save(store: &dyn Store, order: &OrderStore, editor: &mut EditorPane) -> Result<Outcome> {
    let Some(id) = editor.loaded().cloned() else {
        return Ok(Outcome::Unchanged);
    };
    if !editor.is_dirty() {
        return Ok(Outcome::Unchanged);
    }

    let content = editor.text();

    // A span rather than two loose events, so everything the watcher and the suppressor say
    // while this save is in flight is attributed to it. That attribution is the point: the
    // race worth diagnosing is a save and its own echo arriving in the wrong order.
    //
    // `info_span!`, not `#[instrument]` — the attribute is a proc macro, and `tracing` is
    // taken without `attributes` precisely to keep syn/quote/proc-macro2 out of the graph.
    let span = tracing::info_span!("save", id = ?id.as_str(), bytes = content.len());
    let _entered = span.enter();
    tracing::info!("start");

    // `Store::write` announces itself to the suppression registry before it touches the
    // disk, which is what keeps the resulting filesystem event from reloading the note
    // over whatever the user types next.
    store.write(&id, &content)?;

    // Recorded before the rename, not after. A `Rescan` landing in between compares the
    // renamed file against this baseline, and a stale one would read our own save as an
    // external change and raise a conflict over nothing.
    editor.mark_saved(content.clone());

    let Some(target) = slug_target(&id, &content) else {
        tracing::info!(outcome = "written", "finish");
        return Ok(Outcome::Written);
    };

    // Collision-aware, so the name that lands can differ from the one asked for.
    let landed = store.rename(&id, &target)?;
    editor.rename(landed.clone());

    // `FolderSync` already moved the manifest entry, and does so in place so the note keeps
    // its position. Repeating it here costs one read of a small file and covers a store
    // that does not maintain the manifest; the second call finds nothing to do and writes
    // nothing.
    //
    // Not fatal, for the same reason the store treats it that way: the note has already
    // been written and renamed, and reconciliation repairs a stale entry from the timestamp
    // prefix. Failing the save here would report a loss that did not happen — and would
    // abort the note switch that asked for it.
    let _ = order.record_rename(&id, &landed);

    tracing::info!(outcome = "renamed", landed = ?landed.as_str(), "finish");
    Ok(Outcome::Renamed {
        from: id,
        to: landed,
    })
}

/// The name this note should take from its first line, if it has not been named yet.
///
/// `None` covers both halves of D10: a note that was already renamed once never renames
/// again, and a first line with no title in it leaves the note unnamed until a later save.
fn slug_target(id: &NoteId, content: &str) -> Option<NoteId> {
    if naming::is_slugged(id) {
        return None;
    }
    let slug = naming::slug_from_first_line(content)?;
    NoteId::new(&naming::slugged_name(id, &slug)).ok()
}
