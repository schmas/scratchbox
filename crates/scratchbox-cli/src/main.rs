//! Append stdin to the active note without opening the TUI.
//!
//! The target of a global hotkey: the shortcut runs this, it writes, it exits. No daemon
//! and no IPC — it reaches the same workspace through the same [`FolderSync`] the TUI uses,
//! and a running TUI notices through its watcher like it would any other external change.

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use scratchbox_core::order::OrderStore;
use scratchbox_core::{Config, FolderSync, Format, NoteId, Store, reconcile};

const USAGE: &str = "\
usage: scratchbox [--workspace <dir>]
       scratchbox --purge-trash [--yes] [--workspace <dir>]

Appends stdin to the active note — the one at the top of the list — and exits.

  --workspace <dir>   use this workspace instead of the configured one
  --purge-trash       empty the trash directory
  --yes               answer the purge confirmation in advance
  --help              show this message
  --version           show the version

  echo 'a thought' | scratchbox
";

/// Exit code for being used wrongly, as distinct from failing.
const EXIT_USAGE: u8 = 2;

/// One file per binary, so two processes writing at once never share a handle. Millisecond
/// timestamps are what let a `scratchbox` line be placed against a `scratchbox-tui` one.
const LOG_FILE: &str = "scratchbox.log";

enum Command {
    Append {
        workspace: Option<PathBuf>,
    },
    PurgeTrash {
        workspace: Option<PathBuf>,
        confirmed: bool,
    },
    Help,
    Version,
}

fn main() -> ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("scratchbox: {message}");
            eprint!("{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    match run(command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("scratchbox: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> std::result::Result<Command, String> {
    let mut workspace = None;
    let mut purge = false;
    let mut confirmed = false;

    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--workspace" => {
                let path = args.next().ok_or("--workspace needs a directory")?;
                workspace = Some(PathBuf::from(path));
            }
            "--purge-trash" => purge = true,
            "--yes" | "-y" => confirmed = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    if purge {
        return Ok(Command::PurgeTrash {
            workspace,
            confirmed,
        });
    }
    if confirmed {
        return Err("--yes only means something with --purge-trash".to_owned());
    }
    Ok(Command::Append { workspace })
}

fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Help => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            println!("scratchbox {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Append { workspace } => append(workspace),
        Command::PurgeTrash {
            workspace,
            confirmed,
        } => purge_trash(workspace, confirmed),
    }
}

fn append(workspace: Option<PathBuf>) -> Result<ExitCode> {
    // Nothing is piped in, so there is nothing to append. Blocking on a terminal here would
    // look like the app had hung, which from a hotkey is indistinguishable from broken.
    if io::stdin().is_terminal() {
        eprintln!("scratchbox: nothing on stdin");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(EXIT_USAGE));
    }

    let input = read_stdin()?;
    // A no-op rather than an empty write: touching the note would wake a running TUI's
    // watcher to report a change that never happened.
    if input.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let (store, order) = open(workspace)?;
    let id = active_note(&store, &order)?;

    let existing = store
        .read(&id)
        .with_context(|| format!("could not read {id}"))?;
    store
        .write(&id, &appended(&existing, &input))
        .with_context(|| format!("could not write {id}"))?;

    Ok(ExitCode::SUCCESS)
}

