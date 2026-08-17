//! File-only diagnostics: the activation gate, the writer, and the one place a subscriber
//! is built.
//!
//! Both binaries need this and `scratchbox-core` must not have it, so it lives in its own
//! crate rather than in either of them. The rule it keeps in one place: **diagnostics go to
//! a file and never to a standard stream.** The TUI owns the terminal, and `scratchbox` is
//! fired from a global hotkey where a stray line is a visible defect. `ansi` is not merely
//! disabled — `nu-ansi-term` is not a dependency, so no escape sequence can reach the file
//! even by accident. `parse_lossy` and `from_env_lossy` are likewise unused: they
//! `eprintln!` their complaint, which would break the rule from inside the crate meant to
//! hold it.
//!
//! **Nothing here can fail the operation it observes.** Every entry point returns `Option`,
//! `bool`, or an `io::Result` a caller is expected to discard, so there is nothing to
//! propagate with `?`. An unparseable filter, an unwritable directory, an occupied path, or
//! a log directory pointed inside the watched workspace all mean the same thing: the process
//! runs without diagnostics. `scratchbox` reads stdin to EOF before it opens the workspace,
//! so a diagnostics failure that aborted the run would drain the pipe and drop the thought
//! the hotkey was pressed to capture.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Target prefix a `RUST_LOG` directive has to name before diagnostics exist at all.
///
/// A prefix rather than an exact match because `EnvFilter` matches targets by prefix, so the
/// one directive `scratchbox=debug` covers `scratchbox_core::*` and `scratchbox_tui::*` alike.
const TARGET: &str = "scratchbox";

/// Where a test binary writes, since it has no `Config` to ask.
const TEST_DIR_VAR: &str = "SCRATCHBOX_LOG_DIR";

/// Cap on a log file this crate owns, past which the next open truncates it.
///
/// A trace-level session over a cloud-mounted workspace — a configuration this codebase
/// designs for — writes a line per sidecar event for as long as it runs, and nothing else
/// bounds that. `scratchbox-tui` rotates instead of truncating; see its `diagnostics` module.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Owner-only, and the authoritative barrier for everything in the directory.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Owner-only, for the files this crate creates itself.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// The filter this process should use, or `None` to stay silent.
///
/// `None` must be taken literally: a caller that gets it creates no directory, no file, and
/// no thread. Two conditions have to hold, and the caller checks a third with [`overlaps`].
///
/// 1. **`RUST_LOG` is present and non-empty.** Gating on
///    `EnvFilter::try_from_default_env()`'s error is not enough — that only fails when the
///    variable is absent. `RUST_LOG=""` parses to `Ok` with zero directives, which would
///    otherwise buy a directory, a file, and a worker thread to write nothing forever.
///    `RUST_LOG=` is the ordinary shell idiom for blanking a variable, and both
///    `docker run -e RUST_LOG` and an Actions `env:` entry propagate an empty value.
/// 2. **Some directive names a `scratchbox` target.** `RUST_LOG` is ecosystem-wide. A
///    developer with `export RUST_LOG=info` in their profile must not silently acquire a log
///    directory, an appender thread, and an unbounded file — nor `notify` internals they
///    never asked for. `RUST_LOG=info` stays off; `RUST_LOG=scratchbox=debug` works.
pub fn filter() -> Option<EnvFilter> {
    let value = std::env::var_os("RUST_LOG")?;
    let value = value.to_str()?;
    if value.trim().is_empty() || !names_scratchbox(value) {
        return None;
    }
    // `parse`, never `parse_lossy`: the lossy form prints its complaint to stderr.
    EnvFilter::builder().parse(value).ok()
}

/// Does any directive in `value` name a target inside this project?
///
/// Read off the `RUST_LOG` text rather than back out of the parsed [`EnvFilter`], whose
/// `Display` output is a rendering of its directives rather than a documented API. A target
/// is the one part of a directive whose spelling this crate actually depends on.
///
/// A bare level like `info` has no target and so does not count, which is condition 2 of
/// [`filter`].
fn names_scratchbox(value: &str) -> bool {
    value.split(',').any(|directive| {
        // `target[span{field=value}]=level` — neither the span selector nor the level is
        // part of the target.
        let target = directive.split('=').next().unwrap_or_default();
        let target = target.split('[').next().unwrap_or_default();
        target.trim().starts_with(TARGET)
    })
}

/// Is the log directory somewhere a watcher would see it change?
///
/// A log line describing a filesystem event, written inside the watched tree, produces that
/// same event one debounce window later, which is logged, forever. The default layout puts
/// `log/` beside `notes/` so this cannot happen; this check is what holds when a workspace is
/// pointed somewhere unusual. Callers that get `true` install nothing.
///
/// Both directions, and both sides resolved, for the same reasons
/// `Config::trash_overlaps_workspace` gives — the shape this deliberately copies.
pub fn overlaps(log_dir: &Path, workspace: &Path) -> bool {
    let log_dir = resolved(log_dir);
    let workspace = resolved(workspace);
    log_dir.starts_with(&workspace) || workspace.starts_with(&log_dir)
}

