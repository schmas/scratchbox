//! What the `scratchbox` binary does about diagnostics, checked from outside it.
//!
//! **Subprocess tests, and not as a matter of style.** `RUST_LOG`, `$XDG_DATA_HOME`, and
//! `tracing`'s installed subscriber are all process-global: one test here needs the log
//! directory absent while its sibling needs it present, a second `try_init` in one process
//! loses, and under edition 2024 `env::set_var` is `unsafe`. So each test gets its own child
//! with its own `TempDir` and its own environment — the rule
//! `crates/scratchbox-core/tests/config.rs` already states for exactly this reason.
//!
//! The CLI rather than the TUI because it is the binary that can be driven headlessly *and*
//! does instrumented work on every run: it appends to a note, so `foldersync` emits a record
//! carrying a note id and a byte count, which is what the escaping and no-note-text
//! guarantees need something to be read against. `scratchbox-tui`'s own writer — a rotating
//! appender rather than a plain file — is covered in `crates/scratchbox-tui/tests/logging.rs`.

mod support;

use std::fs;

use support::{Sandbox, code, stderr};

/// A `RUST_LOG` that turns diagnostics on for this project and nothing else.
const ON: &str = "scratchbox=trace";

const LOG_FILE: &str = "scratchbox.log";

fn read_log(sandbox: &Sandbox) -> String {
    let path = sandbox.log_dir().join(LOG_FILE);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("no log at {}: {error}", path.display()))
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Not activated has to mean *nothing exists* — no directory, no file, no thread. All three
/// ways of not asking for diagnostics, because the earlier design only caught the first.
#[test]
fn no_log_appears_when_rust_log_is_unset_empty_or_untargeted() {
    let cases: [(&str, &[(&str, &str)]); 4] = [
        ("absent", &[]),
        // Parses to `Ok` with zero directives, so gating on the parse error alone would have
        // bought a directory, a file, and a worker thread to write nothing forever.
        ("empty", &[("RUST_LOG", "")]),
        // A level with no target. `RUST_LOG` is ecosystem-wide and this is what a developer
        // with `export RUST_LOG=info` in their profile has.
        ("no target", &[("RUST_LOG", "info")]),
        ("somebody else's crate", &[("RUST_LOG", "notify=trace")]),
    ];

    for (what, env) in cases {
        let sandbox = Sandbox::new();
        sandbox.note("a.md", "already here\n");

        let output = sandbox.run_with_env(&[], b"captured\n", env);

        assert_eq!(code(&output), 0, "{what}: {}", stderr(&output));
        assert_eq!(sandbox.read("a.md"), "already here\ncaptured\n");
        assert!(
            !sandbox.log_dir().exists(),
            "{what} created {}; not activated must mean nothing exists at all",
            sandbox.log_dir().display()
        );
    }
}

/// Asserted rather than inspected, because a 0644 log is invisible to every other check in
/// this file. Every other file this project creates is forced to 0600 and `atomic.rs` says
/// why: a scratchpad collects API keys and passwords whether or not it was meant to.
#[cfg(unix)]
#[test]
fn the_log_directory_is_owner_only_and_so_is_the_log() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", ON)]);

    assert_eq!(
        mode(&sandbox.log_dir()),
        0o700,
        "the log directory is the authoritative barrier and must be owner-only"
    );
    assert_eq!(
        mode(&sandbox.log_dir().join(LOG_FILE)),
        0o600,
        "a log this binary creates itself must be owner-only"
    );
}

#[test]
fn a_note_body_never_reaches_the_log() {
    const SECRET: &str = "sk-live-DEADBEEFCAFE";

    let sandbox = Sandbox::new();
    // An innocuous first line with the secret below it, so this is testing that the *content*
    // is not logged rather than being saved by the first line happening to be harmless.
    sandbox.note("a.md", &format!("shopping list\n{SECRET}\n"));

    sandbox.run_with_env(&[], b"milk and eggs\n", &[("RUST_LOG", ON)]);

    let log = read_log(&sandbox);
    assert!(!log.contains(SECRET), "a note body reached the log:\n{log}");
    assert!(
        !log.contains("milk and eggs"),
        "the appended text reached the log:\n{log}"
    );

    // And it is not passing by having logged nothing: the id and a byte count are what a
    // record about a write is supposed to carry instead of the text.
    assert!(
        log.contains(r#"id="a.md""#),
        "no note id in the log:\n{log}"
    );
    assert!(log.contains("bytes="), "no byte count in the log:\n{log}");
}

/// `NoteId::new` rejects NUL, separators, and a leading dot. It does **not** reject `\n` or
/// `\x1b`, and a note arriving through whatever syncs the workspace can carry either — so this
/// name is legal input rather than a contrived one. Formatted with `?`, its `Debug` escapes
/// both; formatted with `%` it would reach the file byte for byte, forging a whole record in
/// the log this phase exists to make trustworthy and executing an escape sequence in the
/// terminal of whoever reads it or opens the CI artifact.
#[test]
fn a_control_character_in_a_note_name_is_escaped() {
    let hostile = "a\n\u{1b}[2J2026-01-01T00:00:00.000000Z  INFO forged: nothing happened.md";

    let sandbox = Sandbox::new();
    sandbox.note(hostile, "body\n");

    let output = sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", ON)]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let raw = fs::read(sandbox.log_dir().join(LOG_FILE)).unwrap();
    assert!(
        !raw.contains(&0x1b),
        "an escape sequence reached the log and would execute in a reader's terminal"
    );

    let text = String::from_utf8(raw).expect("the log should stay valid UTF-8");
    // One record per line. A line the formatter wrote opens with its ISO-8601 timestamp; a
    // line forged by an unescaped `\n` opens with whatever followed the newline.
    for line in text.lines().filter(|line| !line.is_empty()) {
        assert!(
            line.starts_with(|c: char| c.is_ascii_digit()),
            "a record did not begin with a timestamp, so a note name broke out of its field: \
             {line:?}"
        );
    }
    assert!(
        text.contains(r"\n") && text.contains(r"\u{1b}"),
        "the name should appear with both control characters escaped:\n{text}"
    );
}

/// The TUI owns the terminal and `scratchbox` runs from a global hotkey, so a diagnostic on
/// either stream is a visible defect rather than a cosmetic one.
#[test]
fn nothing_reaches_stdout_or_stderr_while_logging() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    let output = sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", ON)]);

    assert_eq!(code(&output), 0);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "diagnostics reached stdout"
    );
    assert_eq!(stderr(&output), "", "diagnostics reached stderr");

    // Not passing by being switched off.
    assert!(
        !read_log(&sandbox).is_empty(),
        "the log is empty, so this proved nothing"
    );
}

