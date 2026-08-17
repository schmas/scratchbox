//! Autosave, the rename-on-first-content, and what happens when a note changes underneath.
//!
//! Driven headlessly through `App`, like the Phase 6 suite: no terminal, and external
//! changes made by writing to the workspace directly and handing the app the event a
//! watcher would have delivered. That keeps every conflict case deterministic rather than
//! dependent on filesystem timing. One test runs a real watcher, because synthetic events
//! cannot show that the app still recognises its own saves.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use scratchbox_core::order::OrderStore;
use scratchbox_core::watcher::DEBOUNCE_WINDOW;
use scratchbox_core::{
    APP_SUBDIR, FolderSync, NoteId, Store, StoreEvent, Suppressor, WorkspaceHealth,
};
use scratchbox_tui::app::{App, Conflict, IDLE_SAVE};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    workspace: PathBuf,
    /// The store's own record of the writes it is about to make. One entry per write and
    /// two per rename, which makes it an exact count of what the app put on disk.
    suppressor: Arc<Suppressor>,
    app: App,
}

impl Fixture {
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.workspace.join(name)).unwrap()
    }

    fn names(&self) -> Vec<&str> {
        self.app.notes().iter().map(|n| n.id.as_str()).collect()
    }

    fn manifest(&self) -> Vec<String> {
        OrderStore::new(&self.workspace.join(APP_SUBDIR)).load()
    }

    /// Make the pending save come due without waiting on a real 300ms of idleness.
    fn idle_past_the_deadline(&self) {
        thread::sleep(IDLE_SAVE + Duration::from_millis(50));
    }
}

