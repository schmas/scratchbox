//! The CLI, driven as a real process.
//!
//! Spawned rather than called, because most of what matters here is process behaviour:
//! exit codes, what lands on stderr, and whether stdin is read at all.

mod support;

use std::fs;
use std::process::{Command, Stdio};

use support::{Sandbox, code, names_in, stderr};
use tempfile::TempDir;

// --- appending -------------------------------------------------------------------------

#[test]
fn input_is_appended_to_the_note_at_the_top_of_the_order() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "first\n");
    sandbox.note("b.md", "other\n");
    // The manifest decides which note is active, not the modification time.
    fs::create_dir_all(sandbox.workspace().join(".scratchbox")).unwrap();
    fs::write(
        sandbox.workspace().join(".scratchbox/order"),
        "b.md\na.md\n",
    )
    .unwrap();

    let output = sandbox.run(&[], b"appended\n");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(sandbox.read("b.md"), "other\nappended\n");
    assert_eq!(
        sandbox.read("a.md"),
        "first\n",
        "the wrong note was written"
    );
}

#[test]
fn a_note_not_ending_in_a_newline_does_not_get_its_last_line_joined() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "no trailing newline");

    let output = sandbox.run(&[], b"appended\n");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(sandbox.read("a.md"), "no trailing newline\nappended\n");
}

#[test]
fn multi_line_input_lands_verbatim_including_its_whitespace() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "head\n");

    let output = sandbox.run(&[], b"  indented  \n\n\ttabbed\nno trailing newline");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        sandbox.read("a.md"),
        "head\n  indented  \n\n\ttabbed\nno trailing newline"
    );
}

#[test]
fn an_empty_workspace_gets_a_note_created_for_it() {
    let sandbox = Sandbox::new();
    assert!(sandbox.note_names().is_empty());

    let output = sandbox.run(&[], b"first thought\n");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let names = sandbox.note_names();
    assert_eq!(names.len(), 1, "expected one note, got {names:?}");
    assert!(names[0].ends_with(".md"));
    assert_eq!(sandbox.read(&names[0]), "first thought\n");
}

/// Writing nothing would still bump the note's modification time, which a running TUI's
/// watcher would report as a change that never happened.
#[test]
fn empty_input_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "untouched\n");
    let before = fs::metadata(sandbox.workspace().join("a.md"))
        .unwrap()
        .modified()
        .unwrap();

    let output = sandbox.run(&[], b"");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(sandbox.read("a.md"), "untouched\n");
    assert_eq!(
        fs::metadata(sandbox.workspace().join("a.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before,
        "the note was rewritten with the same contents"
    );
}

#[test]
fn invalid_utf8_is_refused_without_touching_the_note() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "untouched\n");

    let output = sandbox.run(&[], &[0x68, 0x69, 0xff, 0xfe]);

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("not valid UTF-8"),
        "unhelpful error: {}",
        stderr(&output)
    );
    assert_eq!(sandbox.read("a.md"), "untouched\n");
}

#[test]
fn the_workspace_flag_overrides_the_configured_one() {
    let sandbox = Sandbox::new();
    sandbox.note("configured.md", "configured\n");
    let elsewhere = sandbox.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("other.md"), "other\n").unwrap();

    let output = sandbox.run(&["--workspace", elsewhere.to_str().unwrap()], b"appended\n");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(elsewhere.join("other.md")).unwrap(),
        "other\nappended\n"
    );
    assert_eq!(sandbox.read("configured.md"), "configured\n");
}

/// The CLI is request/response. A watcher would cost startup and buy nothing, and the
/// order manifest is only rewritten by operations this one never performs.
#[test]
fn appending_leaves_the_app_directory_alone() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "first\n");
    let app_dir = sandbox.workspace().join(".scratchbox");
    fs::create_dir_all(&app_dir).unwrap();
    fs::write(app_dir.join("order"), "a.md\n").unwrap();
    let before = fs::read_to_string(app_dir.join("order")).unwrap();

    let output = sandbox.run(&[], b"appended\n");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(fs::read_to_string(app_dir.join("order")).unwrap(), before);
    assert_eq!(
        names_in(&app_dir),
        ["order"],
        "the CLI left something behind in the app directory"
    );
}