/// The path with its longest existing prefix canonicalized.
///
/// `canonicalize` needs the whole path to exist and the log directory usually does not yet,
/// so resolving only the part that does is what makes the comparison in [`overlaps`] work on
/// macOS: FSEvents reports `/private/var/...` for a workspace given as `/var/...`, which is
/// why `watcher::spawn` canonicalizes its root at all. A textual `starts_with` between one
/// resolved path and one unresolved path would miss the overlap entirely.
fn resolved(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut head = path;
    loop {
        if let Ok(mut real) = head.canonicalize() {
            real.extend(suffix.iter().rev());
            return real;
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                suffix.push(name.to_owned());
                head = parent;
            }
            // No existing ancestor at all. The literal path is the best answer available,
            // and a comparison between two literal paths is still a useful one.
            _ => return path.to_path_buf(),
        }
    }
}

/// Create `dir` and any missing parent, owner-only.
///
/// 0700 rather than whatever the umask says, and the directory is the authoritative barrier
/// for everything inside it: `tracing-appender` creates its rotated files with no mode hook
/// of their own, so this is the only thing standing between them and a default 0644. Every
/// other file this project creates is forced to 0600 and `atomic.rs` says why — *a
/// scratchpad collects API keys and passwords whether or not it was meant to*. A log of that
/// scratchpad's activity is a third creation path and inherits the rule.
///
/// An existing directory is left as it is, mode included. Nothing here reaches back to
/// tighten a directory the user created.
pub fn ensure_log_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DIR_MODE);
    }
    builder.create(dir)
}

/// Open (or create) a log file at `dir/name`, owner-only, truncating it if it has already
/// grown past [`MAX_BYTES`].
///
/// Used by `scratchbox` and by test binaries. `scratchbox-tui` wraps a rotating appender
/// instead, because it is the one that stays open for hours.
///
/// Truncating rather than rotating is what keeps `tracing-appender` — and with it `time` and
/// `symlink` — out of the hotkey binary's dependency graph. `scratchbox` writes a handful of
/// lines per invocation, and a test binary wants one file per run anyway.
pub fn open_log_file(dir: &Path, name: &str) -> io::Result<LogFile> {
    ensure_log_dir(dir)?;
    let path = dir.join(name);

    let oversized = fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_BYTES);

    let mut options = OpenOptions::new();
    options.write(true).create(true);
    // Never both: `append` and `truncate` together is an error, not a preference.
    if oversized {
        options.truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }

    Ok(LogFile(Mutex::new(options.open(&path)?)))
}

/// Install a subscriber writing to `writer` and to nothing else. `true` if it took.
///
/// The single place the "a file, never a standard stream" rule is expressed, so the two
/// binaries cannot drift into one of them printing into a terminal it does not own.
///
/// `false` means a subscriber was already installed. That is `tracing`'s own rule — one
/// global default per process — and not a failure worth reporting anywhere.
pub fn subscribe<W>(filter: EnvFilter, writer: W) -> bool
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_target(true)
        .try_init()
        .is_ok()
}

/// Install diagnostics for a test binary, once per process.
///
/// Test binaries install no subscriber of their own, so without this the CI wiring that
/// repeats the watcher suite twenty times per OS records nothing — and the race it exists to
/// catch is precisely the one this crate was added to make diagnosable.
///
/// The directory comes from `$SCRATCHBOX_LOG_DIR` rather than from a config, because a test
/// binary has none to read and because that is what lets CI hand each repetition its own
/// directory. One accumulating file is twenty runs concatenated with no run boundary.
///
/// **Synchronous**, not `tracing_appender::non_blocking`: nothing drops a worker guard at
/// test-process exit, so a non-blocking writer would lose the tail of the run — the half of
/// a race record worth having.
///
/// The file is named for the test binary, so two binaries running concurrently under one
/// `$SCRATCHBOX_LOG_DIR` do not interleave their records.
pub fn init_for_tests() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let Some(filter) = filter() else {
            return;
        };
        let Some(dir) = std::env::var_os(TEST_DIR_VAR) else {
            return;
        };
        let Ok(writer) = open_log_file(Path::new(&dir), &test_log_name()) else {
            return;
        };
        subscribe(filter, writer);
    });
}

/// This test binary's own file name, so concurrent binaries get separate files.
fn test_log_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map_or_else(|| "tests.log".to_owned(), |name| format!("{name}.log"))
}