fn fixture(notes: &[(&str, &str)]) -> Fixture {
    // A no-op after the first call, and off entirely unless `$RUST_LOG` names a `scratchbox`
    // target and `$SCRATCHBOX_LOG_DIR` says where to write.
    scratchbox_log::init_for_tests();

    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();
    let suppressor = Arc::clone(store.suppressor());

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
        suppressor,
        app,
    }
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).unwrap()
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.edit(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

/// The name a note created right now gets, before it earns a slug.
fn only_note(f: &Fixture) -> String {
    f.app.notes().first().unwrap().id.as_str().to_owned()
}

// --- autosave --------------------------------------------------------------------------

#[test]
fn typing_then_idling_writes_the_buffer_to_disk() {
    let mut f = fixture(&[("a.md", "before")]);

    type_text(&mut f.app, "after");
    assert!(
        f.app.wake_at().is_some(),
        "typing should have armed a save deadline"
    );
    assert_eq!(f.read("a.md"), "before", "the save fired before it was due");

    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();

    assert_eq!(f.read("a.md"), f.app.editor().text());
    assert!(!f.app.editor().is_dirty());
    assert!(f.app.wake_at().is_none(), "the deadline should be spent");
}

/// The point of a deadline rather than a save per keystroke.
#[test]
fn a_burst_of_typing_produces_exactly_one_write() {
    let mut f = fixture(&[("a.md", "")]);

    type_text(&mut f.app, &"x".repeat(50));
    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();

    assert_eq!(
        f.suppressor.len(),
        1,
        "50 keystrokes should leave one write behind them, not 50"
    );
    assert_eq!(f.read("a.md").len(), 50);
}

#[test]
fn an_unmodified_buffer_never_arms_a_save() {
    let mut f = fixture(&[("a.md", "untouched")]);

    assert!(f.app.wake_at().is_none());
    f.app.save_now().unwrap();

    assert_eq!(f.suppressor.len(), 0, "an idle buffer wrote to disk");
}

/// Undoing back to what is already on disk should cancel the pending write, not perform it.
#[test]
fn editing_back_to_the_saved_content_disarms_the_save() {
    let mut f = fixture(&[("a.md", "")]);

    type_text(&mut f.app, "x");
    assert!(f.app.wake_at().is_some());

    f.app
        .edit(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    assert!(f.app.wake_at().is_none(), "a clean buffer stayed armed");
}

/// The echo of our own save must not come back as an external change.
#[test]
fn a_save_produces_no_reload_even_if_its_own_event_reaches_the_app() {
    let mut f = fixture(&[("a.md", "before")]);
    type_text(&mut f.app, "typed ");
    f.app.save_now().unwrap();

    let text = f.app.editor().text();
    let cursor = f.app.editor().cursor();

    // Suppression normally swallows this; feeding it through anyway proves the app does not
    // depend on suppression alone to avoid reloading over the user.
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    assert_eq!(f.app.editor().text(), text);
    assert_eq!(f.app.editor().cursor(), cursor, "the cursor moved");
    assert_eq!(f.app.conflict(), None);
}

#[test]
fn switching_notes_saves_the_one_being_left() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two")]);

    type_text(&mut f.app, "edited ");
    f.app.select_next().unwrap();

    assert_eq!(f.read("a.md"), "edited one");
    assert_eq!(f.app.editor().text(), "two");
}

#[test]
fn creating_a_note_saves_the_one_being_left() {
    let mut f = fixture(&[("a.md", "one")]);

    type_text(&mut f.app, "edited ");
    f.app.create_note().unwrap();

    assert_eq!(f.read("a.md"), "edited one");
}

#[test]
fn quitting_persists_unsaved_changes() {
    let mut f = fixture(&[("a.md", "one")]);

    type_text(&mut f.app, "unsaved ");
    f.app.quit().unwrap();

    assert!(f.app.should_quit());
    assert_eq!(f.read("a.md"), "unsaved one");
}

/// A failed save is the one moment the user can be told their work is not on disk, so the
/// first quit refuses. The second goes through, or a broken workspace would trap them.
#[test]
fn a_failed_save_refuses_the_first_quit_and_allows_the_second() {
    let mut f = fixture(&[("a.md", "one")]);
    type_text(&mut f.app, "unsaved ");

    // A directory where the note should be: the write fails, the workspace is still fine.
    fs::remove_file(f.workspace.join("a.md")).unwrap();
    fs::create_dir(f.workspace.join("a.md")).unwrap();

    f.app.quit().unwrap();
    assert!(
        !f.app.should_quit(),
        "quit lost the buffer without saying so"
    );
    assert!(f.app.status().unwrap().contains("^Q again"));
    assert_eq!(
        f.app.editor().text(),
        "unsaved one",
        "the buffer was dropped"
    );

    f.app.quit().unwrap();
    assert!(f.app.should_quit(), "the user is trapped in the app");
}

/// The suppression loop for real: a live watcher, genuine filesystem events, and the app
/// draining them the way its event loop does.
///
/// Everything else in this file hands the app synthetic events, which is what makes the
/// conflict cases deterministic — but it also means nothing else here would notice if the
/// registry stopped recognising our own saves. Repeated rather than done once, because a
/// suppression window that is one event too short survives a single round.
#[test]
fn repeated_saves_under_a_live_watcher_never_reload_over_the_buffer() {
    // Called here rather than left to `fixture`, which this test deliberately does not use.
    // It is the one test in the file that drives a real watcher, so CI repeats it twenty times
    // per OS — and it is the run whose interleaving is worth having on disk when it reds.
    scratchbox_log::init_for_tests();

    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();
    fs::write(workspace.join("a.md"), "").unwrap();
    let events = store.subscribe();

    let mut app = App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap();
    app.start_watching().unwrap();

    let mut expected = String::new();
    for round in 0..5 {
        let chunk = format!("round-{round} ");
        type_text(&mut app, &chunk);
        expected.push_str(&chunk);
        app.save_now().unwrap();

        // Long enough for the debouncer to have flushed anything it was holding.
        let until = std::time::Instant::now() + DEBOUNCE_WINDOW + Duration::from_millis(200);
        while let Ok(event) = events.recv_deadline(until) {
            app.apply_store_event(&event).unwrap();
        }

        assert_eq!(app.editor().text(), expected, "round {round} lost text");
        assert_eq!(app.conflict(), None, "round {round} raised a conflict");
        assert!(!app.editor().is_dirty(), "round {round} left work unsaved");
    }
    assert_eq!(
        fs::read_to_string(workspace.join("a.md")).unwrap(),
        expected
    );
}

// --- rename on first content -----------------------------------------------------------

#[test]
fn the_first_save_with_a_title_renames_the_note() {
    let mut f = fixture(&[]);
    f.app.create_note().unwrap();
    let born_as = only_note(&f);

    type_text(&mut f.app, "My Note\nbody");
    f.app.save_now().unwrap();

    let renamed = only_note(&f);
    let stem = born_as.trim_end_matches(".md");
    assert_eq!(renamed, format!("{stem}-my-note.md"));
    assert!(
        !f.workspace.join(&born_as).exists(),
        "the old name survived"
    );
    assert_eq!(f.read(&renamed), "My Note\nbody");
    assert_eq!(
        f.app.editor().loaded().unwrap().as_str(),
        renamed,
        "the buffer is still pointing at the old name"
    );
    assert_eq!(f.app.selected().unwrap().as_str(), renamed);
}

/// D10 freezes the name after the first rename, so a note stays stable enough to reference.
#[test]
fn editing_the_first_line_again_does_not_rename() {
    let mut f = fixture(&[]);
    f.app.create_note().unwrap();

    type_text(&mut f.app, "First Title");
    f.app.save_now().unwrap();
    let named = only_note(&f);

    type_text(&mut f.app, " Rewritten Completely");
    f.app.save_now().unwrap();

    assert_eq!(only_note(&f), named, "the note renamed itself twice");
}

/// A first line with no letters or digits in it is not a title; the note waits.
#[test]
fn a_save_with_no_usable_first_line_leaves_the_note_unnamed() {
    let mut f = fixture(&[]);
    f.app.create_note().unwrap();
    let born_as = only_note(&f);

    type_text(&mut f.app, "***\nbody");
    f.app.save_now().unwrap();
    assert_eq!(only_note(&f), born_as);

    // And it still renames later, once there is something to name it after.
    f.app.editor_mut().reload("Real Title\nbody");
    type_text(&mut f.app, "!");
    f.app.save_now().unwrap();

    assert_ne!(only_note(&f), born_as, "the note never earned its name");
}

#[test]
fn a_renamed_note_keeps_its_position_in_the_manifest() {
    let mut f = fixture(&[("a.md", "one"), ("b.md", "two"), ("c.md", "three")]);
    f.app.create_note().unwrap();

    // Park the new note in the middle, where a rename that re-added it would be visible.
    f.app.move_selection_down().unwrap();
    f.app.move_selection_down().unwrap();
    let born_as = f.names()[2].to_owned();

    type_text(&mut f.app, "Middle Note");
    f.app.save_now().unwrap();

    let renamed = f.names()[2].to_owned();
    assert_ne!(renamed, born_as, "the rename did not happen");
    assert_eq!(f.names(), ["a.md", "b.md", &renamed, "c.md"]);
    assert_eq!(f.manifest(), ["a.md", "b.md", &renamed, "c.md"]);
    assert_eq!(f.app.selected().unwrap().as_str(), renamed);
}

/// RT-5: the baseline is recorded before the rename, so a rescan arriving right after one
/// compares the renamed file against current content rather than raising a conflict.
#[test]
fn a_rescan_straight_after_a_rename_raises_nothing() {
    let mut f = fixture(&[]);
    f.app.create_note().unwrap();
    type_text(&mut f.app, "Fresh Note");
    f.app.save_now().unwrap();
    let text = f.app.editor().text();

    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(f.app.conflict(), None, "our own rename read as a conflict");
    assert_eq!(f.app.editor().text(), text);
    assert_eq!(f.app.selected().unwrap().as_str(), only_note(&f));
}

// --- external changes ------------------------------------------------------------------

#[test]
fn an_external_change_reloads_a_clean_buffer_and_keeps_the_cursor() {
    let mut f = fixture(&[("a.md", "line one\nline two\nline three")]);
    // Put the cursor somewhere a reload could plausibly lose it.
    for _ in 0..2 {
        f.app.edit(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    f.app
        .edit(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let cursor = f.app.editor().cursor();
    assert!(cursor.row > 0, "the cursor never moved off the first line");

    fs::write(f.workspace.join("a.md"), "line one\nline two\nline THREE").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    assert_eq!(f.app.editor().text(), "line one\nline two\nline THREE");
    assert_eq!(f.app.editor().cursor(), cursor);
    assert_eq!(f.app.conflict(), None);
}

/// The reload has to survive the external change being shorter than the cursor position.
#[test]
fn a_reload_clamps_a_cursor_the_new_content_cannot_hold() {
    let mut f = fixture(&[("a.md", "line one\nline two\nline three")]);
    for _ in 0..2 {
        f.app.edit(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }

    fs::write(f.workspace.join("a.md"), "x").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    let cursor = f.app.editor().cursor();
    assert_eq!(cursor.row, 0);
    assert!(cursor.col <= 1);
}

#[test]
fn an_external_change_to_a_dirty_buffer_conflicts_instead_of_reloading() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    let mine = f.app.editor().text();

    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    assert_eq!(f.app.conflict(), Some(Conflict::Changed));
    assert_eq!(f.app.editor().text(), mine, "the buffer was reloaded over");
    assert!(f.app.wake_at().is_none(), "autosave is still armed");
}

/// The failure the conflicted state exists to prevent: autosave writing the buffer over an
/// external change while the user is still being asked about it.
#[test]
fn a_conflict_stops_autosave_from_overwriting_the_external_change() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    // Everything that could plausibly write, while the question is open.
    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();
    f.app.save_now().unwrap();
    type_text(&mut f.app, "more ");
    f.app.on_tick().unwrap();

    assert_eq!(f.read("a.md"), "theirs");
}

#[test]
fn keeping_mine_writes_the_buffer_and_lets_autosave_go_again() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    f.app.keep_mine().unwrap();

    assert_eq!(f.app.conflict(), None);
    assert_eq!(f.read("a.md"), "mine original");

    type_text(&mut f.app, "and more ");
    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();
    assert_eq!(f.read("a.md"), f.app.editor().text());
}

#[test]
fn taking_theirs_loads_the_external_content() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    f.app.take_theirs().unwrap();

    assert_eq!(f.app.conflict(), None);
    assert_eq!(f.app.editor().text(), "theirs");
    assert!(!f.app.editor().is_dirty());
}

/// The path the risk assessment called out: leaving a conflicted note and coming back must
/// not be a way for autosave to resume with the question still unanswered.
#[test]
fn a_conflicted_note_cannot_be_navigated_away_from() {
    let mut f = fixture(&[("a.md", "original"), ("b.md", "other")]);
    type_text(&mut f.app, "mine ");
    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    f.app.select_next().unwrap();
    f.app.create_note().unwrap();

    assert_eq!(f.app.selected().unwrap().as_str(), "a.md");
    assert_eq!(f.app.conflict(), Some(Conflict::Changed));
    assert_eq!(f.app.editor().text(), "mine original");
    assert_eq!(f.read("a.md"), "theirs", "something wrote while conflicted");
}

#[test]
fn an_external_delete_of_a_dirty_note_offers_to_write_it_back() {
    let mut f = fixture(&[("a.md", "original"), ("b.md", "other")]);
    type_text(&mut f.app, "mine ");

    fs::remove_file(f.workspace.join("a.md")).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Removed(id("a.md")))
        .unwrap();

    assert_eq!(f.app.conflict(), Some(Conflict::Deleted));
    assert_eq!(f.app.editor().text(), "mine original");
    assert_eq!(
        f.app.selected().unwrap().as_str(),
        "a.md",
        "the selection wandered off the note being decided about"
    );

    f.app.keep_mine().unwrap();

    assert_eq!(f.read("a.md"), "mine original");
    assert!(f.names().contains(&"a.md"), "the note did not come back");
}

