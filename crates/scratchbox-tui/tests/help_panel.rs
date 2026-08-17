//! The keybindings panel: who owns the keyboard while it is open, and what it says.
//!
//! Driven headlessly through `input::handle_key`, which is the real routing the binary uses —
//! a test that reimplemented the ownership order would only prove its own copy was consistent.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use scratchbox_core::order::OrderStore;
use scratchbox_core::{APP_SUBDIR, FolderSync, NoteId, StoreEvent, WorkspaceHealth};
use scratchbox_tui::app::{App, Focus, IDLE_SAVE};
use scratchbox_tui::editor::EditorPane;
use scratchbox_tui::keys::{self, Action, BINDINGS, Command, HelpRow, HelpSection};
use scratchbox_tui::{input, ui};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    workspace: PathBuf,
    app: App,
}

fn fixture(notes: &[(&str, &str)]) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();

    for (name, body) in notes {
        fs::write(workspace.join(name), body).unwrap();
    }
    if !notes.is_empty() {
        let ids: Vec<NoteId> = notes.iter().map(|(name, _)| id(name)).collect();
        OrderStore::new(&workspace.join(APP_SUBDIR))
            .save(&ids)
            .unwrap();
    }

    let app = App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        workspace,
        app,
    }
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).unwrap()
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn press(app: &mut App, key: KeyEvent) {
    input::handle_key(app, key).unwrap();
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, plain(KeyCode::Char(c)));
    }
}

/// One frame, drawn off-screen.
fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::render(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// The whole frame as text, one string per row.
fn text_rows(buffer: &Buffer) -> Vec<String> {
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn rendered(app: &mut App) -> Vec<String> {
    text_rows(&draw(app, 120, 40))
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    text_rows(&draw(app, width, height)).join("\n")
}

/// The panel's box, found by the title on its top border.
fn panel_box(buffer: &Buffer) -> Rect {
    let rows = text_rows(buffer);
    let y = rows
        .iter()
        .position(|row| row.contains("Keybindings"))
        .expect("the panel is not on screen");
    let title: Vec<char> = rows[y].chars().collect();
    let x = title.iter().position(|c| *c == '┌').unwrap();
    let right = title.iter().rposition(|c| *c == '┐').unwrap();
    let bottom = rows
        .iter()
        .skip(y)
        .position(|row| row.contains('└'))
        .unwrap();

    Rect::new(
        x as u16,
        y as u16,
        (right - x + 1) as u16,
        (bottom + 1) as u16,
    )
}

fn rows(sections: &[HelpSection]) -> Vec<&HelpRow> {
    sections.iter().flat_map(|section| &section.rows).collect()
}

fn section<'a>(sections: &'a [HelpSection], title: &str) -> &'a HelpSection {
    sections
        .iter()
        .find(|section| section.title == title)
        .unwrap_or_else(|| panic!("no {title} section"))
}

// --- opening and ownership -------------------------------------------------------------

#[test]
fn ctrl_h_opens_the_panel_from_the_editor_without_typing_anything() {
    let mut f = fixture(&[("a.md", "text")]);
    assert_eq!(f.app.focus(), Focus::Editor);

    press(&mut f.app, ctrl('h'));

    assert!(f.app.help().is_some());
    assert_eq!(f.app.editor().text(), "text");
}

#[test]
fn f1_opens_the_panel_from_either_pane() {
    for focus in [Focus::List, Focus::Editor] {
        let mut f = fixture(&[("a.md", "text")]);
        if f.app.focus() != focus {
            f.app.toggle_focus();
        }

        press(&mut f.app, plain(KeyCode::F(1)));

        assert!(f.app.help().is_some(), "F1 did not open it in {focus:?}");
    }
}

/// The panel's key was chosen so that this stays true: `?` is a character, not a binding.
#[test]
fn a_question_mark_still_reaches_the_buffer() {
    let mut f = fixture(&[("a.md", "")]);

    press(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
    );

    assert_eq!(f.app.editor().text(), "?");
    assert!(f.app.help().is_none());
}

#[test]
fn an_open_panel_swallows_the_keys_the_editor_would_have_taken() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    type_text(&mut f.app, "abc");

    assert_eq!(f.app.editor().text(), "text");
    assert!(f.app.help().is_some());
}

#[test]
fn an_open_panel_swallows_the_app_shortcuts_too() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    press(&mut f.app, ctrl('n'));
    press(&mut f.app, ctrl('d'));

    assert_eq!(f.app.notes().len(), 1, "^N created a note from the panel");
    assert!(
        f.app.pending_delete().is_none(),
        "^D opened the delete prompt from the panel"
    );
}

