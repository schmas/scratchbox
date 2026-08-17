//! Watcher and self-write suppression, against a real filesystem.
//!
//! Its own test target so it can be quarantined without taking the unit tests with it.

mod support;

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use crossbeam_channel::Receiver;
use scratchbox_core::watcher::DEBOUNCE_WINDOW;
use scratchbox_core::{FolderSync, Format, NoteId, Store, StoreEvent};
use support::{collect, expect_silence, settle, wait_for};
use tempfile::TempDir;

struct Watched {
    _tmp: TempDir,
    store: FolderSync,
    events: Receiver<StoreEvent>,
    workspace: PathBuf,
}

fn watched() -> Watched {
    // Every test in this binary comes through here, and `init_for_tests` is a no-op after the
    // first call. This suite is the reason the diagnostics exist: CI repeats it twenty times
    // per OS because an intermittent pass is a race rather than a flake, and a red run used to
    // leave no record of the interleaving that caused it. Off unless `$RUST_LOG` names a
    // `scratchbox` target *and* `$SCRATCHBOX_LOG_DIR` says where to write.
    scratchbox_log::init_for_tests();

    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let mut store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();
    let events = store.subscribe();
    store.start_watching().unwrap();
    Watched {
        _tmp: tmp,
        store,
        events,
        workspace,
    }
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).unwrap()
}

fn named(event: &StoreEvent, name: &str) -> bool {
    match event {
        StoreEvent::Created(id) | StoreEvent::Modified(id) | StoreEvent::Removed(id) => {
            id.as_str() == name
        }
        StoreEvent::Renamed { to, .. } => to.as_str() == name,
        StoreEvent::Rescan => false,
    }
}

#[test]
fn an_external_create_is_reported() {
    let w = watched();

    fs::write(w.workspace.join("outside.md"), "made elsewhere").unwrap();

    let event = wait_for(&w.events, |e| named(e, "outside.md"));
    assert!(
        matches!(
            event,
            Some(StoreEvent::Created(_)) | Some(StoreEvent::Modified(_))
        ),
        "expected the new note to be reported, got {event:?}"
    );
}

#[test]
fn an_external_modify_is_reported() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "first").unwrap();
    settle(&w.events);

    fs::write(w.workspace.join("note.md"), "second").unwrap();

    assert!(
        wait_for(&w.events, |e| named(e, "note.md")).is_some(),
        "an edit made outside the app was never reported"
    );
}

#[test]
fn an_external_delete_is_reported() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "body").unwrap();
    settle(&w.events);

    fs::remove_file(w.workspace.join("note.md")).unwrap();

    let event = wait_for(
        &w.events,
        |e| matches!(e, StoreEvent::Removed(id) if id.as_str() == "note.md"),
    );
    assert!(event.is_some(), "an external delete was never reported");
}

#[test]
fn an_external_rename_is_reported() {
    let w = watched();
    fs::write(w.workspace.join("before.md"), "body").unwrap();
    settle(&w.events);

    fs::rename(w.workspace.join("before.md"), w.workspace.join("after.md")).unwrap();

    let event = wait_for(&w.events, |e| named(e, "after.md"));
    assert!(
        matches!(
            event,
            Some(StoreEvent::Renamed { .. }) | Some(StoreEvent::Created(_))
        ),
        "expected the rename destination to be reported, got {event:?}"
    );
}

/// vim and VS Code save by writing a temp file and renaming it over the note. The user did
/// not create a new note — they edited an existing one, and that is what has to be reported.
#[test]
fn an_editor_saving_through_a_temp_file_reads_as_a_modification() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "first").unwrap();
    settle(&w.events);

    let temp = w.workspace.join(".note.md.swp");
    fs::write(&temp, "second").unwrap();
    fs::rename(&temp, w.workspace.join("note.md")).unwrap();

    assert!(
        wait_for(&w.events, |e| named(e, "note.md")).is_some(),
        "an editor's temp-file save was never reported"
    );
}

#[test]
fn our_own_write_produces_no_events() {
    let w = watched();
    let note = w.store.create(Format::Markdown).unwrap();
    settle(&w.events);

    w.store.write(&note, "typed by the user").unwrap();

    expect_silence(&w.events, "our own write");
}

#[test]
fn our_own_rename_produces_no_events() {
    let w = watched();
    fs::write(w.workspace.join("2026-08-15-1548.md"), "body").unwrap();
    settle(&w.events);

    w.store
        .rename(&id("2026-08-15-1548.md"), &id("2026-08-15-1548-titled.md"))
        .unwrap();

    expect_silence(&w.events, "our own rename");
}

/// Phase 7 saves and then renames in one breath; neither half may echo.
#[test]
fn our_own_write_then_rename_produces_no_events() {
    let w = watched();
    let note = w.store.create(Format::Markdown).unwrap();
    settle(&w.events);

    w.store.write(&note, "# A Title\n\nbody").unwrap();
    w.store
        .rename(&note, &id("2026-08-15-1548-a-title.md"))
        .unwrap();

    expect_silence(&w.events, "our own write followed by our own rename");
}