/// A log file that writes one record at a time.
///
/// The mutex is load-bearing rather than defensive. The formatting layer can issue more than
/// one `write` per event, and the watcher's translator thread logs concurrently with whatever
/// the main thread is doing, so without it two records interleave mid-line. That would cost
/// both the one-record-per-line shape the escaping guarantee is read against and a human's
/// ability to follow an interleaving — which is the entire reason this log exists.
pub struct LogFile(Mutex<File>);

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        // A poisoned lock must not take the process down over diagnostics: a panic elsewhere
        // should cost at most one garbled line. `Suppressor::lock` makes the same call for
        // the same reason.
        LogFileWriter(
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

/// The guard [`LogFile`] hands out, held for exactly one record.
pub struct LogFileWriter<'a>(MutexGuard<'a, File>);

impl Write for LogFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

// Unix-only: every assertion here is either about a permission bit or about resolving a
// symlink, and neither has a meaning to check on a platform without them. `docs/dependencies`
// records that Windows support is deferred.
#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const LOG_NAME: &str = "scratchbox.log";

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_bare_level_names_no_target_so_it_leaves_us_off() {
        assert!(!names_scratchbox("info"));
        assert!(!names_scratchbox("trace"));
        // Somebody else's crate, which is the common case for an exported `RUST_LOG`.
        assert!(!names_scratchbox("notify=trace,hyper=debug"));
    }

    #[test]
    fn a_scratchbox_directive_turns_us_on_however_it_is_spelled() {
        assert!(names_scratchbox("scratchbox=debug"));
        assert!(names_scratchbox("scratchbox_core::watcher=trace"));
        // One directive out of several is enough, and the level may be omitted.
        assert!(names_scratchbox("info,scratchbox=trace"));
        assert!(names_scratchbox("scratchbox"));
        // A span selector is not part of the target.
        assert!(names_scratchbox("scratchbox[save]=info"));
    }

    #[test]
    fn a_log_directory_beside_the_workspace_does_not_overlap_it() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!overlaps(
            &tmp.path().join("log"),
            &tmp.path().join("notes")
        ));
    }

    #[test]
    fn a_log_directory_inside_the_workspace_overlaps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("notes");
        assert!(overlaps(&workspace.join("log"), &workspace));
    }

    /// The other direction is checked for the same reason
    /// `Config::trash_overlaps_workspace` checks it: a workspace *inside* the log directory
    /// means every note sits in a tree we write to.
    #[test]
    fn a_workspace_inside_the_log_directory_overlaps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        assert!(overlaps(&log, &log.join("notes")));
    }

    /// Component-wise, so a sibling that merely shares a name prefix is correctly separate.
    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_an_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!overlaps(
            &tmp.path().join("notes-log"),
            &tmp.path().join("notes")
        ));
    }

    /// The macOS trap `watcher::spawn` documents, built by hand: one side resolves through a
    /// symlink and the other does not exist yet, so a textual comparison reports no overlap.
    #[test]
    fn an_overlap_seen_through_a_symlinked_parent_is_still_an_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(overlaps(&real.join("log"), &link));
        assert!(overlaps(&link.join("log"), &real));
    }

    #[test]
    fn the_directory_is_owner_only_and_so_is_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested/log");

        let _writer = open_log_file(&dir, LOG_NAME).unwrap();

        assert_eq!(mode(&dir), DIR_MODE, "the log directory must be owner-only");
        assert_eq!(
            mode(&dir.join(LOG_NAME)),
            FILE_MODE,
            "a log we create ourselves must be owner-only"
        );
        // `recursive` applies the mode to every component it creates, so the intermediate
        // directory is covered too rather than left at the umask.
        assert_eq!(mode(&tmp.path().join("nested")), DIR_MODE);
    }

    /// Open the log and write one record through it, the way a subscriber would.
    fn append_line(dir: &Path, line: &str) {
        let log = open_log_file(dir, LOG_NAME).unwrap();
        writeln!(log.make_writer(), "{line}").unwrap();
    }

    #[test]
    fn a_second_open_appends_rather_than_truncating() {
        let tmp = tempfile::tempdir().unwrap();

        append_line(tmp.path(), "first");
        append_line(tmp.path(), "second");

        assert_eq!(
            fs::read_to_string(tmp.path().join(LOG_NAME)).unwrap(),
            "first\nsecond\n",
            "a second invocation should not throw away what the first recorded"
        );
    }

    #[test]
    fn a_file_past_the_cap_is_truncated_on_the_next_open() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOG_NAME);
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();

        append_line(tmp.path(), "fresh");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fresh\n",
            "an oversized log should be truncated rather than grown forever"
        );
    }

    #[test]
    fn a_path_occupied_by_a_file_is_an_error_rather_than_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let occupied = tmp.path().join("log");
        fs::write(&occupied, "not a directory").unwrap();

        assert!(open_log_file(&occupied, LOG_NAME).is_err());
    }
}