#[test]
fn esc_closes_the_panel() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    press(&mut f.app, plain(KeyCode::Esc));

    assert!(f.app.help().is_none());
    assert!(!f.app.should_quit());
}

#[test]
fn quitting_from_the_panel_closes_it_first() {
    for key in [ctrl('q'), ctrl('c')] {
        let mut f = fixture(&[("a.md", "text")]);
        press(&mut f.app, ctrl('h'));

        press(&mut f.app, key);

        assert!(f.app.help().is_none(), "{key:?} left the panel open");
        assert!(f.app.should_quit());
    }
}

/// I8's core case. A refused quit says so in the status line, and the status line is behind
/// the panel — so the panel has to be gone before the refusal is written, or the user presses
/// quit again and the second press goes through with the buffer unwritten.
#[test]
fn a_quit_refused_by_a_failed_save_leaves_the_message_where_it_can_be_seen() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");

    // A note that cannot be written while the workspace itself is still fine: health stays
    // Ok, so the save is attempted for real and fails on the note.
    fs::remove_file(f.workspace.join("a.md")).unwrap();
    fs::create_dir(f.workspace.join("a.md")).unwrap();

    press(&mut f.app, ctrl('h'));
    press(&mut f.app, ctrl('q'));

    assert_eq!(f.app.health(), WorkspaceHealth::Ok);
    assert!(!f.app.should_quit(), "a failed save let the app exit");
    assert!(f.app.help().is_none(), "the message is behind the panel");
    assert!(f.app.status().is_some(), "the user was not told");
}

/// I8's other half: a workspace that cannot be written at all writes nothing and says so in
/// the banner, and quitting from the panel still lets that banner be seen.
#[test]
fn a_quit_under_a_degraded_workspace_leaves_the_banner_visible() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::remove_dir_all(&f.workspace).unwrap();

    press(&mut f.app, ctrl('h'));
    press(&mut f.app, ctrl('q'));

    assert_eq!(f.app.health(), WorkspaceHealth::Missing);
    assert!(f.app.help().is_none());
    assert!(f.app.should_quit());
}

#[test]
fn the_panel_cannot_be_opened_while_an_external_change_is_unresolved() {
    let mut f = raised_conflict();

    press(&mut f.app, ctrl('h'));

    assert!(f.app.help().is_none(), "the panel opened over the prompt");
    assert!(f.app.conflict().is_some());
}

#[test]
fn an_external_change_arriving_closes_the_panel() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    press(&mut f.app, ctrl('h'));
    assert!(f.app.help().is_some());

    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();

    assert!(f.app.conflict().is_some());
    assert!(f.app.help().is_none(), "two prompts on screen at once");
}

/// The panel is the longest-lived modal in the app, so the half of the loop that is not the
/// keyboard has to keep running underneath it.
#[test]
fn an_armed_autosave_fires_while_the_panel_is_open() {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    press(&mut f.app, ctrl('h'));

    assert!(
        f.app.wake_at().is_some(),
        "nothing was pending to begin with"
    );
    thread::sleep(IDLE_SAVE + Duration::from_millis(50));
    f.app.on_tick().unwrap();

    assert_eq!(
        fs::read_to_string(f.workspace.join("a.md")).unwrap(),
        "mine original"
    );
    assert!(f.app.help().is_some(), "the panel closed on a tick");
}

// --- the cursor and the view -----------------------------------------------------------

#[test]
fn the_cursor_stops_at_both_ends_and_jumps_to_them() {
    let mut f = fixture(&[("a.md", "text")]);
    let last = keys::help_row_count(&keys::help_sections()) - 1;
    press(&mut f.app, ctrl('h'));

    press(&mut f.app, plain(KeyCode::Up));
    assert_eq!(f.app.help().unwrap().cursor(), 0, "the cursor wrapped");

    press(&mut f.app, plain(KeyCode::Down));
    assert_eq!(f.app.help().unwrap().cursor(), 1);
    press(&mut f.app, plain(KeyCode::Char('k')));
    assert_eq!(f.app.help().unwrap().cursor(), 0);
    press(&mut f.app, plain(KeyCode::Char('j')));
    assert_eq!(f.app.help().unwrap().cursor(), 1);

    press(&mut f.app, plain(KeyCode::End));
    assert_eq!(f.app.help().unwrap().cursor(), last);
    press(&mut f.app, plain(KeyCode::Down));
    assert_eq!(f.app.help().unwrap().cursor(), last, "the cursor ran off");

    press(&mut f.app, plain(KeyCode::Home));
    assert_eq!(f.app.help().unwrap().cursor(), 0);
}