// --- argument handling -----------------------------------------------------------------

#[test]
fn help_and_version_succeed_and_say_something() {
    let sandbox = Sandbox::new();

    for flag in ["--help", "-h"] {
        let output = sandbox.run(&[flag], b"");
        assert_eq!(code(&output), 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("usage: scratchbox"));
    }

    let output = sandbox.run(&["--version"], b"");
    assert_eq!(code(&output), 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("scratchbox "));
}

#[test]
fn a_bad_argument_exits_with_the_usage_code() {
    let sandbox = Sandbox::new();

    for args in [
        vec!["--nonsense"],
        vec!["--workspace"],
        // `--yes` on its own reads like a confirmation of an append, which has no prompt.
        vec!["--yes"],
    ] {
        let output = sandbox.run(&args, b"anything\n");
        assert_eq!(code(&output), 2, "{args:?} did not exit 2");
        assert!(stderr(&output).contains("usage: scratchbox"));
    }
}

// --- purging the trash -----------------------------------------------------------------

#[test]
fn purge_empties_the_trash_and_leaves_every_note_alone() {
    let sandbox = Sandbox::new();
    sandbox.note("live.md", "still here\n");
    fs::create_dir_all(sandbox.trash()).unwrap();
    fs::write(sandbox.trash().join("deleted.md"), "gone\n").unwrap();
    fs::create_dir_all(sandbox.trash().join("a-directory")).unwrap();

    let output = sandbox.run(&["--purge-trash", "--yes"], b"");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        names_in(&sandbox.trash()).is_empty(),
        "the trash still has entries"
    );
    assert_eq!(sandbox.read("live.md"), "still here\n");
    assert_eq!(sandbox.note_names(), ["live.md"]);
}

#[test]
fn purging_an_empty_trash_is_a_no_op_that_succeeds() {
    let sandbox = Sandbox::new();

    let output = sandbox.run(&["--purge-trash", "--yes"], b"");

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("removed 0"));
}

/// The failure this guard exists for: a trash pointed inside the workspace turns "empty the
/// trash" into "delete every note".
#[test]
fn purge_refuses_when_the_trash_sits_inside_the_workspace() {
    let sandbox = Sandbox::with_trash("notes/.trash");
    sandbox.note("live.md", "still here\n");

    let output = sandbox.run(&["--purge-trash", "--yes"], b"");

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("refusing to purge"),
        "unexpected error: {}",
        stderr(&output)
    );
    assert_eq!(sandbox.read("live.md"), "still here\n");
}

/// The same guard the other way round, which is the more destructive of the two.
#[test]
fn purge_refuses_when_the_workspace_sits_inside_the_trash() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let trash = root.join("trash");
    let workspace = trash.join("notes");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(root.join("config/scratchbox")).unwrap();
    fs::write(
        root.join("config/scratchbox/config.toml"),
        format!(
            "workspace = {:?}\ntrash = {:?}\n",
            workspace.display().to_string(),
            trash.display().to_string()
        ),
    )
    .unwrap();
    fs::write(workspace.join("live.md"), "still here\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_scratchbox"))
        .args(["--purge-trash", "--yes"])
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code().unwrap(), 1);
    assert_eq!(
        fs::read_to_string(workspace.join("live.md")).unwrap(),
        "still here\n"
    );
}

/// Nobody to ask means the answer is no. Guessing on a script's behalf is how a scheduled
/// job quietly deletes something.
#[test]
fn purge_without_a_terminal_needs_the_yes_flag() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.trash()).unwrap();
    fs::write(sandbox.trash().join("deleted.md"), "gone\n").unwrap();

    let output = sandbox.run(&["--purge-trash"], b"");

    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("--yes"),
        "the error should say how to proceed: {}",
        stderr(&output)
    );
    assert_eq!(names_in(&sandbox.trash()), ["deleted.md"]);
}
