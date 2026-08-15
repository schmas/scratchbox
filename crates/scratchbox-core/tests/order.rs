//! Order manifest: reconciliation, persistence, and the repairs that keep a user's
//! chosen ordering from being rewritten by a crash.

mod support;

use std::fs;
use std::time::{Duration, SystemTime};

use scratchbox_core::note::NoteMeta;
use scratchbox_core::order::{self, OrderStore};
use scratchbox_core::{FolderSync, NoteId, Store, reconcile};
use support::{collect, expect_silence};
use tempfile::TempDir;

fn id(name: &str) -> NoteId {
    NoteId::new(name).expect("valid note name")
}

/// A note on disk, `age` seconds old. Larger `age` means older.
fn on_disk(name: &str, age: u64) -> NoteMeta {
    NoteMeta::from_id(
        id(name),
        SystemTime::UNIX_EPOCH + Duration::from_secs(10_000 - age),
    )
}

fn lines(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn names(order: &[NoteId]) -> Vec<&str> {
    order.iter().map(NoteId::as_str).collect()
}

#[test]
fn remembered_order_is_preserved() {
    let manifest = lines(&["b.md", "a.md", "c.md"]);
    let disk = [on_disk("a.md", 1), on_disk("b.md", 2), on_disk("c.md", 3)];

    assert_eq!(
        names(&reconcile(&manifest, &disk)),
        ["b.md", "a.md", "c.md"]
    );
}

#[test]
fn new_files_land_on_top_newest_first() {
    let manifest = lines(&["old.md"]);
    let disk = [
        on_disk("old.md", 100),
        on_disk("older-new.md", 20),
        on_disk("newest.md", 1),
    ];

    assert_eq!(
        names(&reconcile(&manifest, &disk)),
        ["newest.md", "older-new.md", "old.md"]
    );
}

#[test]
fn an_entry_whose_file_is_gone_is_pruned() {
    let manifest = lines(&["a.md", "deleted.md", "b.md"]);
    let disk = [on_disk("a.md", 1), on_disk("b.md", 2)];

    assert_eq!(names(&reconcile(&manifest, &disk)), ["a.md", "b.md"]);
}

#[test]
fn a_missing_or_empty_manifest_falls_back_to_newest_first() {
    let disk = [
        on_disk("middle.md", 50),
        on_disk("oldest.md", 99),
        on_disk("newest.md", 1),
    ];

    assert_eq!(
        names(&reconcile(&[], &disk)),
        ["newest.md", "middle.md", "oldest.md"]
    );
}

#[test]
fn a_hand_mangled_manifest_never_loses_a_note() {
    let manifest = lines(&[
        "",
        "   ",
        "a.md",
        "\u{0}garbage",
        "a.md", // duplicated by a careless edit
        "not a real note.md",
        "b.md",
    ]);
    let disk = [on_disk("a.md", 2), on_disk("b.md", 1)];

    // The duplicate is ignored rather than listing the note twice, and the junk lines name
    // nothing on disk, so they simply do not appear.
    assert_eq!(names(&reconcile(&manifest, &disk)), ["a.md", "b.md"]);
}

/// The traversal path RT-1 closed: a manifest line naming something outside the workspace
/// must not become a listed, selectable, deletable note.
#[test]
fn a_manifest_line_pointing_outside_the_workspace_is_dropped() {
    let manifest = lines(&[
        "../../.ssh/id_rsa",
        "/etc/passwd",
        "..",
        ".hidden",
        "real.md",
    ]);
    let disk = [on_disk("real.md", 1)];

    assert_eq!(names(&reconcile(&manifest, &disk)), ["real.md"]);
}

/// A crash between the rename and the manifest write leaves exactly this state. Without
/// repair the note is treated as new and jumps to the top, rewriting the user's ordering.
#[test]
fn a_note_renamed_since_the_last_save_keeps_its_place() {
    let manifest = lines(&["a.md", "b.md", "2026-08-15-1548.md", "c.md"]);
    let disk = [
        on_disk("a.md", 4),
        on_disk("b.md", 3),
        // Renamed on its first save with content, before the manifest caught up.
        on_disk("2026-08-15-1548-my-note.md", 1),
        on_disk("c.md", 2),
    ];

    let order = reconcile(&manifest, &disk);

    assert_eq!(
        names(&order),
        ["a.md", "b.md", "2026-08-15-1548-my-note.md", "c.md"],
        "the repaired note should hold its middle position, not jump to the top"
    );
    assert_eq!(order[2].as_str(), "2026-08-15-1548-my-note.md");
}

#[test]
fn an_ambiguous_rename_is_not_guessed_at() {
    let manifest = lines(&["a.md", "2026-08-15-1548.md"]);
    let disk = [
        on_disk("a.md", 9),
        on_disk("2026-08-15-1548-one.md", 2),
        on_disk("2026-08-15-1548-two.md", 1),
    ];

    let order = reconcile(&manifest, &disk);

    // Two candidates share the prefix, so neither is claimed: both are new, and the stale
    // entry is pruned. Misattributing one to the other would be worse than losing a place.
    assert_eq!(
        names(&order),
        ["2026-08-15-1548-two.md", "2026-08-15-1548-one.md", "a.md"]
    );
}

#[test]
fn a_rename_across_extensions_is_not_treated_as_a_repair() {
    let manifest = lines(&["2026-08-15-1548.md"]);
    let disk = [on_disk("2026-08-15-1548-note.ts", 1)];

    // Same timestamp, different format: a coincidence, not the same note.
    assert_eq!(
        names(&reconcile(&manifest, &disk)),
        ["2026-08-15-1548-note.ts"]
    );
}

#[test]
fn moving_a_note_stops_at_the_ends() {
    let mut order = vec![id("a.md"), id("b.md"), id("c.md")];

    order::move_up(&mut order, 0);
    assert_eq!(names(&order), ["a.md", "b.md", "c.md"], "top cannot go up");

    order::move_down(&mut order, 2);
    assert_eq!(
        names(&order),
        ["a.md", "b.md", "c.md"],
        "bottom cannot go down"
    );

    // Out of range on purpose: a stale index must not panic.
    order::move_up(&mut order, 99);
    order::move_down(&mut order, 99);
    assert_eq!(names(&order), ["a.md", "b.md", "c.md"]);
}

#[test]
fn moving_a_note_swaps_it_with_its_neighbour() {
    let mut order = vec![id("a.md"), id("b.md"), id("c.md")];

    order::move_up(&mut order, 2);
    assert_eq!(names(&order), ["a.md", "c.md", "b.md"]);

    order::move_down(&mut order, 0);
    assert_eq!(names(&order), ["c.md", "a.md", "b.md"]);
}

#[test]
fn renaming_an_entry_keeps_its_index() {
    let mut order = vec![id("a.md"), id("b.md"), id("c.md"), id("d.md")];

    order::rename_entry(&mut order, &id("d.md"), &id("d-titled.md"));

    assert_eq!(names(&order), ["a.md", "b.md", "c.md", "d-titled.md"]);
}

#[test]
fn a_manifest_round_trips_through_disk() {
    let tmp = TempDir::new().unwrap();
    let store = OrderStore::new(tmp.path());
    let order = vec![id("b.md"), id("a.md")];

    store.save(&order).unwrap();

    assert_eq!(store.load(), lines(&["b.md", "a.md"]));
    assert_eq!(
        fs::read_to_string(store.path()).unwrap(),
        "b.md\na.md\n",
        "the manifest should stay greppable, one name per line"
    );
}

#[test]
fn a_binary_manifest_reads_as_no_manifest_at_all() {
    let tmp = TempDir::new().unwrap();
    let store = OrderStore::new(tmp.path());
    fs::write(store.path(), [0xff, 0xfe, 0x00, 0x9f]).unwrap();

    assert!(store.load().is_empty(), "invalid UTF-8 is a cache miss");
}

#[test]
fn ten_thousand_blank_lines_are_harmless() {
    let tmp = TempDir::new().unwrap();
    let store = OrderStore::new(tmp.path());
    fs::write(store.path(), "\n".repeat(10_000)).unwrap();

    let disk = [on_disk("a.md", 1)];
    assert_eq!(names(&reconcile(&store.load(), &disk)), ["a.md"]);
}

#[test]
fn a_missing_manifest_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let store = OrderStore::new(&tmp.path().join("never-created"));

    assert!(store.load().is_empty());
}