/// A note that is still listed but can no longer be read leaves stale contents in the
/// buffer, so it has to say why rather than looking like nothing happened.
#[test]
fn a_note_that_became_unreadable_reports_itself() {
    let mut f = fixture(&[("a.md", "original")]);

    fs::write(f.workspace.join("a.md"), [0xff, 0xfe]).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    assert_eq!(f.app.conflict(), None);
    assert!(
        f.app.status().unwrap().contains("cannot reload"),
        "an unreadable note changed nothing on screen"
    );
}

#[test]
fn an_external_delete_of_a_clean_note_just_moves_on() {
    let mut f = fixture(&[("a.md", "original"), ("b.md", "other")]);

    fs::remove_file(f.workspace.join("a.md")).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Removed(id("a.md")))
        .unwrap();

    assert_eq!(f.app.conflict(), None);
    assert_eq!(f.app.editor().text(), "other");
    assert_eq!(f.app.selected().unwrap().as_str(), "b.md");
}

#[test]
fn taking_theirs_on_a_deleted_note_lets_it_go() {
    let mut f = fixture(&[("a.md", "original"), ("b.md", "other")]);
    type_text(&mut f.app, "mine ");
    fs::remove_file(f.workspace.join("a.md")).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Removed(id("a.md")))
        .unwrap();

    f.app.take_theirs().unwrap();

    assert_eq!(f.app.conflict(), None);
    assert_eq!(f.names(), ["b.md"]);
    assert_eq!(f.app.editor().text(), "other");
}

