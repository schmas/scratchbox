//! What the `scratchbox-tui` binary does about diagnostics at startup.
//!
//! The other half of `crates/scratchbox-cli/tests/logging.rs`. There are **two writer paths**
//! — this binary composes a non-blocking worker over a daily-rotating appender, `scratchbox`
//! writes through a plain file that truncates past a cap — and the activation gate, the
//! permission barrier, and the overlap refusal have to hold on both. The CLI file carries the
//! guarantees that need log *content* to check, because a `--bench-first-frame` run does no
//! instrumented work; this file carries the ones about what exists on disk and what reaches a
//! standard stream.
//!
//! Subprocess tests for the reason `tests/support/mod.rs` gives.

mod support;

use std::fs;

use support::{Sandbox, code, stderr};

const ON: &str = "scratchbox=trace";

/// Not activated has to mean *nothing exists* — no directory, no file, no worker thread. The
/// appender is the path where a thread is genuinely at stake.
#[test]
fn no_log_appears_when_rust_log_is_unset_empty_or_untargeted() {
    let cases: [(&str, &[(&str, &str)]); 4] = [
        ("absent", &[]),
        ("empty", &[("RUST_LOG", "")]),
        ("no target", &[("RUST_LOG", "info")]),
        ("somebody else's crate", &[("RUST_LOG", "notify=trace")]),
    ];

    for (what, env) in cases {
        let sandbox = Sandbox::new();
        let output = sandbox.run(&sandbox.workspace(), env);

        assert_eq!(code(&output), 0, "{what}: {}", stderr(&output));
        assert!(
            !sandbox.log_dir().exists(),
            "{what} created {}; not activated must mean nothing exists at all",
            sandbox.log_dir().display()
        );
    }
}

/// The directory is the barrier and the only one available here. `tracing-appender` opens its
/// rotated files with `append(true).create(true)` and no mode hook, so those inherit the umask
/// — accepted and documented in `diagnostics.rs` rather than papered over with a per-file
/// assertion this code cannot honour.
#[cfg(unix)]
#[test]
fn the_log_directory_is_owner_only_and_holds_the_appender_file() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new();
    let output = sandbox.run(&sandbox.workspace(), &[("RUST_LOG", ON)]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let mode = fs::metadata(sandbox.log_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o700,
        "the log directory is what keeps a rotated file unreachable and must be owner-only"
    );

    // Created eagerly by the appender, so an activated session always leaves one behind even
    // before it has anything to say.
    let files = sandbox.log_files();
    assert_eq!(
        files.len(),
        1,
        "expected exactly one appender file, got {files:?}"
    );
    assert!(
        files[0].starts_with("scratchbox-tui.") && files[0].ends_with(".log"),
        "the daily rotation should date-stamp its file: {files:?}"
    );
}

/// This binary owns the terminal. A diagnostic on either stream would land in an alternate
/// screen or, worse, survive it.
#[test]
fn nothing_reaches_stdout_or_stderr_while_logging() {
    let sandbox = Sandbox::new();

    let output = sandbox.run(&sandbox.workspace(), &[("RUST_LOG", ON)]);

    assert_eq!(code(&output), 0);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "diagnostics reached stdout"
    );
    assert_eq!(stderr(&output), "", "diagnostics reached stderr");
    // Not passing by being switched off.
    assert_eq!(sandbox.log_files().len(), 1);
}

/// Reds if the overlap refusal is removed: the appender would then create the directory, and
/// its own writes would be inside the tree the watcher reports on.
#[test]
fn a_log_directory_inside_the_workspace_disables_logging() {
    let sandbox = Sandbox::new();

    // The log defaults to `<data_home>/scratchbox/log`, so a workspace at
    // `<data_home>/scratchbox` contains it.
    let workspace = sandbox.data_home().join("scratchbox");
    fs::create_dir_all(&workspace).unwrap();

    let output = sandbox.run(&workspace, &[("RUST_LOG", ON)]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        !sandbox.log_dir().exists(),
        "the overlap was not refused, so the appender would feed the watcher it describes"
    );
}

/// An unwritable data home costs diagnostics and nothing else. `rolling::daily` would have
/// **panicked** here rather than returned — which is why `diagnostics.rs` goes through
/// `RollingFileAppender::builder().build()`.
#[test]
fn an_unwritable_log_directory_does_not_fail_the_run() {
    let sandbox = Sandbox::new();

    // A file where the log directory needs to go.
    fs::create_dir_all(sandbox.data_home().join("scratchbox")).unwrap();
    fs::write(sandbox.log_dir(), "not a directory").unwrap();

    let output = sandbox.run(&sandbox.workspace(), &[("RUST_LOG", ON)]);

    assert_eq!(
        code(&output),
        0,
        "diagnostics failed the session they only exist to observe: {}",
        stderr(&output)
    );
    assert_eq!(stderr(&output), "", "the failure was announced on stderr");
}
