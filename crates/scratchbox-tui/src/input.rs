//! Deciding what a keystroke means, and doing it.
//!
//! In the library rather than beside the event loop so this order can be driven from a test.
//! Which modal owns the keyboard is where the bugs in this shape of UI live — a key reaching
//! the buffer from behind a prompt, a confirmation answered by a keystroke meant for
//! something else — and none of it needs a terminal to prove.

use crossterm::event::KeyEvent;
use scratchbox_core::Result;

use crate::app::App;
use crate::keys::{self, Action, Command, HelpKey};

/// Hand a key to whatever owns the keyboard.
///
/// The order is the contract. A delete waiting to be confirmed owns every key; an unresolved
/// external change owns them next, for the reason its own branch gives; the keybindings panel
/// comes after both, because it is the one modal the user can always close; and only then
/// does the normal-mode keymap get a look.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
            Action::Do(Command::Quit) => app.quit(),
            _ => Ok(()),
        };
    }

    if app.help().is_some() {
        return match keys::map_help(key, app.help_searching()) {
            HelpKey::Close => {
                app.close_help();
                Ok(())
            }
            HelpKey::Up => {
                app.help_move(-1);
                Ok(())
            }
            HelpKey::Down => {
                app.help_move(1);
                Ok(())
            }
            HelpKey::Top => {
                app.help_to(0);
                Ok(())
            }
            HelpKey::Bottom => {
                app.help_to_end();
                Ok(())
            }
            HelpKey::Search => {
                app.help_search();
                Ok(())
            }
            HelpKey::Type(c) => {
                app.help_type(c);
                Ok(())
            }
            HelpKey::Backspace => {
                app.help_backspace();
                Ok(())
            }
            HelpKey::SearchCommit => {
                app.help_search_commit();
                Ok(())
            }
            HelpKey::SearchCancel => {
                app.help_search_cancel();
                Ok(())
            }
            // Read before closing, because closing drops the state the cursor lives in. A row
            // with nothing to run does nothing at all — it is drawn dimmed to say so.
            HelpKey::Run => match app.help_selection() {
                Some(command) => {
                    app.close_help();
                    dispatch(app, command)
                }
                None => Ok(()),
            },
            // Closed before quitting, or a refusal to quit lands in the status line while
            // the panel is covering it — and a user who cannot see the refusal presses quit
            // again, which goes through with the buffer unwritten.
            HelpKey::Quit => {
                app.close_help();
                app.quit()
            }
            _ => Ok(()),
        };
    }

    match keys::map(key, app.focus()) {
        Action::Do(command) => dispatch(app, command),
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

/// Perform a command.
fn dispatch(app: &mut App, command: Command) -> Result<()> {
    match command {
        Command::Quit => app.quit(),
        Command::NewNote => app.create_note(),
        Command::RequestDelete => {
            app.request_delete();
            Ok(())
        }
        Command::MoveNoteUp => app.move_selection_up(),
        Command::MoveNoteDown => app.move_selection_down(),
        Command::SelectPrevious => app.select_previous(),
        Command::SelectNext => app.select_next(),
        Command::ToggleFocus => {
            app.toggle_focus();
            Ok(())
        }
        Command::OpenHelp => {
            app.open_help();
            Ok(())
        }
    }
}
