//! Format → syntax resolution, and the guarantees that go with it.

use std::collections::HashSet;
use std::fs;

use edtui::THEME_SET;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use scratchbox_core::order::OrderStore;
use scratchbox_core::{APP_SUBDIR, FolderSync, Format, NoteId, naming};
use scratchbox_tui::app::App;
use scratchbox_tui::syntax::{self, THEME_NAME};
use scratchbox_tui::ui;
use tempfile::TempDir;

/// Every format the app declares, and the syntax it must resolve to.
///
/// Named explicitly rather than looped over, so adding a format to `Format` without
/// deciding what highlights it fails here instead of silently degrading to plain text.
const EXPECTED: [(Format, &str); 8] = [
    (Format::Markdown, "Markdown"),
    (Format::Json, "JSON"),
    (Format::PlainText, "Plain Text"),
    (Format::Java, "Java"),
    // bat's set carries the Babel grammar, which is a superset of plain JavaScript.
    (Format::JavaScript, "JavaScript (Babel)"),
    (Format::TypeScript, "TypeScript"),
    (Format::Css, "CSS"),
    (Format::Html, "HTML"),
];

#[test]
fn every_declared_format_resolves_to_a_syntax() {
    for (format, expected) in EXPECTED {
        assert_eq!(
            syntax::syntax_name(format),
            expected,
            "{format:?} did not resolve to its syntax"
        );
    }
}

/// The reason `two-face` is a dependency at all: syntect's bundled set has no TypeScript,
/// so a plain-text TS note would be the signal that the extra set stopped being loaded.
#[test]
fn typescript_resolves_which_syntects_own_defaults_cannot_do() {
    assert_eq!(syntax::syntax_name(Format::TypeScript), "TypeScript");
    assert!(
        edtui::SYNTAX_SET.find_syntax_by_extension("ts").is_none(),
        "syntect's defaults gained TypeScript; the two-face dependency may no longer be needed"
    );
}

/// An unknown extension is plain text well before it reaches syntect — `Format` maps
/// anything it does not recognise to `PlainText`. Both halves are checked, because the
/// fallback inside `highlighter` only ever fires if this one stops holding.
#[test]
fn unknown_extensions_fall_back_to_plain_text() {
    for name in ["notes.xyz", "notes.", "notes", "a.b.c.unknown"] {
        assert_eq!(
            Format::from_name(name),
            Format::PlainText,
            "{name} was not treated as plain text"
        );
    }
    assert_eq!(syntax::syntax_name(Format::PlainText), "Plain Text");
}

/// Building a highlighter must not panic for any format, including the fallback path.
#[test]
fn a_highlighter_can_be_built_for_every_format() {
    for (format, _) in EXPECTED {
        let _ = syntax::highlighter(format);
    }
}

/// The theme name is a hard-coded string with no config key behind it (RT-13), so a typo
/// would silently produce an unstyled editor rather than an error.
#[test]
fn the_theme_name_resolves() {
    assert!(
        THEME_SET.themes.contains_key(THEME_NAME),
        "{THEME_NAME} is not in the bundled theme set"
    );
}

// --- what actually reaches the screen --------------------------------------------------

/// Every declared format, with a sample that exercises more than one scope on its opening
/// lines. Only the first screenful is highlighted, so the sample has to be short.
const SAMPLES: [(Format, &str, &str); 8] = [
    (
        Format::Markdown,
        "md",
        "# Heading\n\n- **bold** item with `code`\n",
    ),
    (
        Format::Json,
        "json",
        "{\n  \"key\": \"value\",\n  \"n\": 42\n}\n",
    ),
    (
        Format::Java,
        "java",
        "public class A {\n  private int x = 1; // a comment\n}\n",
    ),
    (
        Format::TypeScript,
        "ts",
        "const value: number = 1; // a comment\n",
    ),
    (Format::JavaScript, "js", "const value = 1; // a comment\n"),
    (Format::Css, "css", "body {\n  color: #ffffff;\n}\n"),
    (Format::Html, "html", "<p class=\"lead\">text</p>\n"),
    (Format::PlainText, "txt", "just words, nothing to colour\n"),
];

/// Distinct foreground colours in a rendered frame for a note of this name.
///
/// The whole frame, chrome included — which is the point. Every case renders the identical
/// list, borders, and status line, so comparing two renders of the same body isolates what
/// the highlighter contributed.
fn painted_colors(note: &str, body: &str) -> HashSet<Color> {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("notes");
    let store = FolderSync::new(workspace.clone(), tmp.path().join("trash")).unwrap();
    fs::write(workspace.join(note), body).unwrap();

    let mut app = App::new(
        Box::new(store),
        OrderStore::new(&workspace.join(APP_SUBDIR)),
    )
    .unwrap();

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.fg)
        .collect()
}

/// The success criterion, checked rather than eyeballed: the same bytes rendered under a
/// highlighted extension put more colours on screen than under an unknown one.
#[test]
fn every_highlighted_format_paints_more_colours_than_plain_text() {
    for (format, extension, body) in SAMPLES {
        if format == Format::PlainText {
            continue;
        }
        let highlighted = painted_colors(&format!("sample.{extension}"), body);
        let plain = painted_colors("sample.xyz", body);

        assert!(
            highlighted.len() > plain.len(),
            "{format:?} painted {} colours, no more than the {} plain text managed \
             — it is not highlighting",
            highlighted.len(),
            plain.len()
        );
    }
}

/// The other half: plain text is genuinely plain, and an unknown extension renders without
/// erroring or panicking rather than refusing the note.
#[test]
fn plain_text_and_unknown_extensions_paint_the_same_thing() {
    let (_, _, body) = SAMPLES[7];

    let txt = painted_colors("sample.txt", body);
    let unknown = painted_colors("sample.xyz", body);
    let extensionless = painted_colors("sample", body);

    assert_eq!(txt, unknown);
    assert_eq!(txt, extensionless);
}

/// D10 renames a note by appending a slug and leaves the extension alone, so the syntax it
/// highlights with cannot change underneath it.
#[test]
fn the_rename_on_first_content_does_not_change_the_syntax() {
    for (born_as, slug) in [
        ("2026-08-15-1548.md", "my-note"),
        ("2026-08-15-1548.ts", "some-module"),
        ("2026-08-15-1548.json", "config"),
    ] {
        let id = NoteId::new(born_as).unwrap();
        let renamed = naming::slugged_name(&id, slug);

        assert_eq!(
            Format::from_name(born_as),
            Format::from_name(&renamed),
            "{born_as} changed format when it became {renamed}"
        );
    }
}