fn purge_trash(workspace: Option<PathBuf>, confirmed: bool) -> Result<ExitCode> {
    let config = load_config(workspace)?;

    // Checked before anything is opened or deleted. `purge_trash` empties whatever
    // directory it is pointed at, so a trash overlapping the workspace would take live
    // notes with it — refused outright rather than half-done and reported.
    if config.trash_overlaps_workspace() {
        bail!(
            "refusing to purge: the trash at {} overlaps the workspace at {}. \
             Point `trash` somewhere outside the workspace first.",
            config.trash.display(),
            config.workspace.display(),
        );
    }

    if !confirmed && !confirm(&config)? {
        println!("nothing was removed");
        return Ok(ExitCode::SUCCESS);
    }

    config.ensure_dirs().context("could not open the trash")?;
    let store = FolderSync::new(config.workspace.clone(), config.trash.clone())
        .context("could not open the workspace")?;

    let removed = store.purge_trash().context("could not empty the trash")?;
    println!(
        "removed {removed} {} from {}",
        if removed == 1 { "entry" } else { "entries" },
        config.trash.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Ask before emptying the trash.
///
/// Without a terminal there is nobody to ask, so the answer is no and `--yes` is the way to
/// say otherwise. Guessing on behalf of a script is how a cron job deletes something.
fn confirm(config: &Config) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("--purge-trash needs --yes when it is not run from a terminal");
    }

    print!("Empty the trash at {}? [y/N] ", config.trash.display());
    io::stdout().flush().context("could not write the prompt")?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("could not read the answer")?;

    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn open(workspace: Option<PathBuf>) -> Result<(FolderSync, OrderStore)> {
    let config = load_config(workspace)?;
    config
        .ensure_dirs()
        .context("could not open the workspace")?;

    start_diagnostics(&config);

    let store = FolderSync::new(config.workspace.clone(), config.trash.clone())
        .context("could not open the workspace")?;
    Ok((store, OrderStore::new(&config.app_dir())))
}

/// Start file diagnostics if `RUST_LOG` asks for them, and never mind if that fails.
///
/// **Nothing here may return an error.** `append` reads stdin to EOF at its `read_stdin` call
/// before it ever gets here, so a `?` on this path would mean `echo 'a thought' | scratchbox`
/// draining the pipe, exiting non-zero, and writing nothing — straight into the failure
/// `active_note` calls out below: *a hotkey that reports "no notes" instead of capturing the
/// thought has failed at the only job it has*. An unparseable `RUST_LOG`, an unwritable data
/// home, and a log directory inside the workspace all cost diagnostics and nothing else.
///
/// No appender, so no rotation: the file truncates once it passes
/// `scratchbox_log::MAX_BYTES`. This binary writes a handful of lines per invocation.
fn start_diagnostics(config: &Config) {
    let Some(filter) = scratchbox_log::filter() else {
        return;
    };
    let dir = config.log_dir();

    // A log line written inside the watched tree wakes a running TUI's watcher, which is the
    // one process that would notice. Refusing is the honest answer; see `scratchbox_log`.
    if scratchbox_log::overlaps(&dir, &config.workspace) {
        return;
    }

    if let Ok(writer) = scratchbox_log::open_log_file(&dir, LOG_FILE) {
        scratchbox_log::subscribe(filter, writer);
    }
}

fn load_config(workspace: Option<PathBuf>) -> Result<Config> {
    let config = Config::load(workspace).context("could not read the configuration")?;
    for warning in &config.warnings {
        eprintln!("scratchbox: {warning}");
    }
    Ok(config)
}

/// The note this appends to: the top of the list, which is the one the TUI would open on.
///
/// An empty workspace gets a note made for it — a hotkey that reports "no notes" instead of
/// capturing the thought has failed at the only job it has.
fn active_note(store: &FolderSync, order: &OrderStore) -> Result<NoteId> {
    let notes = store.list().context("could not read the workspace")?;

    match reconcile(&order.load(), &notes).into_iter().next() {
        Some(id) => Ok(id),
        None => store
            .create(Format::Markdown)
            .context("could not create the first note"),
    }
}

/// Join input onto a note without running it into the last line.
///
/// The input itself is untouched, trailing whitespace and all: what was piped in is what
/// lands in the note.
fn appended(existing: &str, input: &str) -> String {
    let mut out = String::with_capacity(existing.len() + input.len() + 1);
    out.push_str(existing);
    if !existing.is_empty() && !existing.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(input);
    out
}

fn read_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .context("could not read stdin")?;

    // Notes are UTF-8. Refusing beats writing bytes that would make the note unreadable to
    // the app that has to open it next.
    String::from_utf8(bytes).map_err(|error| {
        anyhow::anyhow!(
            "stdin is not valid UTF-8 (byte {} is invalid); notes are text",
            error.utf8_error().valid_up_to()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::appended;

    #[test]
    fn a_note_ending_in_a_newline_is_appended_to_directly() {
        assert_eq!(appended("first\n", "second\n"), "first\nsecond\n");
    }

    #[test]
    fn a_note_without_a_trailing_newline_gets_one_first() {
        assert_eq!(appended("first", "second\n"), "first\nsecond\n");
    }

    #[test]
    fn an_empty_note_gains_no_leading_blank_line() {
        assert_eq!(appended("", "first\n"), "first\n");
    }

    #[test]
    fn input_is_appended_exactly_as_it_arrived() {
        assert_eq!(appended("a\n", "  b  \n\n\tc"), "a\n  b  \n\n\tc");
    }
}