#[test]
fn a_resize_is_recorded_and_pulls_the_view_back_inside_the_list() {
    let mut f = fixture(&[("a.md", "text")]);
    let lines = keys::help_line_count(&keys::help_sections());
    press(&mut f.app, ctrl('h'));

    // A short terminal: the list does not fit, so the view can legitimately be scrolled.
    f.app.set_size(80, 12);
    assert_eq!(f.app.last_size(), (80, 12));
    f.app.help_scroll(100);
    let scrolled = f.app.help().unwrap().offset();
    assert!(scrolled > 0 && scrolled < lines, "offset {scrolled}");

    // Tall enough for the whole list: there is nothing left to scroll past.
    f.app.set_size(120, 40);
    assert_eq!(f.app.help().unwrap().offset(), 0);
}

// --- what the panel says -----------------------------------------------------------------

#[test]
fn every_section_lists_something() {
    for section in keys::help_sections() {
        assert!(!section.rows.is_empty(), "{} is empty", section.title);
    }
}

#[test]
fn every_binding_appears_with_its_own_caption_and_description() {
    let sections = keys::help_sections();
    let rows = rows(&sections);

    for binding in BINDINGS {
        let row = rows
            .iter()
            .find(|row| row.caption == binding.caption)
            .unwrap_or_else(|| panic!("{} is not listed", binding.caption));
        assert_eq!(row.desc, binding.desc);
    }
}

/// `⏎` runs a row, and three kinds of row deliberately do nothing: the literal ones, because
/// they describe a prompt that is not open; quit, because Enter landing on it while browsing
/// would exit the app; and the panel's own key, because it is already open.
#[test]
fn only_the_literal_rows_and_the_two_exceptions_cannot_be_run() {
    let sections = keys::help_sections();
    let inert: Vec<&str> = rows(&sections)
        .iter()
        .filter(|row| row.run.is_none())
        .map(|row| row.caption)
        .collect();

    assert_eq!(
        inert,
        vec![
            "^Q/^C",
            "^H/F1",
            "y/⏎",
            "any other key",
            "k",
            "t",
            "^Q",
            "esc",
            "↑/↓",
            "⏎",
            "/",
            "",
            "^N ^D ^H",
        ]
    );
}

/// The editor's own keymap is edtui's, and restating it here would be a copy no test could
/// guard. What the panel owes the user is the model it is in and the keys scratchbox takes
/// out of it.
#[test]
fn the_editor_section_names_the_model_and_the_chords_taken_from_it() {
    let sections = keys::help_sections();
    let editor = section(&sections, "Editor");
    let text: String = editor
        .rows
        .iter()
        .map(|row| format!("{} {}", row.caption, row.desc))
        .collect::<Vec<_>>()
        .join(" ");

    for claim in ["emacs", "modeless", "^N", "^D", "^H"] {
        assert!(
            text.contains(claim),
            "the Editor section never says {claim}"
        );
    }
}

/// The prose above is checkable, so it is checked: the editor really is in emacs mode, and
/// really has no mode to leave.
#[test]
fn the_editor_is_emacs_style_and_modeless() {
    let mut editor = EditorPane::new();
    editor.load(id("a.md"), "abc");

    // Modeless: a plain character is text, with nothing pressed first to make it so.
    editor.on_key(plain(KeyCode::Char('X')));
    assert_eq!(editor.text(), "Xabc");

    // Emacs-style: ^E is end-of-line, which no other keymap edtui ships binds there.
    editor.on_key(ctrl('e'));
    editor.on_key(plain(KeyCode::Char('Z')));
    assert_eq!(editor.text(), "XabcZ");
}

/// The rows for the two prompts are written out by hand, because their rule is "anything else
/// cancels" rather than a set of chords. This is what stops that from drifting: every row is
/// fed through the function that decides it, and checked against the words the prompt itself
/// puts on screen.
#[test]
fn the_literal_rows_state_what_the_code_does_and_what_the_prompts_print() {
    assert_eq!(
        keys::map_confirmation(plain(KeyCode::Char('y'))),
        Action::ConfirmDelete
    );
    assert_eq!(
        keys::map_confirmation(plain(KeyCode::Enter)),
        Action::ConfirmDelete
    );
    assert_eq!(
        keys::map_confirmation(plain(KeyCode::Char('x'))),
        Action::CancelDelete
    );

    assert_eq!(
        keys::map_conflict(plain(KeyCode::Char('k'))),
        Action::KeepMine
    );
    assert_eq!(
        keys::map_conflict(plain(KeyCode::Char('t'))),
        Action::TakeTheirs
    );
    assert_eq!(keys::map_conflict(ctrl('q')), Action::Do(Command::Quit));

    let sections = keys::help_sections();
    assert_prompt_agrees(&sections, "Delete prompt", &delete_prompt_on_screen());
    assert_prompt_agrees(&sections, "External change", &conflict_prompt_on_screen());
}

