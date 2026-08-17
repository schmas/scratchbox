//! The scratchbox terminal UI.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use scratchbox_core::order::OrderStore;
use scratchbox_core::{Config, FolderSync, Store};

use scratchbox_tui::app::App;
use scratchbox_tui::event::{AppEvent, Events};
use scratchbox_tui::keys::Action;
use scratchbox_tui::{diagnostics, keys, ui};

struct Options {
    workspace: Option<PathBuf>,
    /// Render one frame and exit, for measuring startup without a human in the loop.
    bench_first_frame: bool,
}

fn main() -> ExitCode {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("scratchbox-tui: {message}");
            eprintln!("usage: scratchbox-tui [--workspace <dir>] [--bench-first-frame]");
            return ExitCode::from(2);
        }
    };

    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Printed after the terminal is restored, or it would land in an alternate
            // screen that is about to disappear.
            eprintln!("scratchbox-tui: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args() -> std::result::Result<Options, String> {
    let mut options = Options {
        workspace: None,
        bench_first_frame: false,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                let path = args.next().ok_or("--workspace needs a directory")?;
                options.workspace = Some(PathBuf::from(path));
            }
            "--bench-first-frame" => options.bench_first_frame = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(options)
}

fn run(options: Options) -> Result<()> {
    let config = Config::load(options.workspace).context("could not read the configuration")?;
    config
        .ensure_dirs()
        .context("could not open the workspace")?;

    // Before the store, so the watcher and the suppressor have somewhere to report from.
    //
    // Bound to a name and held for the whole of `run`: dropping the guard flushes the
    // appender's worker, and a `let _ =` would drop it here and lose every line. Deliberately
    // no `?` — there is no error to propagate. Diagnostics that could fail startup would be a
    // feature that breaks the app it exists to observe.
    let _diagnostics = diagnostics::start(&config);

    let store = FolderSync::new(config.workspace.clone(), config.trash.clone())
        .context("could not open the workspace")?;
    let events = store.subscribe();

    let order = OrderStore::new(&config.app_dir());
    let mut app = App::new(Box::new(store), order)?;
    for warning in &config.warnings {
        eprintln!("scratchbox: {warning}");
    }

    if options.bench_first_frame {
        return bench_first_frame(&mut app);
    }

    let mut terminal = init_terminal();
    // On screen first. Watching the workspace is what makes the app feel live, but it is
    // also the slowest thing at startup, and the notes are readable without it.
    terminal
        .draw(|frame| ui::render(frame, &mut app))
        .context("could not render the first frame")?;
    if let Err(error) = app.start_watching() {
        app.set_status(format!(
            "changes made outside the app will not show: {error}"
        ));
    }

    let outcome = event_loop(&mut terminal, &mut app, &Events::new(events));
    ratatui::restore();
    outcome.context("the terminal session ended badly")
}

/// Render one frame off-screen and exit, for measuring startup.
///
/// Off-screen rather than on the real terminal because entering raw mode needs a TTY,
/// which a benchmark runner does not have. What it measures is the work that actually
/// scales with the workspace — reading the directory, reconciling the order, composing the
/// frame — rather than the fixed cost of switching screens.
fn bench_first_frame(app: &mut App) -> Result<()> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
        .context("could not set up an off-screen terminal")?;
    terminal
        .draw(|frame| ui::render(frame, app))
        .context("could not render the first frame")?;
    Ok(())
}

/// Enter the alternate screen.
///
/// `ratatui::init` installs a panic hook that restores the terminal first, which is the
/// behaviour that matters here: a panic that skipped the restore would leave the shell in
/// raw mode with no echo, unusable for reasons the user cannot see. Verified rather than
/// assumed — a deliberate panic leaves the alternate screen and prints its message.
fn init_terminal() -> DefaultTerminal {
    ratatui::init()
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    events: &Events,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        // With nothing pending this is `None` and the loop simply blocks: an idle
        // scratchpad should cost nothing at all.
        let timeout = app
            .wake_at()
            .map(|at| at.saturating_duration_since(Instant::now()));
        let Some(event) = events.next(timeout) else {
            return Ok(());
        };

        match event {
            AppEvent::Terminal(crossterm::event::Event::Key(key)) if key.is_press() => {
                if let Err(error) = handle_key(app, key) {
                    app_error(app, error);
                }
            }
            AppEvent::Terminal(_) => {}
            AppEvent::Store(store_event) => {
                if let Err(error) = app.apply_store_event(&store_event) {
                    app_error(app, error);
                }
            }
            AppEvent::Tick => {
                if let Err(error) = app.on_tick() {
                    app_error(app, error);
                }
            }
        }

        if app.should_quit() {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> scratchbox_core::Result<()> {
    if app.pending_delete().is_some() {
        return match keys::map_confirmation(key) {
            Action::ConfirmDelete => app.confirm_delete(),
            _ => {
                app.cancel_delete();
                Ok(())
            }
        };
    }

    // An unresolved external change owns the keyboard until it is answered: every other
    // path through the app either reloads the buffer or writes it, and both would decide
    // for the user which version of the note survives.
    if app.conflict().is_some() {
        return match keys::map_conflict(key) {
            Action::KeepMine => app.keep_mine(),
            Action::TakeTheirs => app.take_theirs(),
            Action::Quit => app.quit(),
            _ => Ok(()),
        };
    }

    match keys::map(key, app.focus()) {
        Action::Quit => app.quit(),
        Action::NewNote => app.create_note(),
        Action::RequestDelete => {
            app.request_delete();
            Ok(())
        }
        Action::MoveNoteUp => app.move_selection_up(),
        Action::MoveNoteDown => app.move_selection_down(),
        Action::SelectPrevious => app.select_previous(),
        Action::SelectNext => app.select_next(),
        Action::ToggleFocus => {
            app.toggle_focus();
            Ok(())
        }
        Action::Edit(key) => {
            app.edit(key);
            Ok(())
        }
        Action::ConfirmDelete
        | Action::CancelDelete
        | Action::KeepMine
        | Action::TakeTheirs
        | Action::Ignore => Ok(()),
    }
}

/// Show a failure in the status line rather than tearing the session down.
///
/// A note that will not save is worth knowing about, but exiting would take the rest of the
/// user's unsaved work with it.
fn app_error(app: &mut App, error: scratchbox_core::Error) {
    app.set_status(format!("{error}"));
}
