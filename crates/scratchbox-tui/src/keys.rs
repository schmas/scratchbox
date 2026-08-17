//! The keymap.
//!
//! Anything not claimed here belongs to the editor. The editor is the point of the app, so
//! its keys are not shadowed for app-level shortcuts.
//!
//! [`BINDINGS`] is the normal-mode keymap: dispatch scans it, and everything that prints a
//! key — the status line, and the keybindings panel — reads it. A rebind therefore moves the
//! behaviour and the text that describes it together. The two prompt keymaps below are the
//! exception, because their rule is a catch-all rather than a set of chords.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Focus;

/// A chord, matched on `(code, ctrl, alt)` exactly.
///
/// SHIFT is deliberately not compared: crossterm sets it for every uppercase character and
/// for nothing this app binds, so comparing it would reject chords nobody typed differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl Chord {
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
        }
    }

    pub const fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: true,
            alt: false,
        }
    }

    pub const fn alt(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: true,
        }
    }

    pub fn matches(&self, key: KeyEvent) -> bool {
        self.code == key.code
            && self.ctrl == key.modifiers.contains(KeyModifiers::CONTROL)
            && self.alt == key.modifiers.contains(KeyModifiers::ALT)
    }

    /// How this chord is printed, in the app's existing vocabulary: `^N`, `alt-↑`, `tab`.
    ///
    /// Derived rather than stored, so a binding whose printed caption is just its first chord
    /// does not have to repeat it.
    pub fn caption(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push('^');
        }
        if self.alt {
            out.push_str("alt-");
        }
        match self.code {
            // Uppercase only alongside a modifier, which is where `^N` reads as a chord
            // rather than as a letter to type.
            KeyCode::Char(c) if self.ctrl => out.push(c.to_ascii_uppercase()),
            KeyCode::Char(c) => out.push(c),
            KeyCode::Up => out.push('↑'),
            KeyCode::Down => out.push('↓'),
            KeyCode::Tab => out.push_str("tab"),
            KeyCode::Enter => out.push('⏎'),
            KeyCode::Esc => out.push_str("esc"),
            KeyCode::F(n) => out.push_str(&format!("F{n}")),
            other => out.push_str(&format!("{other:?}").to_lowercase()),
        }
        out
    }
}

/// Which pane a binding applies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Global,
    ListOnly,
}

/// How the panel groups the bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Notes,
    Navigation,
    Panes,
    General,
}

/// An app-level command: exactly the set [`BINDINGS`] declares.
///
/// Separate from [`Action`] so the table's completeness test is a `match` the compiler
/// checks, rather than a list of exemptions maintained by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Quit,
    NewNote,
    RequestDelete,
    MoveNoteUp,
    MoveNoteDown,
    SelectPrevious,
    SelectNext,
    ToggleFocus,
    /// Show the keybindings panel. No chord produces it yet — the panel it opens does not
    /// exist, and claiming a key for a no-op would take a working editor key away for nothing.
    OpenHelp,
}

/// One documented binding. This table is the keymap.
pub struct Binding {
    pub chords: &'static [Chord],
    /// What the panel prints. Covers every chord of the binding at once: `^Q/^C`.
    pub caption: &'static str,
    /// Prose for the panel. The status line abbreviates independently, so the two share the
    /// key and the command rather than the wording — both are right at their own width.
    ///
    /// Written with search in mind: the panel's filter matches over this text, so the words
    /// a user would look for belong here.
    pub desc: &'static str,
    pub section: Section,
    pub ctx: Ctx,
    pub command: Command,
}

impl Binding {
    fn applies(&self, focus: Focus) -> bool {
        match self.ctx {
            Ctx::Global => true,
            Ctx::ListOnly => focus == Focus::List,
        }
    }
}