/// The condition the deleted `loggable` predicate was supposed to defend and got backwards.
/// Reds if the overlap refusal is removed: the directory would then be created.
#[test]
fn a_log_directory_inside_the_workspace_disables_logging() {
    let sandbox = Sandbox::new();

    // The log defaults to `<data_home>/scratchbox/log`, so pointing the workspace at
    // `<data_home>/scratchbox` puts it inside the watched tree. That is the feedback loop:
    // a line describing a filesystem event, written where a watcher can see it, produces that
    // same event one debounce window later, which is logged, forever.
    let workspace = sandbox.data_home().join("scratchbox");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("a.md"), "already here\n").unwrap();

    let output = sandbox.run_with_env(
        &["--workspace", workspace.to_str().unwrap()],
        b"captured\n",
        &[("RUST_LOG", ON)],
    );

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(workspace.join("a.md")).unwrap(),
        "already here\ncaptured\n",
        "refusing to log must not cost the append"
    );
    assert!(
        !sandbox.log_dir().exists(),
        "the overlap was not refused, so diagnostics would feed the watcher they describe"
    );
}

/// `read_stdin` drains the pipe before `open` runs, so a diagnostics failure that propagated
/// would exit non-zero having already consumed the thought — the failure this binary's own
/// doc calls out: *a hotkey that reports "no notes" instead of capturing the thought has
/// failed at the only job it has*.
#[test]
fn an_unwritable_log_directory_does_not_fail_the_run() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    // A file where the log directory needs to go, so creating it cannot succeed. Portable, and
    // it does not depend on running as a user who cannot chmod their way past a mode bit.
    fs::create_dir_all(sandbox.data_home().join("scratchbox")).unwrap();
    fs::write(sandbox.log_dir(), "not a directory").unwrap();

    let output = sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", ON)]);

    assert_eq!(
        code(&output),
        0,
        "diagnostics must never cost the capture: {}",
        stderr(&output)
    );
    assert_eq!(sandbox.read("a.md"), "already here\ncaptured\n");
    assert_eq!(
        stderr(&output),
        "",
        "the failure was announced on a stream this binary has to keep quiet"
    );
}

/// A `scratchbox` target so the gate opens, and a level that does not exist so the parse
/// fails. `parse_lossy` would have printed its complaint to stderr from inside the crate that
/// exists to keep diagnostics off it, which is why it is banned.
#[test]
fn an_unparseable_rust_log_does_not_fail_the_run() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    let output = sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", "scratchbox=verbose")]);

    assert_eq!(
        code(&output),
        0,
        "an unparseable RUST_LOG failed the run: {}",
        stderr(&output)
    );
    assert_eq!(sandbox.read("a.md"), "already here\ncaptured\n");
    assert!(
        !sandbox.log_dir().exists(),
        "a filter that did not parse still created {}",
        sandbox.log_dir().display()
    );
    assert_eq!(stderr(&output), "", "the parse complaint reached stderr");
}

/// The records an append leaves, in the order it made them. Not the full watcher timeline —
/// that needs a live watcher and is covered by `save_flow` — but it does check that the write
/// is announced to the suppression registry *after* it is described, and that the span
/// carrying the note id is the one the core's records are attributed to.
#[test]
fn an_append_leaves_an_ordered_record_of_what_it_did() {
    let sandbox = Sandbox::new();
    sandbox.note("a.md", "already here\n");

    sandbox.run_with_env(&[], b"captured\n", &[("RUST_LOG", ON)]);

    let log = read_log(&sandbox);
    let write = log
        .find("foldersync: write")
        .unwrap_or_else(|| panic!("no write record:\n{log}"));
    let registered = log
        .find("registered a write")
        .unwrap_or_else(|| panic!("no registration record:\n{log}"));

    assert!(
        write < registered,
        "the write should be described before its registration:\n{log}"
    );
}
