//! `FolderSync` against real directories.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use scratchbox_core::{FolderSync, Format, NoteId, Store, WorkspaceHealth};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    store: FolderSync,
    workspace: PathBuf,
    trash: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let trash = tmp.path().join("trash");
    let store = FolderSync::new(workspace.clone(), trash.clone()).unwrap();
    Fixture {
        _tmp: tmp,
        store,
        workspace,
        trash,
    }
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).expect("valid note name")
}

fn listed_names(store: &FolderSync) -> BTreeSet<String> {
    store
        .list()
        .unwrap()
        .into_iter()
        .map(|note| note.id.as_str().to_owned())
        .collect()
}

fn entry_names(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn list_returns_notes_and_nothing_else() {
    let f = fixture();
    fs::write(f.workspace.join("2026-08-15-1548.md"), "one").unwrap();
    fs::write(f.workspace.join("2026-08-15-1549-note.ts"), "two").unwrap();
    // Cloud sidecars and our own app dir are hidden, so they are not notes.
    fs::write(f.workspace.join(".DS_Store"), "junk").unwrap();
    fs::write(f.workspace.join(".scratchbox/order"), "manifest").unwrap();
    fs::create_dir(f.workspace.join("subdir")).unwrap();

    assert_eq!(
        listed_names(&f.store),
        BTreeSet::from([
            "2026-08-15-1548.md".to_owned(),
            "2026-08-15-1549-note.ts".to_owned(),
        ])
    );
}

#[test]
fn conflict_copies_are_ordinary_notes() {
    let f = fixture();
    fs::write(f.workspace.join("note (1).md"), "copy").unwrap();
    fs::write(f.workspace.join("note (conflicted copy).md"), "copy").unwrap();

    assert_eq!(
        listed_names(&f.store),
        BTreeSet::from([
            "note (1).md".to_owned(),
            "note (conflicted copy).md".to_owned(),
        ])
    );
}

#[test]
fn read_and_write_round_trip() {
    let f = fixture();
    let note = f.store.create(Format::Markdown).unwrap();

    f.store.write(&note, "hello\nworld").unwrap();

    assert_eq!(f.store.read(&note).unwrap(), "hello\nworld");
}

#[test]
fn a_reader_never_sees_a_half_written_note() {
    let f = fixture();
    let note = f.store.create(Format::Markdown).unwrap();
    let short = "s".repeat(4 * 1024);
    let long = "l".repeat(512 * 1024);
    f.store.write(&note, &short).unwrap();

    let path = f.workspace.join(note.as_str());
    let stop = Arc::new(AtomicBool::new(false));

    let reader = {
        let (path, stop) = (path.clone(), Arc::clone(&stop));
        thread::spawn(move || {
            let mut reads = 0;
            while !stop.load(Ordering::Relaxed) {
                // A torn write would show up as a body that is neither size.
                if let Ok(seen) = fs::read_to_string(&path) {
                    assert!(
                        seen.len() == 4 * 1024 || seen.len() == 512 * 1024,
                        "observed a truncated note of {} bytes",
                        seen.len()
                    );
                    reads += 1;
                }
            }
            reads
        })
    };

    for _ in 0..40 {
        f.store.write(&note, &long).unwrap();
        f.store.write(&note, &short).unwrap();
    }
    stop.store(true, Ordering::Relaxed);

    let reads = reader.join().unwrap();
    assert!(reads > 0, "reader never managed to read the note");
}

#[test]
fn two_notes_created_in_the_same_minute_are_distinct() {
    let f = fixture();

    let first = f.store.create(Format::Markdown).unwrap();
    let second = f.store.create(Format::Markdown).unwrap();

    assert_ne!(first, second);
    assert_eq!(listed_names(&f.store).len(), 2);
    assert!(second.as_str().contains("-2."), "unexpected name {second}");
}

#[test]
fn rename_reports_the_name_that_landed() {
    let f = fixture();
    fs::write(f.workspace.join("2026-08-15-1548.md"), "body").unwrap();
    fs::write(f.workspace.join("2026-08-15-1548-taken.md"), "other").unwrap();

    let renamed = f
        .store
        .rename(&id("2026-08-15-1548.md"), &id("2026-08-15-1548-taken.md"))
        .unwrap();

    // The requested name was occupied, so the caller is told what it actually got.
    assert_eq!(renamed.as_str(), "2026-08-15-1548-taken-2.md");
    assert_eq!(f.store.read(&renamed).unwrap(), "body");
    assert_eq!(
        f.store.read(&id("2026-08-15-1548-taken.md")).unwrap(),
        "other"
    );
}

#[test]
fn renaming_a_note_to_its_own_name_is_a_no_op() {
    let f = fixture();
    let note = f.store.create(Format::Markdown).unwrap();
    f.store.write(&note, "body").unwrap();

    let renamed = f.store.rename(&note, &note).unwrap();

    assert_eq!(renamed, note);
    assert_eq!(listed_names(&f.store).len(), 1);
}

#[test]
fn delete_moves_the_note_out_of_the_workspace_and_into_the_trash() {
    let f = fixture();
    fs::write(f.workspace.join("2026-08-15-1548.md"), "secrets").unwrap();

    f.store.delete(&id("2026-08-15-1548.md")).unwrap();

    assert!(listed_names(&f.store).is_empty());
    assert_eq!(
        fs::read_to_string(f.trash.join("2026-08-15-1548.md")).unwrap(),
        "secrets"
    );
}

#[test]
fn two_notes_deleted_under_one_name_both_survive_in_the_trash() {
    let f = fixture();

    for body in ["first", "second"] {
        fs::write(f.workspace.join("note.md"), body).unwrap();
        f.store.delete(&id("note.md")).unwrap();
    }

    assert_eq!(
        entry_names(&f.trash),
        BTreeSet::from(["note.md".to_owned(), "note-2.md".to_owned()])
    );
    assert_eq!(
        fs::read_to_string(f.trash.join("note.md")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(f.trash.join("note-2.md")).unwrap(),
        "second"
    );
}

#[test]
fn purge_trash_empties_the_trash_and_leaves_the_workspace_alone() {
    let f = fixture();
    fs::write(f.workspace.join("gone.md"), "x").unwrap();
    fs::write(f.workspace.join("kept.md"), "y").unwrap();
    f.store.delete(&id("gone.md")).unwrap();

    let removed = f.store.purge_trash().unwrap();

    assert_eq!(removed, 1);
    assert!(entry_names(&f.trash).is_empty());
    assert_eq!(
        listed_names(&f.store),
        BTreeSet::from(["kept.md".to_owned()])
    );
}

#[test]
fn a_non_utf8_note_is_listed_but_not_readable() {
    let f = fixture();
    fs::write(f.workspace.join("binary.txt"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

    assert_eq!(
        listed_names(&f.store),
        BTreeSet::from(["binary.txt".to_owned()])
    );
    // Surfaced as a typed error rather than lossily converted or panicking.
    assert!(f.store.read(&id("binary.txt")).is_err());
}

#[test]
fn health_reports_a_workspace_that_disappeared() {
    let f = fixture();
    assert_eq!(f.store.health(), WorkspaceHealth::Ok);

    fs::remove_dir_all(&f.workspace).unwrap();

    assert_eq!(f.store.health(), WorkspaceHealth::Missing);
}

#[test]
fn the_store_is_object_safe() {
    let f = fixture();
    let store: Box<dyn Store> =
        Box::new(FolderSync::new(f.workspace.clone(), f.trash.clone()).unwrap());

    assert!(store.list().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn notes_are_owner_only_however_they_were_made() {
    use std::os::unix::fs::PermissionsExt;

    let f = fixture();
    let created = f.store.create(Format::Markdown).unwrap();
    let written = id("written.md");
    f.store.write(&written, "body").unwrap();

    for note in [&created, &written] {
        let mode = fs::metadata(f.workspace.join(note.as_str()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{note} should be owner-only, was {mode:o}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_pointing_outside_the_workspace_cannot_be_written_through() {
    let f = fixture();
    let outside = f._tmp.path().join("outside.md");
    fs::write(&outside, "original").unwrap();
    std::os::unix::fs::symlink(&outside, f.workspace.join("escape.md")).unwrap();

    let escape = id("escape.md");

    assert!(f.store.write(&escape, "overwritten").is_err());
    assert!(f.store.read(&escape).is_err());
    assert!(f.store.delete(&escape).is_err());
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "original",
        "a file outside the workspace was reachable through a symlink"
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_refused_rather_than_created() {
    let f = fixture();
    let missing = f._tmp.path().join("nowhere.md");
    std::os::unix::fs::symlink(&missing, f.workspace.join("dangling.md")).unwrap();

    assert!(f.store.write(&id("dangling.md"), "created?").is_err());
    assert!(
        !missing.exists(),
        "writing through a dangling link created its target"
    );
}

/// Real cross-device delete, when the machine happens to offer a second filesystem.
///
/// Linux CI has tmpfs at `/dev/shm`; macOS usually has one volume, so this reports a skip
/// instead of pretending to cover the case. The mechanics are covered unconditionally by
/// the unit tests next to `copy_then_unlink`.
#[cfg(unix)]
#[test]
fn delete_across_filesystems_moves_the_note() {
    use std::os::unix::fs::MetadataExt;

    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    fs::create_dir_all(&workspace).unwrap();

    let shm = Path::new("/dev/shm");
    if !shm.is_dir() || fs::metadata(shm).unwrap().dev() == fs::metadata(&workspace).unwrap().dev()
    {
        println!("skipped: no second filesystem available on this machine");
        return;
    }

    let trash = shm.join(format!("scratchbox-test-trash-{}", std::process::id()));
    let store = FolderSync::new(workspace.clone(), trash.clone()).unwrap();
    fs::write(workspace.join("note.md"), "secrets").unwrap();

    store.delete(&id("note.md")).unwrap();

    assert!(store.list().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(trash.join("note.md")).unwrap(),
        "secrets"
    );
    fs::remove_dir_all(&trash).unwrap();
}
