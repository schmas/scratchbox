//! Phase 1 spike: does `edtui 0.11.6` actually work as Phase 6 assumes?
//!
//! Three questions, answered by running this rather than by reading metadata:
//!
//! 1. Does `EditorView` render through the same `Widget` trait `ratatui 0.30.2` exposes?
//!    That only holds if both crates resolve to one `ratatui-core`.
//! 2. Is `emacs_mode()` genuinely modeless — do typing, arrows, Home/End and Backspace
//!    all work without a mode switch?
//! 3. Is undo/redo reachable from keys, not only from the action API?
//!
//! Headless on purpose: it feeds scripted `crossterm` key events and renders into a
//! `Buffer`, so it answers the same questions in CI as it does on a terminal.
//!
//! Run with `cargo run -p scratchbox-tui --example spike_editor`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{EditorEventHandler, EditorMode, EditorState, EditorView, Lines};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

fn main() {
    let mut results = Results::default();

    results.check("typing inserts text", typing_inserts_text());
    results.check("editing stays in one mode", editing_stays_in_one_mode());
    results.check("home/end move within the line", home_and_end_move());
    results.check("backspace deletes backwards", backspace_deletes());
    results.check("arrows move the cursor", arrows_move_cursor());
    results.check("ctrl+u undo, ctrl+r redo", undo_and_redo());
    results.check(
        "EditorView renders into a ratatui Buffer",
        renders_into_buffer(),
    );

    results.report();
}

/// A fresh editor in the state the TUI will keep it in: Insert, where every emacs
/// binding lives.
fn editor(text: &str) -> (EditorState, EditorEventHandler) {
    let mut state = EditorState::new(Lines::from(text));
    // `EditorState::new` starts in Normal. The emacs bindings are all registered against
    // Insert, so a modeless editor has to opt in here — see the spike findings.
    state.mode = EditorMode::Insert;
    (state, EditorEventHandler::emacs_mode())
}

fn press(handler: &mut EditorEventHandler, state: &mut EditorState, code: KeyCode) {
    handler.on_key_event(KeyEvent::new(code, KeyModifiers::NONE), state);
}

fn press_ctrl(handler: &mut EditorEventHandler, state: &mut EditorState, c: char) {
    handler.on_key_event(
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        state,
    );
}

fn type_str(handler: &mut EditorEventHandler, state: &mut EditorState, text: &str) {
    for c in text.chars() {
        press(handler, state, KeyCode::Char(c));
    }
}

fn text_of(state: &EditorState) -> String {
    String::from(state.lines.clone())
}

fn typing_inserts_text() -> Outcome {
    let (mut state, mut handler) = editor("");
    type_str(&mut handler, &mut state, "hello");
    Outcome::eq("hello", &text_of(&state))
}

fn editing_stays_in_one_mode() -> Outcome {
    let (mut state, mut handler) = editor("");
    type_str(&mut handler, &mut state, "abc");
    press(&mut handler, &mut state, KeyCode::Left);
    press(&mut handler, &mut state, KeyCode::Backspace);
    type_str(&mut handler, &mut state, "X");
    Outcome::eq(
        format!("{:?}", EditorMode::Insert),
        &format!("{:?}", state.mode),
    )
}

fn home_and_end_move() -> Outcome {
    let (mut state, mut handler) = editor("hello");
    press(&mut handler, &mut state, KeyCode::End);
    let at_end = state.cursor.col;
    press(&mut handler, &mut state, KeyCode::Home);
    let at_start = state.cursor.col;
    // End lands on the last character (4) or one past it (5); either is a real move.
    Outcome::eq("true", &format!("{}", at_end >= 4 && at_start == 0))
}

fn backspace_deletes() -> Outcome {
    let (mut state, mut handler) = editor("");
    type_str(&mut handler, &mut state, "hello");
    press(&mut handler, &mut state, KeyCode::Backspace);
    Outcome::eq("hell", &text_of(&state))
}

fn arrows_move_cursor() -> Outcome {
    let (mut state, mut handler) = editor("ab\ncd");
    press(&mut handler, &mut state, KeyCode::Down);
    press(&mut handler, &mut state, KeyCode::Right);
    Outcome::eq(
        "row 1 col 1",
        &format!("row {} col {}", state.cursor.row, state.cursor.col),
    )
}

fn undo_and_redo() -> Outcome {
    let (mut state, mut handler) = editor("");
    type_str(&mut handler, &mut state, "hello");
    press_ctrl(&mut handler, &mut state, 'u');
    let after_undo = text_of(&state);
    press_ctrl(&mut handler, &mut state, 'r');
    let after_redo = text_of(&state);

    if after_undo == "hello" {
        return Outcome::fail("ctrl+u did not change the buffer".into());
    }
    if after_redo != "hello" {
        return Outcome::fail(format!(
            "ctrl+r restored {after_redo:?}, expected \"hello\""
        ));
    }
    Outcome::Pass
}

fn renders_into_buffer() -> Outcome {
    let (mut state, _) = editor("hello spike");
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);

    // The load-bearing line: `EditorView` satisfies the `Widget` trait re-exported by
    // ratatui 0.30.2. It would not compile if edtui linked a second ratatui-core.
    EditorView::new(&mut state).render(area, &mut buf);

    let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
    if rendered.contains("hello spike") {
        Outcome::Pass
    } else {
        Outcome::fail("rendered buffer did not contain the editor text".into())
    }
}

enum Outcome {
    Pass,
    Fail(String),
}

impl Outcome {
    fn eq(expected: impl AsRef<str>, actual: &str) -> Self {
        let expected = expected.as_ref();
        if expected == actual {
            Self::Pass
        } else {
            Self::fail(format!("expected {expected:?}, got {actual:?}"))
        }
    }

    fn fail(reason: String) -> Self {
        Self::Fail(reason)
    }
}

#[derive(Default)]
struct Results {
    failed: usize,
}

impl Results {
    fn check(&mut self, name: &str, outcome: Outcome) {
        match outcome {
            Outcome::Pass => println!("PASS  {name}"),
            Outcome::Fail(reason) => {
                self.failed += 1;
                println!("FAIL  {name}: {reason}");
            }
        }
    }

    fn report(&self) {
        if self.failed == 0 {
            println!(
                "\nedtui spike passed: ratatui 0.30.2 compatible, modeless, undo/redo reachable."
            );
        } else {
            println!(
                "\nedtui spike FAILED {} check(s) — escalate before Phase 6.",
                self.failed
            );
            std::process::exit(1);
        }
    }
}