#[test]
fn an_external_rename_follows_the_note_rather_than_losing_it() {
    let mut f = fixture(&[("a.md", "original")]);

    fs::rename(f.workspace.join("a.md"), f.workspace.join("renamed.md")).unwrap();
    f.app
        .apply_store_event(&StoreEvent::Renamed {
            from: id("a.md"),
            to: id("renamed.md"),
        })
        .unwrap();

    assert_eq!(f.app.selected().unwrap().as_str(), "renamed.md");
    assert_eq!(f.app.editor().loaded().unwrap().as_str(), "renamed.md");
    assert_eq!(f.app.editor().text(), "original");
    assert_eq!(f.app.conflict(), None);
}

// --- rescan ----------------------------------------------------------------------------

/// RT-2: a rescan carries no path, so it is reconciled by comparison rather than guessed at.
#[test]
fn a_rescan_over_an_unchanged_file_reloads_nothing() {
    let mut f = fixture(&[("a.md", "original")]);

    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(f.app.conflict(), None);
    assert_eq!(f.app.editor().text(), "original");
}

#[test]
fn a_rescan_after_an_external_change_reloads_a_clean_buffer() {
    let mut f = fixture(&[("a.md", "original")]);

    fs::write(f.workspace.join("a.md"), "changed elsewhere").unwrap();
    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(f.app.editor().text(), "changed elsewhere");
    assert_eq!(f.app.conflict(), None);
}

