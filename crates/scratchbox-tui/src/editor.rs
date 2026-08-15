//! The editing buffer.

use crossterm::event::KeyEvent;
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};
use scratchbox_core::NoteId;

pub struct EditorPane {
    state: EditorState,
    handler: EditorEventHandler,
    loaded: Option<NoteId>,
    /// Contents as last read from or written to disk, for telling edited from untouched.
    baseline: String,
}

impl EditorPane {
    pub fn new() -> Self {
        Self {
            state: modeless(EditorState::new(Lines::default())),
            handler: EditorEventHandler::emacs_mode(),
            loaded: None,
            baseline: String::new(),
        }
    }

    pub fn loaded(&self) -> Option<&NoteId> {
        self.loaded.as_ref()
    }

    pub fn state_mut(&mut self) -> &mut EditorState {
        &mut self.state
    }

    /// Put a note in the buffer.
    ///
    /// Undo history does not survive this. edtui keeps its stack inside the state being
    /// replaced, and carrying one stack across notes would let an undo in note B remove
    /// text from note A.
    pub fn load(&mut self, id: NoteId, content: &str) {
        self.state = modeless(EditorState::new(Lines::from(content)));
        self.loaded = Some(id);
        self.baseline = content.to_owned();
    }

    pub fn clear(&mut self) {
        self.state = modeless(EditorState::new(Lines::default()));
        self.loaded = None;
        self.baseline.clear();
    }

    /// Replace the buffer with what is on disk, keeping the cursor where it was.
    ///
    /// Only ever called for a buffer with nothing unsaved in it — the decision about
    /// whether that is true belongs to [`crate::app::App`], not here.
    pub fn reload(&mut self, content: &str) {
        let cursor = self.state.cursor;
        self.state = modeless(EditorState::new(Lines::from(content)));
        // Clamped rather than restored as-is: the change that arrived may have made the
        // note shorter than the position the cursor was holding.
        self.state.cursor = clamp(&self.state.lines, cursor);
        self.baseline = content.to_owned();
    }

    pub fn text(&self) -> String {
        String::from(self.state.lines.clone())
    }

    /// What the disk held when this note was last read or written.
    pub fn baseline(&self) -> &str {
        &self.baseline
    }

    /// Where the cursor sits. Exposed so a reload can be shown to have preserved it.
    pub fn cursor(&self) -> Index2 {
        self.state.cursor
    }

    /// Does the buffer hold anything not yet on disk?
    pub fn is_dirty(&self) -> bool {
        self.loaded.is_some() && self.text() != self.baseline
    }

    /// Record that `saved` is now what the disk holds.
    pub fn mark_saved(&mut self, saved: String) {
        self.baseline = saved;
    }

    /// Follow the note to a new name after a rename, without disturbing the buffer.
    pub fn rename(&mut self, id: NoteId) {
        if self.loaded.is_some() {
            self.loaded = Some(id);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.handler.on_key_event(key, &mut self.state);
    }
}

impl Default for EditorPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Put the editor in the one mode it ever uses.
///
/// `EditorState` starts in Normal, and every emacs binding is registered against Insert,
/// so without this the arrow keys and typing do nothing at all. This single line is what
/// makes the editor modeless.
fn modeless(mut state: EditorState) -> EditorState {
    state.mode = EditorMode::Insert;
    state
}

/// Pull a remembered position back inside a buffer that may have shrunk under it.
fn clamp(lines: &Lines, cursor: Index2) -> Index2 {
    let row = cursor.row.min(lines.len().saturating_sub(1));
    // The column may equal the line length: that is where the cursor sits when it is
    // about to append, which is the normal resting place in an insert-mode editor.
    let col = cursor.col.min(lines.len_col(row).unwrap_or(0));
    Index2::new(row, col)
}