/// The trash lives outside the workspace, so the note genuinely leaves the watched tree
/// and no trash-side create can occur.
#[test]
fn our_own_delete_produces_exactly_one_removal() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "body").unwrap();
    settle(&w.events);

    w.store.delete(&id("note.md")).unwrap();

    let events = collect(&w.events);
    assert_eq!(
        events,
        vec![StoreEvent::Removed(id("note.md"))],
        "expected exactly one removal"
    );
}

#[test]
fn a_burst_of_external_writes_collapses_into_one_event() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "0").unwrap();
    settle(&w.events);

    let started = Instant::now();
    for n in 0..10 {
        fs::write(w.workspace.join("note.md"), format!("{n}")).unwrap();
    }
    let burst = started.elapsed();

    let events = collect(&w.events);

    // Coalescing is bounded by the debounce window, not by the burst: if a loaded machine
    // stretches the writes past 500ms, reporting once per window is the debouncer working
    // correctly. Deriving the budget from the measured burst tests the actual guarantee
    // rather than assuming the writes were instantaneous.
    let allowed = 1 + burst.as_millis() / DEBOUNCE_WINDOW.as_millis();
    assert!(!events.is_empty(), "the burst was never reported at all");
    assert!(
        events.len() as u128 <= allowed,
        "ten writes spanning {burst:?} should collapse to at most {allowed} event(s), got {events:?}"
    );
}

/// One change, reported twice by the platform, must still reach the user once.
///
/// The kind is deliberately not asserted: macOS reports an edit to an existing note as
/// `Created`, Linux as `Modified`, and callers are documented to treat them alike.
#[test]
fn a_single_external_write_is_reported_once() {
    let w = watched();
    fs::write(w.workspace.join("note.md"), "first").unwrap();
    settle(&w.events);

    fs::write(w.workspace.join("note.md"), "second").unwrap();

    let events = collect(&w.events);
    assert_eq!(
        events.len(),
        1,
        "one edit should be reported once, got {events:?}"
    );
    assert!(
        matches!(
            events[0],
            StoreEvent::Created(ref got) | StoreEvent::Modified(ref got) if got == &id("note.md")
        ),
        "expected the edited note to be reported, got {events:?}"
    );
}

/// The point of fingerprinting rather than path-matching: somebody else's edit that lands
/// inside our suppression window must still get through.
#[test]
fn an_external_write_during_our_own_window_is_not_suppressed() {
    let w = watched();
    let note = w.store.create(Format::Markdown).unwrap();
    settle(&w.events);

    w.store.write(&note, "what we wrote").unwrap();
    // Immediately overwritten from outside, well inside the 2s TTL.
    fs::write(w.workspace.join(note.as_str()), "what somebody else wrote").unwrap();

    assert!(
        wait_for(&w.events, |e| named(e, note.as_str())).is_some(),
        "an external edit inside our suppression window was swallowed"
    );
}

/// Suppression reads the file to fingerprint it, and that read can fail. RT-8's rule is
/// that an unreadable file never suppresses and never panics.
///
/// Only the watcher's survival is asserted here. Whether the vanishing itself produces an
/// event is up to the platform: inotify's debouncer cancels a create and a delete that fall
/// inside one window, so on Linux this sequence legitimately reports nothing. That the
/// event is not *suppressed* is proven deterministically in the `suppress` unit tests,
/// where no debouncer sits in the way.
#[test]
fn a_registration_whose_file_vanishes_does_not_kill_the_watcher() {
    let w = watched();
    let note = w.store.create(Format::Markdown).unwrap();
    settle(&w.events);

    w.store.write(&note, "body").unwrap();
    // Gone before the debounced event arrives, so the fingerprint read fails.
    fs::remove_file(w.workspace.join(note.as_str())).unwrap();

    // The watcher thread is still alive and still delivering.
    fs::write(w.workspace.join("later.md"), "still working").unwrap();
    assert!(
        wait_for(&w.events, |e| named(e, "later.md")).is_some(),
        "the watcher stopped delivering after a failed fingerprint read"
    );
}

#[test]
fn writes_to_the_app_directory_never_reach_the_event_stream() {
    let w = watched();

    // Phase 5 rewrites this manifest constantly; it is not a note.
    fs::write(w.workspace.join(".scratchbox/order").as_path(), "a\nb\nc\n").unwrap();

    expect_silence(&w.events, "a write to the app directory");
}

#[test]
fn the_registry_empties_itself_when_nothing_arrives() {
    let w = watched();
    let note = w.store.create(Format::Markdown).unwrap();

    w.store.write(&note, "body").unwrap();
    assert!(!w.store.suppressor().is_empty());

    // Past the TTL, with no events to consume the entries.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline && !w.store.suppressor().is_empty() {
        std::thread::yield_now();
    }

    assert!(
        w.store.suppressor().is_empty(),
        "registry entries outlived their TTL; memory would grow with session length"
    );
}
