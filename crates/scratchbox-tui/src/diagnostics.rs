//! The TUI's writer: a daily-rotating file behind a non-blocking worker.
//!
//! Separate from `scratchbox-log` because `tracing-appender` is a dependency of this crate
//! alone. It requires `time` non-optionally, which `docs/dependencies.md` rejects by name
//! since `jiff` already covers that ground, plus `symlink` to maintain a `latest` link
//! nothing here asks for. A cargo feature on `scratchbox-log` could not have achieved the
//! split: features unify across two normal dependents in one `cargo build --workspace`, so
//! `scratchbox` — fired from a global hotkey, and the binary whose size the decision record
//! scrutinises most closely — would have linked the appender regardless. See D7.
//!
//! `scratchbox` truncates a single file instead; see `scratchbox_log::open_log_file`.

use scratchbox_core::Config;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// Days of diagnostics kept before the oldest file is removed.
///
/// The session this is for can stay open for days on a cloud-mounted workspace, writing a
/// line per sidecar event at `trace`, and nothing else here bounds that.
const KEEP_DAYS: usize = 7;

const FILE_PREFIX: &str = "scratchbox-tui";
const FILE_SUFFIX: &str = "log";

/// Start file diagnostics for this session, or carry on without them.
///
/// `None` means off, and off means *nothing was created* — no directory, no file, no worker
/// thread. Every failure path returns it: `RUST_LOG` absent, empty, or naming no `scratchbox`
/// target; a log directory that overlaps the watched workspace; an unwritable data home; a
/// subscriber already installed.
///
/// The caller must hold the returned guard for the whole session. Dropping it flushes the
/// worker, so dropping it early costs the tail of the log — which is the half of a race
/// record worth having.
pub fn start(config: &Config) -> Option<WorkerGuard> {
    let filter = scratchbox_log::filter()?;
    let dir = config.log_dir();

    // Refuse rather than filter. A log line describing a filesystem event, written inside the
    // watched tree, produces that same event one debounce window later, which is logged,
    // forever. The default layout puts `log/` beside `notes/`, so this only bites when a
    // workspace is pointed somewhere unusual — and then the honest answer is no diagnostics
    // rather than a feedback loop the user has to diagnose without them.
    if scratchbox_log::overlaps(&dir, &config.workspace) {
        return None;
    }

    // Ours, at 0700, before the appender can create it at the umask. The appender creates its
    // rotated files with `append(true).create(true)` and no mode hook, so those inherit the
    // umask and the directory is the only thing keeping them owner-only. Accepted, and stated
    // here rather than implying per-file control this does not have.
    scratchbox_log::ensure_log_dir(&dir).ok()?;

    // `builder().build()`, not `rolling::daily` or `rolling::never`: those **panic** on a
    // directory failure rather than returning one, which would turn an unwritable
    // `$XDG_DATA_HOME` into a startup crash caused by a feature the user only asked to
    // observe with.
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .filename_suffix(FILE_SUFFIX)
        .max_log_files(KEEP_DAYS)
        .build(&dir)
        .ok()?;

    let (writer, guard) = tracing_appender::non_blocking(appender);

    // Non-blocking here, unlike the test harness: this thread also draws frames, and a
    // synchronous write to a cloud-mounted log during a repaint would be felt. The guard is
    // what makes it safe — it is dropped at the end of the session, which flushes.
    scratchbox_log::subscribe(filter, writer).then_some(guard)
}
