//! The keymap's chords and the table that declares them.
//!
//! Every modifier combination the keymap answers is listed here one entry at a time —
//! including combinations no keyboard on this platform can produce. A test that only checks
//! the declared chords still answer is blind to a chord that quietly stopped being accepted,
//! and a narrowing is exactly the regression this file exists to catch.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use scratchbox_core::order::OrderStore;
use scratchbox_core::{APP_SUBDIR, FolderSync, NoteId};
use scratchbox_tui::app::{App, Focus};
use scratchbox_tui::editor::EditorPane;
use scratchbox_tui::keys::{self, Action, BINDINGS, Chord, Command, Ctx};
use scratchbox_tui::ui;
use tempfile::TempDir;

/// One pinned chord and the answer it gets in each pane.
struct Pin {
    what: &'static str,
    key: KeyEvent,
    in_list: Action,
    in_editor: Action,
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const NONE: KeyModifiers = KeyModifiers::NONE;

fn ctrl_alt() -> KeyModifiers {
    CTRL | ALT
}

/// The chords the table declares, and what they answer.
fn declared() -> Vec<Pin> {
    // A global binding answers the same way in both panes.
    let global = |what, key, command| Pin {
        what,
        key,
        in_list: Action::Do(command),
        in_editor: Action::Do(command),
    };

    vec![
        global("ctrl+q", key(KeyCode::Char('q'), CTRL), Command::Quit),
        // Raw mode delivers Ctrl-C as a key, so it leaves through the same door.
        global("ctrl+c", key(KeyCode::Char('c'), CTRL), Command::Quit),
        global("ctrl+n", key(KeyCode::Char('n'), CTRL), Command::NewNote),
        global(
            "ctrl+d",
            key(KeyCode::Char('d'), CTRL),
            Command::RequestDelete,
        ),
        global("alt+up", key(KeyCode::Up, ALT), Command::MoveNoteUp),
        global("alt+down", key(KeyCode::Down, ALT), Command::MoveNoteDown),
        global("tab", key(KeyCode::Tab, NONE), Command::ToggleFocus),
        // A widely-held pane-switching convention, declared as a second chord so it keeps
        // working now that modifiers are compared exactly.
        global("ctrl+tab", key(KeyCode::Tab, CTRL), Command::ToggleFocus),
        global("ctrl+h", key(KeyCode::Char('h'), CTRL), Command::OpenHelp),
        global("f1", key(KeyCode::F(1), NONE), Command::OpenHelp),
        // The plain arrows are the one focus-conditional pair: selection in the list, cursor
        // movement in the editor.
        Pin {
            what: "up",
            key: key(KeyCode::Up, NONE),
            in_list: Action::Do(Command::SelectPrevious),
            in_editor: Action::Edit(key(KeyCode::Up, NONE)),
        },
        Pin {
            what: "down",
            key: key(KeyCode::Down, NONE),
            in_list: Action::Do(Command::SelectNext),
            in_editor: Action::Edit(key(KeyCode::Down, NONE)),
        },
    ]
}

/// The chords the keymap used to answer and no longer does.
///
/// Each one was reached through a pattern that left a modifier unexamined, so it accepted the
/// chord with that modifier held as well as without. Modifiers are now compared exactly and
/// only the combinations the table declares match, which drops these eight: they fall through
/// to the editor in the editor pane and are ignored in the list pane, exactly like any other
/// unclaimed chord.
///
/// Derived from the arm patterns one combination at a time rather than read off them — a
/// pattern with two unexamined modifiers accepts four combinations, which is what reading by
/// eye missed twice.
fn dropped() -> Vec<Pin> {
    let dropped = |what, key: KeyEvent| Pin {
        what,
        key,
        in_list: Action::Ignore,
        in_editor: Action::Edit(key),
    };

    vec![
        // ALT went unexamined beside CONTROL on the quit, new, and delete chords.
        dropped("ctrl+alt+q", key(KeyCode::Char('q'), ctrl_alt())),
        dropped("ctrl+alt+c", key(KeyCode::Char('c'), ctrl_alt())),
        dropped("ctrl+alt+n", key(KeyCode::Char('n'), ctrl_alt())),
        dropped("ctrl+alt+d", key(KeyCode::Char('d'), ctrl_alt())),
        // CONTROL went unexamined beside ALT on the reorder chords.
        dropped("ctrl+alt+up", key(KeyCode::Up, ctrl_alt())),
        dropped("ctrl+alt+down", key(KeyCode::Down, ctrl_alt())),
        // Tab examined neither, so it answered in four combinations. Two are declared —
        // plain and ctrl+Tab — and these two are not; alt+Tab is not even deliverable on
        // macOS, where the window manager takes it.
        dropped("alt+tab", key(KeyCode::Tab, ALT)),
        dropped("ctrl+alt+tab", key(KeyCode::Tab, ctrl_alt())),
    ]
}

fn assert_answers(pins: Vec<Pin>) {
    for pin in pins {
        assert_eq!(
            keys::map(pin.key, Focus::List),
            pin.in_list,
            "{} answered differently in the list",
            pin.what
        );
        assert_eq!(
            keys::map(pin.key, Focus::Editor),
            pin.in_editor,
            "{} answered differently in the editor",
            pin.what
        );
    }
}

#[test]
fn every_declared_chord_answers_in_the_pane_it_belongs_to() {
    assert_answers(declared());
}

#[test]
fn the_chords_no_longer_declared_fall_through_instead() {
    assert_answers(dropped());
}

/// The dropped chords reach the editor, and the editor does nothing with them either: edtui
/// compares modifiers exactly, so a chord holding both CONTROL and ALT matches none of its
/// bindings. Silent no-ops rather than editing commands — asserted rather than argued.
#[test]
fn the_dropped_chords_leave_the_buffer_alone() {
    for pin in dropped() {
        let mut editor = EditorPane::new();
        editor.load(NoteId::new("a.md").unwrap(), "text");
        editor.on_key(pin.key);

        assert_eq!(editor.text(), "text", "{} changed the buffer", pin.what);
    }
}

/// A literal `?` belongs to the buffer. Nothing in the keymap may claim it, in either pane.
#[test]
fn a_question_mark_is_not_a_binding() {
    let question = key(KeyCode::Char('?'), KeyModifiers::SHIFT);

    assert_eq!(keys::map(question, Focus::Editor), Action::Edit(question));
    assert_eq!(keys::map(question, Focus::List), Action::Ignore);
}

/// Every command the app has, for the count check below.
const COMMANDS: &[Command] = &[
    Command::Quit,
    Command::NewNote,
    Command::RequestDelete,
    Command::MoveNoteUp,
    Command::MoveNoteDown,
    Command::SelectPrevious,
    Command::SelectNext,
    Command::ToggleFocus,
    Command::OpenHelp,
];

/// How many rows the table declares for a command.
///
/// The `match` is what holds the table honest: it is exhaustive, so a command added to the
/// enum without a decision here fails to compile, and the only decision that keeps the panel
/// complete is a row.
fn expected_rows(command: Command) -> usize {
    match command {
        Command::Quit
        | Command::NewNote
        | Command::RequestDelete
        | Command::MoveNoteUp
        | Command::MoveNoteDown
        | Command::SelectPrevious
        | Command::SelectNext
        | Command::ToggleFocus
        | Command::OpenHelp => 1,
    }
}

#[test]
fn the_table_declares_every_command_once() {
    for command in COMMANDS {
        let rows = BINDINGS
            .iter()
            .filter(|binding| binding.command == *command)
            .count();
        assert_eq!(
            rows,
            expected_rows(*command),
            "{command:?} has {rows} rows in the table"
        );
    }

    // The other direction, so the list above cannot fall behind the table.
    for binding in BINDINGS {
        assert!(
            COMMANDS.contains(&binding.command),
            "{:?} is in the table but not in this test's list",
            binding.command
        );
    }
}

#[test]
fn every_row_answers_with_its_own_command() {
    for binding in BINDINGS {
        for chord in binding.chords {
            let event = key(chord.code, modifiers(*chord));
            let expected = Action::Do(binding.command);

            assert_eq!(
                keys::map(event, Focus::List),
                expected,
                "{} did not produce {:?} in the list",
                binding.caption,
                binding.command
            );
            // A list-only binding leaves the editor's keys alone; a global one answers in both.
            let in_editor = match binding.ctx {
                Ctx::Global => expected,
                Ctx::ListOnly => Action::Edit(event),
            };
            assert_eq!(
                keys::map(event, Focus::Editor),
                in_editor,
                "{} answered wrongly in the editor",
                binding.caption
            );
        }
    }
}

/// Every context overlaps in the list pane, so one chord in two rows is a conflict wherever
/// the two rows sit — whichever came first would silently win.
#[test]
fn no_chord_is_claimed_twice() {
    for (index, binding) in BINDINGS.iter().enumerate() {
        for other in BINDINGS.iter().skip(index + 1) {
            for chord in binding.chords {
                assert!(
                    !other.chords.contains(chord),
                    "{} and {} both claim {}",
                    binding.caption,
                    other.caption,
                    chord.caption()
                );
            }
        }
    }
}

/// The status line reads its keys from the table, and prints exactly this.
///
/// Asserted against the literal string rather than against the table, so a table change that
/// alters what the user reads shows up here as a difference rather than agreeing with itself.
#[test]
fn the_default_status_line_prints_the_keys_it_should() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();
    let mut app = App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap();

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();

    assert_eq!(
        row(terminal.backend().buffer(), 9).trim_end(),
        "^N new   ^D delete   alt-↑/↓ reorder   tab switch pane   ^H help   ^Q quit"
    );
}

fn modifiers(chord: Chord) -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    if chord.ctrl {
        modifiers |= CTRL;
    }
    if chord.alt {
        modifiers |= ALT;
    }
    modifiers
}

/// One row of a rendered frame, as text.
fn row(buffer: &Buffer, y: u16) -> String {
    let width = buffer.area.width as usize;
    buffer.content()[y as usize * width..][..width]
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