/// Every row of `title` names a key and a verb the prompt itself also prints.
///
/// Derived from the rows rather than restated, so editing a row without editing the prompt —
/// or the other way round — is what fails here.
fn assert_prompt_agrees(sections: &[HelpSection], title: &str, on_screen: &str) {
    for row in &section(sections, title).rows {
        let key = row.caption.split('/').next().unwrap();
        let verb = row.desc.split_whitespace().next().unwrap();

        assert!(
            on_screen.contains(key),
            "{title}: the prompt never shows {key}"
        );
        assert!(
            on_screen.contains(verb),
            "{title}: the prompt never says {verb}"
        );
    }
}

fn delete_prompt_on_screen() -> String {
    let mut f = fixture(&[("a.md", "text")]);
    f.app.request_delete();
    rendered(&mut f.app).join("\n")
}

fn conflict_prompt_on_screen() -> String {
    let mut f = raised_conflict();
    rendered(&mut f.app).join("\n")
}

// --- drawing it ---------------------------------------------------------------------------

/// The panel is the one thing on screen sized by arithmetic rather than by a layout, and an
/// underflow here panics inside the draw closure — which takes the process and the unsaved
/// buffer with it. So the arithmetic is required to be total on its own terms.
#[test]
fn the_panels_box_is_total_for_every_size() {
    for width in 0..40u16 {
        for height in 0..40u16 {
            for (content, body) in [(0, 0), (40, 35), (u16::MAX, u16::MAX)] {
                let rect = ui::help_rect(content, body, Rect::new(0, 0, width, height));

                assert!(
                    rect.width >= 2 && rect.height >= 2,
                    "{width}x{height} with content {content} gave {rect:?}"
                );
            }
        }
    }
}

#[test]
fn the_open_panel_lists_the_keymap() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    let screen = screen(&mut f.app, 120, 40);

    for shown in [
        "Keybindings",
        "^N",
        "new note",
        "alt-↑",
        "^Q/^C",
        "emacs",
        "Navigation",
    ] {
        assert!(screen.contains(shown), "the panel never shows {shown}");
    }
}

#[test]
fn a_closed_panel_shows_none_of_it() {
    let mut f = fixture(&[("a.md", "text")]);

    let screen = screen(&mut f.app, 120, 40);

    for hidden in ["Keybindings", "^Q/^C", "emacs"] {
        assert!(!screen.contains(hidden), "{hidden} is on screen unopened");
    }
}

/// It floats over the panes rather than replacing them: the margin is what keeps the app
/// visible around it, so the user can see what they are about to go back to.
#[test]
fn the_panes_stay_visible_around_the_panel() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    let rows = rendered(&mut f.app);

    assert!(
        rows[0].contains("Notes"),
        "the list pane's title is covered"
    );
    assert!(
        rows[0].contains("a.md"),
        "the editor pane's title is covered"
    );
}

#[test]
fn the_bottom_border_counts_the_rows() {
    let mut f = fixture(&[("a.md", "text")]);
    let rows = keys::help_row_count(&keys::help_sections());
    press(&mut f.app, ctrl('h'));

    assert!(screen(&mut f.app, 120, 40).contains(&format!("1 of {rows}")));

    press(&mut f.app, plain(KeyCode::End));
    assert!(screen(&mut f.app, 120, 40).contains(&format!("{rows} of {rows}")));
}

#[test]
fn the_status_line_switches_to_the_panels_own_keys() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    let rows = rendered(&mut f.app);
    let status = rows.last().unwrap();

    assert!(status.starts_with("esc close"), "status line: {status:?}");
    assert!(status.contains("^Q quit"));
    assert!(
        !status.contains("^N new"),
        "the normal-mode keys are still advertised"
    );
}

