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

impl Section {
    /// The panel's heading for this section.
    pub fn title(&self) -> &'static str {
        match self {
            Section::Notes => "Notes",
            Section::Navigation => "Navigation",
            Section::Panes => "Panes",
            Section::General => "General",
        }
    }
}

/// The order the panel lists the derived sections in.
///
/// Not the table's declaration order: that one is grouped for reading the keymap, this one
/// leads with what a user came to look up.
const SECTION_ORDER: &[Section] = &[
    Section::Notes,
    Section::Navigation,
    Section::Panes,
    Section::General,
];

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
    /// Show the keybindings panel.
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
    Binding {
        // `^H` is reachable from the editor, which `?` would not be, and it takes nothing the
        // user cannot do another way: it shadows edtui's delete-character-backward, which is
        // what Backspace already does. `F1` is the alias rather than the primary because
        // macOS needs `fn` held with it unless the standard-function-keys setting is on.
        chords: &[Chord::ctrl(KeyCode::Char('h')), Chord::plain(KeyCode::F(1))],
        caption: "^H/F1",
        desc: "show this list of keybindings",
        section: Section::General,
        ctx: Ctx::Global,
        command: Command::OpenHelp,
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

/// What a key means while the keybindings panel is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpKey {
    Close,
    Up,
    Down,
    Top,
    Bottom,
    /// Perform the selected binding.
    Run,
    /// Start filtering the list.
    Search,
    /// A character for the filter.
    Type(char),
    Backspace,
    /// Leave the filter prompt, keeping the query.
    SearchCommit,
    /// Leave the filter prompt and clear the query.
    SearchCancel,
    Quit,
    Ignore,
}

/// What a key means while the keybindings panel is open.
///
/// Hardcoded like the other two prompt keymaps, and for the same reason: its rules include
/// catch-alls rather than an enumerable set of chords.
///
/// Quit stays live, exactly as it does while an external change is unresolved: a user must
/// never be trapped inside a modal.
///
/// `searching` decides whether a printable key moves the cursor or goes into the filter, so it
/// is examined before anything else.
pub fn map_help(key: KeyEvent, searching: bool) -> HelpKey {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if searching {
        let ctrl_or_alt = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        return match key.code {
            // Quit survives even in here. Inside the prompt `q` is a character, so this is
            // the only way out that is not one of the two below.
            KeyCode::Char('q' | 'c') if ctrl => HelpKey::Quit,
            KeyCode::Esc => HelpKey::SearchCancel,
            KeyCode::Enter => HelpKey::SearchCommit,
            KeyCode::Backspace => HelpKey::Backspace,
            // SHIFT is not rejected: crossterm sets it for every uppercase character, and
            // rejecting it would make the filter lowercase-only.
            KeyCode::Char(c) if !ctrl_or_alt => HelpKey::Type(c),
            _ => HelpKey::Ignore,
        };
    }

    match (key.code, ctrl) {
        (KeyCode::Char('q') | KeyCode::Char('c'), true) => HelpKey::Quit,
        (KeyCode::Esc, _) => HelpKey::Close,
        (KeyCode::Up | KeyCode::Char('k'), false) => HelpKey::Up,
        (KeyCode::Down | KeyCode::Char('j'), false) => HelpKey::Down,
        (KeyCode::Home, false) => HelpKey::Top,
        (KeyCode::End, false) => HelpKey::Bottom,
        (KeyCode::Char('/'), false) => HelpKey::Search,
        (KeyCode::Enter, false) => HelpKey::Run,
        // Everything else is swallowed rather than passed on, as the two prompts do: the
        // panel covers the panes, and a key that reached the buffer from behind it would
        // arrive somewhere the user cannot see.
        _ => HelpKey::Ignore,
    }
}

/// One row the panel lists.
///
/// `run` is what `⏎` performs, which is deliberately not the binding the row displays. It is
/// `None` for three different reasons:
///   - a literal row has no single command behind it at all;
///   - opening the panel from an already-open panel is a no-op;
///   - quitting would exit the app from a panel opened to read, and `^Q` still works as a
///     keypress.
///
/// Rows that cannot be run are dimmed, so `⏎` doing nothing reads as an answer rather than a
/// bug.
#[derive(Clone, Copy)]
pub struct HelpRow {
    pub caption: &'static str,
    pub desc: &'static str,
    pub run: Option<Command>,
}