#[test]
fn renaming_a_note_moves_its_manifest_entry_in_place() {
    let tmp = TempDir::new().unwrap();
    let store = FolderSync::new(tmp.path().join("notes"), tmp.path().join("trash")).unwrap();
    fs::write(store.workspace().join("2026-08-15-1548.md"), "body").unwrap();
    store
        .order()
        .save(&[id("a.md"), id("2026-08-15-1548.md"), id("c.md")])
        .unwrap();

    store
        .rename(&id("2026-08-15-1548.md"), &id("2026-08-15-1548-titled.md"))
        .unwrap();

    assert_eq!(
        store.order().load(),
        lines(&["a.md", "2026-08-15-1548-titled.md", "c.md"])
    );
}

#[test]
fn deleting_a_note_drops_its_manifest_entry() {
    let tmp = TempDir::new().unwrap();
    let store = FolderSync::new(tmp.path().join("notes"), tmp.path().join("trash")).unwrap();
    fs::write(store.workspace().join("gone.md"), "body").unwrap();
    store
        .order()
        .save(&[id("a.md"), id("gone.md"), id("c.md")])
        .unwrap();

    store.delete(&id("gone.md")).unwrap();

    assert_eq!(store.order().load(), lines(&["a.md", "c.md"]));
}

/// A manifest write that reached the watcher would re-list, which would rewrite the
/// manifest, which would wake the watcher again. The app directory is excluded for exactly
/// this reason, and it is worth an explicit test because the failure mode is a spin.
#[test]
fn writing_the_manifest_produces_no_events() {
    let tmp = TempDir::new().unwrap();
    let mut store = FolderSync::new(tmp.path().join("notes"), tmp.path().join("trash")).unwrap();
    let events = store.subscribe();
    store.start_watching().unwrap();
    collect(&events);

    for _ in 0..5 {
        store.order().save(&[id("a.md"), id("b.md")]).unwrap();
    }

    expect_silence(&events, "writing the order manifest");
}