/// The normal-mode keymap, in declaration order.
///
/// The reorder bindings come before the plain arrows. Exact matching means the two cannot
/// both answer one event, so this is not a correctness requirement — but it keeps the table
/// readable and survives anyone reintroducing a wildcard.
pub static BINDINGS: &[Binding] = &[
    Binding {
        chords: &[Chord::alt(KeyCode::Up)],
        caption: "alt-↑",
        desc: "move this note up — reorder the list",
        section: Section::Navigation,
        ctx: Ctx::Global,
        command: Command::MoveNoteUp,
    },
    Binding {
        chords: &[Chord::alt(KeyCode::Down)],
        caption: "alt-↓",
        desc: "move this note down — reorder the list",
        section: Section::Navigation,
        ctx: Ctx::Global,
        command: Command::MoveNoteDown,
    },
    Binding {
        chords: &[Chord::plain(KeyCode::Up)],
        caption: "↑",
        desc: "select the previous note",
        section: Section::Navigation,
        ctx: Ctx::ListOnly,
        command: Command::SelectPrevious,
    },
    Binding {
        chords: &[Chord::plain(KeyCode::Down)],
        caption: "↓",
        desc: "select the next note",
        section: Section::Navigation,
        ctx: Ctx::ListOnly,
        command: Command::SelectNext,
    },
    Binding {
        chords: &[Chord::ctrl(KeyCode::Char('n'))],
        caption: "^N",
        desc: "new note",
        section: Section::Notes,
        ctx: Ctx::Global,
        command: Command::NewNote,
    },
    Binding {
        chords: &[Chord::ctrl(KeyCode::Char('d'))],
        caption: "^D",
        desc: "delete this note, after a confirmation",
        section: Section::Notes,
        ctx: Ctx::Global,
        command: Command::RequestDelete,
    },
    Binding {
        // The second chord keeps a widely-held pane-switching convention working. The caption
        // stays `tab`: it exists so the habit does not break, not to be advertised as a second
        // way to do the same trivial thing.
        chords: &[Chord::plain(KeyCode::Tab), Chord::ctrl(KeyCode::Tab)],
        caption: "tab",
        desc: "switch pane between the note list and the editor",
        section: Section::Panes,
        ctx: Ctx::Global,
        command: Command::ToggleFocus,
    },
    Binding {
        // Ctrl-C arrives as a key event rather than a signal in raw mode, so quitting stays
        // on the ordinary path where the terminal is restored on the way out.
        chords: &[
            Chord::ctrl(KeyCode::Char('q')),
            Chord::ctrl(KeyCode::Char('c')),
        ],
        caption: "^Q/^C",
        desc: "save and quit",
        section: Section::General,
        ctx: Ctx::Global,
        command: Command::Quit,
    },
];

/// The binding that declares `command`.
///
/// Every command in the enum has exactly one row, which `tests/keys_table.rs` checks
/// exhaustively — a variant with no row fails that test to compile.
pub fn binding(command: Command) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.command == command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A command from the table.
    Do(Command),
    ConfirmDelete,
    CancelDelete,
    KeepMine,
    TakeTheirs,
    /// Hand the key to the editor.
    Edit(KeyEvent),
    Ignore,
}

/// What a key means while a delete is waiting to be confirmed.
///
/// Everything else is swallowed: a stray keystroke must not answer a question about
/// destroying a note, and must not leak into the buffer behind the prompt either.
pub fn map_confirmation(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::ConfirmDelete,
        _ => Action::CancelDelete,
    }
}

/// What a key means while an external change is waiting to be resolved.
///
/// Everything else is swallowed, exactly as for a delete. The buffer's fate is undecided,
/// and letting the user pile more edits onto it would only make the choice harder — and
/// would let a stray `k` answer a question about whose version of a note survives.
///
/// Quit is the one way out that is not an answer: it leaves the external change on disk
/// and drops the buffer, which beats trapping the user in a prompt.
pub fn map_conflict(key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Char('q'), true) | (KeyCode::Char('c'), true) => Action::Do(Command::Quit),
        (KeyCode::Char('k') | KeyCode::Char('K'), false) => Action::KeepMine,
        (KeyCode::Char('t') | KeyCode::Char('T'), false) => Action::TakeTheirs,
        _ => Action::Ignore,
    }
}

/// What a key means in normal mode: a scan over the one table that declares them.
pub fn map(key: KeyEvent, focus: Focus) -> Action {
    for binding in BINDINGS {
        if binding.applies(focus) && binding.chords.iter().any(|chord| chord.matches(key)) {
            return Action::Do(binding.command);
        }
    }

    // Unchanged: anything not claimed above belongs to the editor.
    if focus == Focus::Editor {
        Action::Edit(key)
    } else {
        Action::Ignore
    }
}