/// I8. The status line is the only place a refused quit is reported, and the panel is drawn
/// over the pane above it — so a message always outranks the hints.
#[test]
fn a_message_outranks_the_panels_hints() {
    let mut f = fixture(&[("a.md", "text")]);
    f.app
        .set_status("could not save — press ^Q again to quit anyway".to_owned());
    press(&mut f.app, ctrl('h'));

    let rows = rendered(&mut f.app);
    let status = rows.last().unwrap();

    assert!(status.contains("press ^Q again"), "status line: {status:?}");
    assert!(!status.contains("esc close"));
}

/// I5, structurally: the render order is the key-ownership order, so the modal that owns the
/// keyboard is the one on screen even if the panel was left open some other way.
#[test]
fn a_prompt_that_owns_the_keyboard_is_drawn_instead_of_the_panel() {
    let mut f = fixture(&[("a.md", "text")]);
    f.app.open_help();
    f.app.request_delete();

    let screen = screen(&mut f.app, 120, 40);

    assert!(screen.contains("Confirm"));
    assert!(
        !screen.contains("Keybindings"),
        "the panel covered the prompt"
    );
}

#[test]
fn an_external_change_prompt_is_drawn_instead_of_the_panel() {
    let mut f = raised_conflict();
    // Straight onto the state, because no keystroke can reach here: the conflict owns the
    // keyboard, and a conflict arriving closes the panel. This is the structural guarantee,
    // not the one that depends on either of those holding.
    f.app.open_help();

    let screen = screen(&mut f.app, 120, 40);

    assert!(screen.contains("External change"));
    assert!(
        !screen.contains("Keybindings"),
        "the panel covered the prompt"
    );
}

#[test]
fn a_wide_terminal_caps_the_panel_rather_than_stretching_it() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    let wide = panel_box(&draw(&mut f.app, 200, 60));

    assert_eq!(wide.width, 90, "the panel took 70% of a wide frame");
}

#[test]
fn a_frame_too_small_for_the_panel_draws_nothing_and_does_not_panic() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    // One row and one column short of the guard, in the body's own terms: the body is the
    // frame less the status line.
    let too_small = screen(&mut f.app, ui::HELP_MIN_BODY_W, ui::HELP_MIN_BODY_H);
    assert!(!too_small.contains("Keybindings"));

    let drawn = screen(&mut f.app, 40, 12);
    assert!(drawn.contains("Keybindings"));
}

/// Every size the panel can be asked for, including the ones where the window is taller than
/// the list — the case where an unbounded slice would panic.
#[test]
fn the_panel_renders_at_every_size_in_between() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));
    press(&mut f.app, plain(KeyCode::End));

    for (width, height) in [(20, 6), (21, 7), (40, 12), (80, 24), (120, 40), (200, 60)] {
        let _ = draw(&mut f.app, width, height);
    }
}

/// Scrolling addresses lines, not rows, so that a heading comes into view with the binding
/// underneath it. A cursor sitting on a section's first row with the heading scrolled off
/// would leave the user reading a key with no idea what it belongs to.
#[test]
fn a_sections_heading_comes_into_view_with_its_first_binding() {
    let mut f = fixture(&[("a.md", "text")]);
    let sections = keys::help_sections();
    let first_of_last = keys::help_row_count(&sections) - sections.last().unwrap().rows.len();

    press(&mut f.app, ctrl('h'));
    f.app.help_to(first_of_last);

    // Short enough that the list cannot all fit, so the view has to have scrolled.
    let screen = screen(&mut f.app, 80, 16);
    assert!(
        screen.contains("Editor"),
        "the heading scrolled off its rows"
    );
}

#[test]
fn the_cursor_bar_spans_the_full_inner_width() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    let buffer = draw(&mut f.app, 120, 40);
    let panel = panel_box(&buffer);
    let widest = (panel.top()..panel.bottom())
        .map(|y| {
            (panel.left()..panel.right())
                .filter(|x| buffer[(*x, y)].modifier.contains(Modifier::REVERSED))
                .count()
        })
        .max()
        .unwrap();

    assert_eq!(usize::from(panel.width) - 2, widest, "the bar stops short");
}

// --- filtering ----------------------------------------------------------------------------

/// Open the panel and type a query into its filter.
fn search(app: &mut App, query: &str) {
    press(app, ctrl('h'));
    press(app, plain(KeyCode::Char('/')));
    for c in query.chars() {
        press(app, plain(KeyCode::Char(c)));
    }
}

/// The rows the panel is listing, as `caption` values.
fn listed(app: &App) -> Vec<&'static str> {
    app.help()
        .unwrap()
        .listed()
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| row.caption)
        .collect()
}