#[test]
fn a_rescan_after_an_external_change_conflicts_with_a_dirty_buffer() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");

    fs::write(f.workspace.join("a.md"), "changed elsewhere").unwrap();
    f.app.apply_store_event(&StoreEvent::Rescan).unwrap();

    assert_eq!(f.app.conflict(), Some(Conflict::Changed));
    assert_eq!(f.app.editor().text(), "mine original");

    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();
    assert_eq!(f.read("a.md"), "changed elsewhere");
}

// --- workspace health ------------------------------------------------------------------

/// RT-4: a workspace that goes away must not take the user's text with it, and must not
/// leave an autosave loop hammering a dead mount.
#[test]
fn losing_the_workspace_halts_autosave_and_keeps_the_buffer() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");

    fs::remove_dir_all(&f.workspace).unwrap();
    f.idle_past_the_deadline();
    f.app.on_tick().unwrap();

    assert_eq!(f.app.health(), WorkspaceHealth::Missing);
    assert_eq!(
        f.app.editor().text(),
        "mine original",
        "the buffer was lost"
    );

    // Waking again in five seconds to look, not in another 300ms to fail again.
    let next = f.app.wake_at().expect("nothing scheduled a recovery check");
    assert!(
        next.saturating_duration_since(std::time::Instant::now()) > Duration::from_secs(4),
        "autosave is still retrying on a dead workspace"
    );

    // And more typing does not re-arm it.
    type_text(&mut f.app, "more ");
    assert!(
        f.app
            .wake_at()
            .unwrap()
            .saturating_duration_since(std::time::Instant::now())
            > Duration::from_secs(4)
    );
}

/// The other half of RT-4. The five-second timer only decides *when* this runs; what it
/// runs is the same health check every save goes through, which is what is exercised here.
#[test]
fn a_restored_workspace_resumes_autosave_and_flushes_the_buffer() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::remove_dir_all(&f.workspace).unwrap();
    f.app.save_now().unwrap();
    assert_eq!(f.app.health(), WorkspaceHealth::Missing);

    fs::create_dir_all(f.workspace.join(APP_SUBDIR)).unwrap();
    f.app.save_now().unwrap();

    assert_eq!(f.app.health(), WorkspaceHealth::Ok);
    assert_eq!(f.read("a.md"), "mine original", "the buffer never flushed");
    assert!(
        f.app.wake_at().is_none(),
        "the recovery check is still armed"
    );
}
