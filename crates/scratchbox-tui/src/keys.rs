//! The keymap.
//!
//! Anything not claimed here belongs to the editor. The editor is the point of the app, so
//! its keys are not shadowed for app-level shortcuts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Focus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    NewNote,
    RequestDelete,
    ConfirmDelete,
    CancelDelete,
    MoveNoteUp,
    MoveNoteDown,
    SelectPrevious,
    SelectNext,
    ToggleFocus,
    KeepMine,
    TakeTheirs,
    /// Hand the key to the editor.
    Edit(KeyEvent),
    Ignore,
}

/// What a key means while a delete is waiting to be confirmed.
///
/// Everything else is swallowed: a stray keystroke must not answer a question about
/// destroying a note, and must not leak into the buffer behind the prompt either.
pub fn map_confirmation(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::ConfirmDelete,
        _ => Action::CancelDelete,
    }
}

/// What a key means while an external change is waiting to be resolved.
///
/// Everything else is swallowed, exactly as for a delete. The buffer's fate is undecided,
/// and letting the user pile more edits onto it would only make the choice harder — and
/// would let a stray `k` answer a question about whose version of a note survives.
///
/// Quit is the one way out that is not an answer: it leaves the external change on disk
/// and drops the buffer, which beats trapping the user in a prompt.
pub fn map_conflict(key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Char('q'), true) | (KeyCode::Char('c'), true) => Action::Quit,
        (KeyCode::Char('k') | KeyCode::Char('K'), false) => Action::KeepMine,
        (KeyCode::Char('t') | KeyCode::Char('T'), false) => Action::TakeTheirs,
        _ => Action::Ignore,
    }
}

pub fn map(key: KeyEvent, focus: Focus) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match (key.code, ctrl, alt) {
        // Ctrl-C arrives as a key event rather than a signal in raw mode, so quitting
        // stays on the ordinary path where the terminal is restored on the way out.
        (KeyCode::Char('q'), true, _) | (KeyCode::Char('c'), true, _) => Action::Quit,
        (KeyCode::Char('n'), true, _) => Action::NewNote,
        (KeyCode::Char('d'), true, _) => Action::RequestDelete,

        (KeyCode::Up, _, true) => Action::MoveNoteUp,
        (KeyCode::Down, _, true) => Action::MoveNoteDown,

        (KeyCode::Tab, ..) => Action::ToggleFocus,

        // Plain arrows move the selection only when the list has focus; in the editor they
        // move the cursor.
        (KeyCode::Up, false, false) if focus == Focus::List => Action::SelectPrevious,
        (KeyCode::Down, false, false) if focus == Focus::List => Action::SelectNext,

        _ if focus == Focus::Editor => Action::Edit(key),
        _ => Action::Ignore,
    }
}