#[test]
fn slash_opens_the_filter_prompt() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "");

    assert!(f.app.help().unwrap().searching());
    let rows = rendered(&mut f.app);
    let status = rows.last().unwrap();

    assert!(status.starts_with("Filter:"), "status line: {status:?}");
    assert!(!status.contains("esc close"), "the hints are still there");
}

#[test]
fn a_query_narrows_the_list_and_the_readout_counts_what_is_left() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "delete");

    assert_eq!(listed(&f.app), vec!["^D", "y/⏎", "any other key"]);

    let screen = screen(&mut f.app, 120, 40);
    assert!(screen.contains("1 of 3"), "the readout did not follow");
    assert!(
        !screen.contains("new note"),
        "an unmatched row is still listed"
    );
}

/// Substring matching, not word matching: `del` finds the delete rows and also `modeless`,
/// which is the honest consequence of the simplest rule that works on twenty rows.
#[test]
fn matching_is_a_plain_substring() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "del");

    assert!(listed(&f.app).contains(&"^D"));
    assert_eq!(listed(&f.app).len(), 4, "{:?}", listed(&f.app));
}

#[test]
fn matching_ignores_case_in_both_directions() {
    let mut lower = fixture(&[("a.md", "text")]);
    let mut upper = fixture(&[("a.md", "text")]);
    search(&mut lower.app, "delete");
    search(&mut upper.app, "DELETE");

    assert_eq!(listed(&lower.app), listed(&upper.app));
}

/// The caption is searchable too, which is what makes a half-remembered key findable.
#[test]
fn a_query_matches_captions_as_well_as_descriptions() {
    let mut reorder = fixture(&[("a.md", "text")]);
    search(&mut reorder.app, "alt");
    assert_eq!(listed(&reorder.app), vec!["alt-↑", "alt-↓"]);

    let mut help = fixture(&[("a.md", "text")]);
    search(&mut help.app, "f1");
    assert_eq!(listed(&help.app), vec!["^H/F1"]);
}

/// Inside the prompt every printable key is text — including the ones that move the cursor
/// outside it, and including `/` itself.
#[test]
fn every_printable_key_is_text_inside_the_prompt() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "");

    for c in ['k', 'j', 'q', '/', 'Z'] {
        press(&mut f.app, plain(KeyCode::Char(c)));
    }

    assert_eq!(f.app.help().unwrap().query(), "kjq/Z");
    assert!(f.app.help().unwrap().searching(), "`q` left the prompt");
    assert!(f.app.help().is_some(), "`q` quit from inside the prompt");
}

/// A key that would move the cursor outside the prompt does not move it inside one.
#[test]
fn the_cursor_holds_still_while_the_query_is_being_typed() {
    let mut f = fixture(&[("a.md", "text")]);
    // A query broad enough that the next keystroke below does not shorten the list under the
    // cursor, so a moved cursor can only have been moved by the keystroke itself.
    search(&mut f.app, "e");
    f.app.help_to(2);

    press(&mut f.app, plain(KeyCode::Char('l')));

    assert_eq!(f.app.help().unwrap().query(), "el");
    assert_eq!(f.app.help().unwrap().cursor(), 2, "`l` moved the cursor");
}

#[test]
fn a_modified_key_is_not_typed_into_the_query() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "de");

    press(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT),
    );

    assert_eq!(f.app.help().unwrap().query(), "de");
}

#[test]
fn backspace_on_an_empty_query_does_nothing() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "d");

    press(&mut f.app, plain(KeyCode::Backspace));
    press(&mut f.app, plain(KeyCode::Backspace));

    assert_eq!(f.app.help().unwrap().query(), "");
    assert!(f.app.help().is_some());
}

/// One `esc` gives the whole list back, the next closes the panel. A single key that did both
/// would make an accidental filter cost the user their place.
#[test]
fn esc_clears_the_query_before_it_closes_the_panel() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "del");

    press(&mut f.app, plain(KeyCode::Esc));
    assert!(f.app.help().is_some(), "the panel closed on the first esc");
    assert_eq!(f.app.help().unwrap().query(), "");
    assert!(!f.app.help().unwrap().searching());

    press(&mut f.app, plain(KeyCode::Esc));
    assert!(f.app.help().is_none());
}

#[test]
fn enter_leaves_the_prompt_with_the_query_intact() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "delete");

    press(&mut f.app, plain(KeyCode::Enter));

    assert!(!f.app.help().unwrap().searching());
    assert_eq!(f.app.help().unwrap().query(), "delete");
    assert_eq!(listed(&f.app).len(), 3, "the filter came off");
}

