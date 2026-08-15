//! App behaviour, driven without a terminal.
//!
//! Everything here runs headlessly in CI: state lives in `App`, and rendering only reads
//! it, so the interesting logic is reachable without a TTY.

use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use scratchbox_core::order::OrderStore;
use scratchbox_core::{APP_SUBDIR, FolderSync, NoteId, StoreEvent};
use scratchbox_tui::app::{App, Focus};
use scratchbox_tui::keys::{self, Action};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    workspace: std::path::PathBuf,
    app: App,
}

/// An app over a workspace holding `notes`, oldest first so the listing order is known.
fn fixture(notes: &[(&str, &str)]) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();

    for (name, body) in notes {
        fs::write(workspace.join(name), body).unwrap();
    }
    if !notes.is_empty() {
        let ids: Vec<NoteId> = notes.iter().map(|(name, _)| id(name)).collect();
        OrderStore::new(&workspace.join(APP_SUBDIR))
            .save(&ids)
            .unwrap();
    }

    let app = App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        workspace,
        app,
    }
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).unwrap()
}

fn names(app: &App) -> Vec<&str> {
    app.notes().iter().map(|note| note.id.as_str()).collect()
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.edit(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn reopen(workspace: &Path, trash: &Path) -> App {
    let store = FolderSync::new(workspace.to_path_buf(), trash.to_path_buf()).unwrap();
    App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap()
}

#[test]
fn notes_are_listed_in_manifest_order() {
    let f = fixture(&[("a.md", "one"), ("b.md", "two"), ("c.md", "three")]);

    assert_eq!(names(&f.app), ["a.md", "b.md", "c.md"]);
    assert_eq!(f.app.selected(), Some(&id("a.md")));
}

#[test]
fn selecting_a_note_loads_it() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);

    f.app.select_next().unwrap();

    assert_eq!(f.app.selected(), Some(&id("b.md")));
    assert_eq!(f.app.editor().text(), "two");
}

#[test]
fn selection_stops_at_the_ends_of_the_list() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);

    f.app.select_previous().unwrap();
    assert_eq!(f.app.selected(), Some(&id("a.md")));

    f.app.select_next().unwrap();
    f.app.select_next().unwrap();
    assert_eq!(f.app.selected(), Some(&id("b.md")));
}

/// The classic bug in this shape of UI: a list that shifts under an index-based selection
/// silently moves the user to a different note.
#[test]
fn selection_survives_a_note_appearing_above_it() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two"), ("c.md", "three")]);
    f.app.select_next().unwrap();
    f.app.select_next().unwrap();
    assert_eq!(f.app.selected(), Some(&id("c.md")));

    // Something else drops a note into the workspace.
    fs::write(f.workspace.join("from-elsewhere.md"), "new").unwrap();
    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(
        f.app.selected(),
        Some(&id("c.md")),
        "the selection followed the index instead of the note"
    );
    assert_eq!(names(&f.app)[0], "from-elsewhere.md");
}

/// Phase 7 owns buffer reconciliation. A rescan here must not touch what the user is
/// typing, or an external change would silently discard unsaved work.
#[test]
fn a_rescan_leaves_the_open_buffer_untouched() {
    let mut f = fixture(&[("a.md", "on disk")]);
    type_text(&mut f.app, "unsaved edits");
    let before = f.app.editor().text();

    fs::write(f.workspace.join("a.md"), "changed underneath").unwrap();
    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(
        f.app.editor().text(),
        before,
        "a rescan overwrote the buffer"
    );
}

#[test]
fn a_new_note_lands_on_top_and_is_opened() {
    let mut f = fixture(&[("a.md", "one")]);

    f.app.create_note().unwrap();

    assert_eq!(f.app.notes().len(), 2);
    assert_eq!(
        f.app.selected(),
        f.app.notes().first().map(|note| &note.id),
        "a new note should be selected at the top of the list"
    );
    assert_eq!(f.app.editor().text(), "");
    assert_eq!(f.app.focus(), Focus::Editor);
}

#[test]
fn reordering_moves_the_note_and_survives_a_restart() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two"), ("c.md", "three")]);
    f.app.select_next().unwrap();
    assert_eq!(f.app.selected(), Some(&id("b.md")));

    f.app.move_selection_up().unwrap();

    assert_eq!(names(&f.app), ["b.md", "a.md", "c.md"]);
    assert_eq!(f.app.selected(), Some(&id("b.md")), "the note keeps focus");

    let reopened = reopen(&f.workspace, &f._tmp.path().join("trash"));
    assert_eq!(names(&reopened), ["b.md", "a.md", "c.md"]);
}

#[test]
fn reordering_at_the_ends_does_nothing() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);

    f.app.move_selection_up().unwrap();
    assert_eq!(names(&f.app), ["a.md", "b.md"]);

    f.app.select_next().unwrap();
    f.app.move_selection_down().unwrap();
    assert_eq!(names(&f.app), ["a.md", "b.md"]);
}