impl HelpRow {
    fn from_binding(binding: &'static Binding) -> Self {
        Self {
            caption: binding.caption,
            desc: binding.desc,
            run: match binding.command {
                Command::Quit | Command::OpenHelp => None,
                command => Some(command),
            },
        }
    }
}

#[derive(Clone)]
pub struct HelpSection {
    pub title: &'static str,
    pub rows: Vec<HelpRow>,
}

/// Every section the panel lists: the ones derived from the table, then the ones written out
/// for the keymaps that cannot be.
pub fn help_sections() -> Vec<HelpSection> {
    let mut sections: Vec<HelpSection> = SECTION_ORDER
        .iter()
        .map(|section| HelpSection {
            title: section.title(),
            rows: BINDINGS
                .iter()
                .filter(|binding| binding.section == *section)
                .map(HelpRow::from_binding)
                .collect(),
        })
        .collect();
    sections.extend(literal_sections());
    sections
}

/// The keymaps that are not tabled, transcribed from the code that decides them.
///
/// Their rule is a catch-all — "anything else cancels" — rather than a set of chords, so there
/// is nothing to walk. Every caption below is fed through the function it describes by a test,
/// and checked against the prose the prompt itself prints, so the two cannot drift apart
/// without something going red.
fn literal_sections() -> Vec<HelpSection> {
    let row = |caption, desc| HelpRow {
        caption,
        desc,
        run: None,
    };

    vec![
        HelpSection {
            title: "Delete prompt",
            rows: vec![
                row("y/⏎", "confirm the delete"),
                row("any other key", "cancel the delete"),
            ],
        },
        HelpSection {
            title: "External change",
            rows: vec![
                row("k", "keep mine — write my buffer over theirs"),
                row("t", "take theirs — discard my edits"),
                row("^Q", "quit without saving"),
            ],
        },
        HelpSection {
            title: "This panel",
            rows: vec![
                row("esc", "close this panel"),
                row("↑/↓", "select a binding"),
                row("⏎", "run the selected binding"),
                row("/", "search this list"),
            ],
        },
        HelpSection {
            title: "Editor",
            rows: vec![
                row(
                    "",
                    "the editor is emacs-style and modeless — there is no mode to leave",
                ),
                row(
                    "^N ^D ^H",
                    "taken by scratchbox: edtui's own meanings do not apply to them",
                ),
            ],
        },
    ]
}

/// The rows matching `query`, case-insensitively over caption and description.
///
/// Sections left with nothing disappear. An empty query is the identity, so the filtered walk
/// is the only walk there is — the cursor, the panel, and `⏎` all index into this.
pub fn filter(sections: &[HelpSection], query: &str) -> Vec<HelpSection> {
    if query.is_empty() {
        return sections.to_vec();
    }
    let needle = query.to_lowercase();

    sections
        .iter()
        .filter_map(|section| {
            let rows: Vec<HelpRow> = section
                .rows
                .iter()
                .filter(|row| {
                    contains_ignore_case(row.caption, &needle)
                        || contains_ignore_case(row.desc, &needle)
                })
                .copied()
                .collect();
            if rows.is_empty() {
                None
            } else {
                Some(HelpSection {
                    title: section.title,
                    rows,
                })
            }
        })
        .collect()
}

/// Substring search that folds ASCII case without allocating.
///
/// The haystacks are `&'static str` and there are twenty of them to scan on every keystroke,
/// so lowercasing each one per key would allocate the whole table repeatedly. Only ASCII case
/// is folded, which covers everything here: the captions and descriptions are ASCII apart
/// from arrows, and an arrow has no case.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// How many selectable rows the panel lists.
pub fn help_row_count(sections: &[HelpSection]) -> usize {
    sections.iter().map(|section| section.rows.len()).sum()
}

/// How many body lines those sections occupy: a heading per section, its rows, and a blank
/// line between sections.
///
/// Lines and rows are different counts, and both are needed: the cursor addresses rows, the
/// scrolling window addresses lines. Rendering lays the body out to this shape, and a test
/// holds the two to the same number.
pub fn help_line_count(sections: &[HelpSection]) -> usize {
    help_row_count(sections) + sections.len() + sections.len().saturating_sub(1)
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