/// The box is measured from the whole list, so it holds still while the user types. A box
/// that resized on every keystroke would be unreadable — and the bounded slice that makes a
/// short body safe only gets exercised because of this.
#[test]
fn the_box_does_not_move_while_the_list_is_being_filtered() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));
    let unfiltered = panel_box(&draw(&mut f.app, 120, 40));

    press(&mut f.app, plain(KeyCode::Char('/')));
    for c in "zzz".chars() {
        press(&mut f.app, plain(KeyCode::Char(c)));
    }
    let filtered = panel_box(&draw(&mut f.app, 120, 40));

    assert_eq!(unfiltered, filtered);
}

#[test]
fn a_query_that_matches_nothing_says_so_instead_of_panicking() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "zzz");

    for (width, height) in [(21, 7), (40, 12), (120, 40), (200, 60)] {
        let screen = screen(&mut f.app, width, height);
        if !screen.contains("Keybindings") {
            continue;
        }
        assert!(
            screen.contains("no match for \"zzz\""),
            "no explanation at {width}x{height}"
        );
        assert!(screen.contains("0 of 0"), "the readout at {width}x{height}");
    }
}

#[test]
fn a_query_pulls_the_cursor_back_inside_what_is_left() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));
    press(&mut f.app, plain(KeyCode::End));
    f.app.help_scroll(100);

    press(&mut f.app, plain(KeyCode::Char('/')));
    for c in "delete".chars() {
        press(&mut f.app, plain(KeyCode::Char(c)));
    }

    let listed = listed(&f.app).len();
    let help = f.app.help().unwrap();
    assert!(
        help.cursor() < listed,
        "cursor {} in a {listed}-row list",
        help.cursor()
    );
    assert_eq!(help.offset(), 0);
}

#[test]
fn the_query_stops_growing_and_the_prompt_stays_on_its_line() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "");
    for _ in 0..200 {
        press(&mut f.app, plain(KeyCode::Char('x')));
    }

    assert_eq!(
        f.app.help().unwrap().query().chars().count(),
        scratchbox_tui::app::HELP_QUERY_MAX
    );

    let rows = rendered(&mut f.app);
    assert_eq!(rows.last().unwrap().chars().count(), 120);
}

/// The query is the only text on this panel that did not come from the source. Nothing here
/// should be able to put a control sequence on the terminal, and that is checked at the sink
/// rather than assumed of the formatter.
#[test]
fn nothing_the_user_types_reaches_the_terminal_as_a_control_sequence() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "");
    for c in "\u{1b}[31m\u{7}😀".chars() {
        f.app.help_type(c);
    }

    let buffer = draw(&mut f.app, 120, 40);

    for cell in buffer.content() {
        assert!(
            !cell.symbol().chars().any(char::is_control),
            "a control character reached the buffer: {:?}",
            cell.symbol()
        );
    }
}

#[test]
fn quitting_from_inside_the_prompt_closes_the_panel_first() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "del");

    press(&mut f.app, ctrl('q'));

    assert!(f.app.help().is_none());
    assert!(f.app.should_quit());
}

// --- running a binding from the panel -----------------------------------------------------

/// Put the cursor on the row with this caption and press `⏎`.
fn run_row(app: &mut App, caption: &str) {
    let at = listed(app)
        .iter()
        .position(|listed| *listed == caption)
        .unwrap_or_else(|| panic!("{caption} is not listed"));
    app.help_to(at);
    press(app, plain(KeyCode::Enter));
}

#[test]
fn enter_on_a_runnable_row_closes_the_panel_and_does_the_thing() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    run_row(&mut f.app, "^N");

    assert!(f.app.help().is_none(), "the panel stayed open");
    assert_eq!(f.app.notes().len(), 2, "no note was created");
}

#[test]
fn enter_on_the_pane_row_switches_pane() {
    let mut f = fixture(&[("a.md", "text")]);
    let before = f.app.focus();
    press(&mut f.app, ctrl('h'));

    run_row(&mut f.app, "tab");

    assert!(f.app.help().is_none());
    assert_ne!(f.app.focus(), before);
}

/// A list-only binding run from the panel ignores the focus condition on its chord. The chord
/// is what belongs to the list pane; the command underneath it was never focus-gated.
#[test]
fn enter_on_a_list_only_row_works_from_the_editor() {
    let mut f = fixture(&[("a.md", "first"), ("b.md", "second")]);
    f.app.select_next().unwrap();
    assert_eq!(f.app.focus(), Focus::Editor);
    assert_eq!(f.app.selected().unwrap().as_str(), "b.md");

    press(&mut f.app, ctrl('h'));
    run_row(&mut f.app, "↑");

    assert_eq!(f.app.selected().unwrap().as_str(), "a.md");
}

