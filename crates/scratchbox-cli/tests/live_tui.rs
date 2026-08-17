//! The CLI writing into a workspace an open TUI is watching.
//!
//! The two halves of the product only ever meet on the filesystem — no daemon, no IPC — so
//! this is the one place their agreement can be checked. It is also where the plan expects
//! the most likely real-world data loss: the CLI appends a captured thought while the
//! editor holds unsaved work on the same note, and something has to not overwrite something
//! else.
//!
//! A real `FolderSync` with a real watcher, and the real binary as a child process. Events
//! are drained the way the event loop drains them.

mod support;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use scratchbox_core::order::OrderStore;
use scratchbox_core::watcher::DEBOUNCE_WINDOW;
use scratchbox_core::{APP_SUBDIR, FolderSync, Store};
use scratchbox_tui::app::{App, Conflict};
use support::Sandbox;

/// An app watching the sandbox workspace, with the first note open.
fn open_tui(
    sandbox: &Sandbox,
) -> (
    App,
    crossbeam_channel::Receiver<scratchbox_core::StoreEvent>,
) {
    // By this file's own header this is where the plan expects the most likely real-world data
    // loss — the CLI appending while the editor holds unsaved work — so it is the last suite
    // that should be silent when it reds. The child `scratchbox` process installs its own
    // subscriber from the same `$RUST_LOG`; this covers the in-process half.
    scratchbox_log::init_for_tests();

    let store = FolderSync::new(sandbox.workspace(), sandbox.trash()).unwrap();
    let events = store.subscribe();

    let mut app = App::new(
        Box::new(store),
        OrderStore::new(&sandbox.workspace().join(APP_SUBDIR)),
    )
    .unwrap();
    app.start_watching().unwrap();
    (app, events)
}

/// Feed the app everything the watcher reports, then stop.
///
/// Waits out the debounce window rather than sleeping a guessed interval: the watcher
/// coalesces by design, so the answer is not available before it has elapsed.
fn drain(
    app: &mut App,
    events: &crossbeam_channel::Receiver<scratchbox_core::StoreEvent>,
) -> usize {
    let until = Instant::now() + DEBOUNCE_WINDOW + Duration::from_millis(400);
    let mut delivered = 0;
    while let Ok(event) = events.recv_deadline(until) {
        app.apply_store_event(&event).unwrap();
        delivered += 1;
    }
    delivered
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.edit(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

#[test]
fn an_append_reaches_an_open_editor_that_has_nothing_unsaved() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    let (mut app, events) = open_tui(&sandbox);
    assert_eq!(app.editor().text(), "already here\n");

    let output = sandbox.run(
        &["--workspace", sandbox.workspace().to_str().unwrap()],
        b"from the hotkey\n",
    );
    assert_eq!(output.status.code().unwrap(), 0);

    let delivered = drain(&mut app, &events);

    assert_eq!(app.editor().text(), "already here\nfrom the hotkey\n");
    assert_eq!(app.conflict(), None, "a clean buffer should just reload");
    assert!(!app.editor().is_dirty());

    // The loop draws once per event, so this is the repaint count for one append.
    //
    // An append is several filesystem operations — a temp file written, then renamed over
    // the target — and macOS reports them separately, sometimes as a `Created` and a
    // `Modified` for the same path. Those are different variants, so the watcher's
    // deduplication cannot collapse them and two events arrive rather than one. Both
    // describe the same final content, which makes the second repaint redundant rather
    // than wrong: the buffer is already correct when it happens.
    //
    // Bounded rather than pinned to a number, because whether the two operations land in
    // one debounce window is a matter of timing. What matters is that the count stays a
    // small constant instead of tracking the number of filesystem operations.
    assert!(
        (1..=2).contains(&delivered),
        "one append produced {delivered} events; the debouncer is not collapsing them"
    );
}

/// The data-loss path the plan singles out. The append must not be reloaded over the user's
/// unsaved work, and the user's unsaved work must not be autosaved over the append.
#[test]
fn an_append_against_an_unsaved_buffer_conflicts_rather_than_losing_either_side() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    let (mut app, events) = open_tui(&sandbox);
    type_text(&mut app, "mine ");
    let typed = app.editor().text();

    let output = sandbox.run(
        &["--workspace", sandbox.workspace().to_str().unwrap()],
        b"from the hotkey\n",
    );
    assert_eq!(output.status.code().unwrap(), 0);

    drain(&mut app, &events);

    assert_eq!(app.conflict(), Some(Conflict::Changed));
    assert_eq!(
        app.editor().text(),
        typed,
        "the append overwrote the buffer"
    );

    // Everything that could write, while the question is open.
    app.on_tick().unwrap();
    app.save_now().unwrap();
    assert_eq!(
        sandbox.read("a.md"),
        "already here\nfrom the hotkey\n",
        "autosave overwrote the appended text"
    );

    // And the user can still have their version, without the append vanishing silently —
    // it is on disk and in the trash of nothing, but the choice was theirs to make.
    app.keep_mine().unwrap();
    assert_eq!(app.conflict(), None);
    assert_eq!(sandbox.read("a.md"), typed);
}

/// The other resolution, which is the one that keeps the captured thought.
#[test]
fn taking_theirs_after_an_append_keeps_the_captured_text() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    let (mut app, events) = open_tui(&sandbox);
    type_text(&mut app, "mine ");

    sandbox.run(
        &["--workspace", sandbox.workspace().to_str().unwrap()],
        b"from the hotkey\n",
    );
    drain(&mut app, &events);
    assert_eq!(app.conflict(), Some(Conflict::Changed));

    app.take_theirs().unwrap();

    assert_eq!(app.editor().text(), "already here\nfrom the hotkey\n");
    assert!(!app.editor().is_dirty());
}

/// An append to a note the editor does not have open is a list-level event and nothing more.
#[test]
fn an_append_to_another_note_leaves_the_open_one_alone() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "open\n");
    sandbox.note("b.md", "other\n");
    std::fs::create_dir_all(sandbox.workspace().join(APP_SUBDIR)).unwrap();
    std::fs::write(
        sandbox.workspace().join(APP_SUBDIR).join("order"),
        "a.md\nb.md\n",
    )
    .unwrap();

    let (mut app, events) = open_tui(&sandbox);
    assert_eq!(app.editor().text(), "open\n");
    type_text(&mut app, "unsaved ");

    // The manifest puts a.md on top, so the CLI appends there — point it at b.md by
    // reordering first, which is what the user pressing alt-down would have done.
    std::fs::write(
        sandbox.workspace().join(APP_SUBDIR).join("order"),
        "b.md\na.md\n",
    )
    .unwrap();

    sandbox.run(
        &["--workspace", sandbox.workspace().to_str().unwrap()],
        b"elsewhere\n",
    );
    drain(&mut app, &events);

    assert_eq!(sandbox.read("b.md"), "other\nelsewhere\n");
    assert_eq!(app.editor().text(), "unsaved open\n");
    assert_eq!(
        app.conflict(),
        None,
        "another note's change raised a prompt"
    );
}