#[test]
fn deleting_asks_first_and_then_moves_the_note_to_the_trash() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);
    let trash = f._tmp.path().join("trash");

    f.app.request_delete();
    assert_eq!(f.app.pending_delete(), Some(&id("a.md")));

    // Second thoughts.
    f.app.cancel_delete();
    assert_eq!(names(&f.app), ["a.md", "b.md"]);
    assert!(!trash.join("a.md").exists());

    f.app.request_delete();
    f.app.confirm_delete().unwrap();

    assert_eq!(names(&f.app), ["b.md"]);
    assert_eq!(fs::read_to_string(trash.join("a.md")).unwrap(), "one");
    assert_eq!(
        f.app.selected(),
        Some(&id("b.md")),
        "the selection should land on the note that took its place"
    );
}

#[test]
fn deleting_the_last_note_leaves_an_empty_workspace() {
    let mut f = fixture(&[("only.md", "solo")]);

    f.app.request_delete();
    f.app.confirm_delete().unwrap();

    assert!(names(&f.app).is_empty());
    assert_eq!(f.app.selected(), None);
    assert_eq!(f.app.editor().text(), "");
}

#[test]
fn an_external_removal_drops_the_note_from_the_list() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);

    fs::remove_file(f.workspace.join("b.md")).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Removed(id("b.md")))
        .unwrap();

    assert_eq!(names(&f.app), ["a.md"]);
}

// --- editing -------------------------------------------------------------------------

#[test]
fn typing_inserts_immediately_with_no_mode_to_enter() {
    let mut f = fixture(&[("a.md", "")]);

    type_text(&mut f.app, "hello");

    assert_eq!(f.app.editor().text(), "hello");
    assert!(f.app.editor().is_dirty());
}

/// Escape is what would switch modes in a modal editor. Here it must do nothing at all.
#[test]
fn escape_does_not_change_how_typing_behaves() {
    let mut f = fixture(&[("a.md", "")]);

    type_text(&mut f.app, "one");
    f.app.edit(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    type_text(&mut f.app, " two");

    assert_eq!(f.app.editor().text(), "one two");
}

/// Undo is the reason a third-party editor widget was chosen over hand-rolling one, so it
/// is verified through the real integration rather than assumed from the spike.
#[test]
fn undo_takes_back_an_edit_and_redo_puts_it_back() {
    let mut f = fixture(&[("a.md", "")]);
    type_text(&mut f.app, "hello world");

    f.app
        .edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    let undone = f.app.editor().text();
    assert_ne!(undone, "hello world", "ctrl+u changed nothing");

    f.app
        .edit(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

    assert_eq!(
        f.app.editor().text(),
        "hello world",
        "ctrl+r did not restore the edit"
    );
}

/// Documented limitation: undo history belongs to the buffer, and switching notes replaces
/// it. Carrying one stack across notes would let an undo in one note delete text from
/// another, which is worse than losing the history.
#[test]
fn undo_history_does_not_survive_a_note_switch() {
    let mut f = fixture(&[("a.md", "original"), ("b.md", "other")]);
    type_text(&mut f.app, "typed ");
    assert!(f.app.editor().is_dirty());

    f.app.select_next().unwrap();
    f.app.select_previous().unwrap();

    // The edit itself survives — switching notes saves first — but the buffer it lives in
    // was rebuilt from disk on the way back, and the undo stack went with the old one.
    assert_eq!(f.app.editor().text(), "typed original");
    f.app
        .edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(
        f.app.editor().text(),
        "typed original",
        "undo reached across a note switch"
    );
}

#[test]
fn a_freshly_loaded_note_is_not_dirty() {
    let f = fixture(&[("a.md", "untouched")]);

    assert!(!f.app.editor().is_dirty());
}

// --- keymap --------------------------------------------------------------------------

#[test]
fn app_shortcuts_are_recognized_in_both_panes() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

    for focus in [Focus::List, Focus::Editor] {
        assert_eq!(keys::map(ctrl('q'), focus), Action::Quit);
        // Raw mode delivers Ctrl-C as a key, so it leaves through the same door.
        assert_eq!(keys::map(ctrl('c'), focus), Action::Quit);
        assert_eq!(keys::map(ctrl('n'), focus), Action::NewNote);
        assert_eq!(keys::map(ctrl('d'), focus), Action::RequestDelete);
        assert_eq!(
            keys::map(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), focus),
            Action::ToggleFocus
        );
        assert_eq!(
            keys::map(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT), focus),
            Action::MoveNoteUp
        );
    }
}

/// Arrow keys mean different things per pane, and the editor's must not be stolen.
#[test]
fn arrows_move_the_selection_in_the_list_and_the_cursor_in_the_editor() {
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);

    assert_eq!(keys::map(up, Focus::List), Action::SelectPrevious);
    assert_eq!(keys::map(up, Focus::Editor), Action::Edit(up));
}

#[test]
fn only_an_explicit_yes_confirms_a_delete() {
    assert_eq!(
        keys::map_confirmation(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        Action::ConfirmDelete
    );
    assert_eq!(
        keys::map_confirmation(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::ConfirmDelete
    );
    // Anything else cancels: a stray keystroke must not destroy a note.
    for code in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('x')] {
        assert_eq!(
            keys::map_confirmation(KeyEvent::new(code, KeyModifiers::NONE)),
            Action::CancelDelete
        );
    }
}