/// The delete row opens the confirmation, exactly as pressing `^D` does — the panel offers no
/// route to the confirmation itself, because that row is one of the ones it will not run.
#[test]
fn enter_on_the_delete_row_asks_rather_than_deletes() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    run_row(&mut f.app, "^D");

    assert!(f.app.help().is_none());
    assert!(f.app.pending_delete().is_some());
    assert!(f.workspace.join("a.md").exists(), "the note was deleted");
}

#[test]
fn enter_on_a_row_that_cannot_be_run_does_nothing_at_all() {
    for caption in ["y/⏎", "k", "^H/F1", "^Q/^C"] {
        let mut f = fixture(&[("a.md", "text")]);
        press(&mut f.app, ctrl('h'));

        run_row(&mut f.app, caption);

        assert!(f.app.help().is_some(), "{caption} closed the panel");
        assert!(!f.app.should_quit(), "{caption} quit the app");
        assert_eq!(f.app.notes().len(), 1);
        assert!(f.app.pending_delete().is_none());
    }
}

/// `^Q` as a keypress still quits. It is `⏎` landing on the quit row that must not, because
/// the cursor gets there by browsing rather than by aiming.
#[test]
fn the_quit_row_is_not_a_way_to_quit_but_the_key_still_is() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));

    run_row(&mut f.app, "^Q/^C");
    assert!(!f.app.should_quit());

    press(&mut f.app, ctrl('q'));
    assert!(f.app.should_quit());
}

#[test]
fn enter_while_searching_commits_the_filter_and_runs_nothing() {
    let mut f = fixture(&[("a.md", "text")]);
    search(&mut f.app, "new");

    press(&mut f.app, plain(KeyCode::Enter));

    assert!(f.app.help().is_some(), "the panel ran a row and closed");
    assert_eq!(f.app.notes().len(), 1);
    assert_eq!(f.app.help().unwrap().query(), "new");
}

/// The cursor addresses what is listed, so under a filter row zero is a different binding —
/// and the one that runs has to be the one highlighted.
#[test]
fn enter_under_a_filter_runs_the_row_the_cursor_is_on() {
    let mut f = fixture(&[("a.md", "text")]);
    let before = f.app.focus();
    search(&mut f.app, "tab");
    press(&mut f.app, plain(KeyCode::Enter));

    assert_eq!(
        listed(&f.app),
        vec!["tab"],
        "the query matched something else"
    );
    assert_eq!(f.app.help().unwrap().cursor(), 0);
    press(&mut f.app, plain(KeyCode::Enter));

    assert_ne!(f.app.focus(), before, "row zero ran the unfiltered binding");
    assert_eq!(f.app.notes().len(), 1, "it created a note instead");
}

/// A row `⏎` will not run is dimmed, so that doing nothing reads as an answer.
#[test]
fn the_rows_that_cannot_be_run_are_drawn_dimmed() {
    let mut f = fixture(&[("a.md", "text")]);
    press(&mut f.app, ctrl('h'));
    // Off every row under test: the cursor's own row is reversed rather than coloured.
    press(&mut f.app, plain(KeyCode::End));

    let buffer = draw(&mut f.app, 120, 40);
    let panel = panel_box(&buffer);
    let rows = text_rows(&buffer);

    for (caption, dimmed) in [("^Q/^C", true), ("^H/F1", true), ("k", true), ("^N", false)] {
        let y = rows
            .iter()
            .position(|row| row.contains(&format!("{caption}  ")))
            .unwrap_or_else(|| panic!("{caption} is not on screen")) as u16;
        // Inside the panel's own columns: the pane borders around it are dim too.
        let x = (panel.left() + 1..panel.right() - 1)
            .find(|x| buffer[(*x, y)].symbol() != " ")
            .unwrap();

        assert_eq!(
            buffer[(x, y)].fg == ratatui::style::Color::DarkGray,
            dimmed,
            "{caption} is painted {:?}",
            buffer[(x, y)].fg
        );
    }
}

/// An app with an unresolved external change on the open note.
fn raised_conflict() -> Fixture {
    let mut f = fixture(&[("a.md", "original")]);
    type_text(&mut f.app, "mine ");
    fs::write(f.workspace.join("a.md"), "theirs").unwrap();
    f.app
        .apply_store_event(&StoreEvent::Modified(id("a.md")))
        .unwrap();
    assert!(f.app.conflict().is_some());
    f
}

