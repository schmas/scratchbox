//! Note model and naming tests. Pure logic — no temp dirs needed.

use scratchbox_core::jiff::Zoned;
use scratchbox_core::naming::{
    MAX_SLUG_LEN, is_slugged, new_note_name, slug_from_first_line, slugged_name,
};
use scratchbox_core::{Format, InvalidNoteId, NoteId};

fn id(name: &str) -> NoteId {
    NoteId::new(name).expect("test name should be valid")
}

#[test]
fn every_declared_format_maps_from_its_extension() {
    let cases = [
        ("md", Format::Markdown),
        ("json", Format::Json),
        ("txt", Format::PlainText),
        ("java", Format::Java),
        ("ts", Format::TypeScript),
        ("js", Format::JavaScript),
        ("css", Format::Css),
        ("html", Format::Html),
    ];
    for (ext, expected) in cases {
        assert_eq!(Format::from_extension(ext), expected, "extension {ext}");
        // Round-trip: the format's own default extension maps back to itself.
        assert_eq!(Format::from_extension(expected.extension()), expected);
    }
}

#[test]
fn extension_matching_ignores_case() {
    assert_eq!(Format::from_extension("MD"), Format::Markdown);
    assert_eq!(Format::from_extension("Json"), Format::Json);
    assert_eq!(Format::from_name("2026-08-15-1548.HTML"), Format::Html);
}

#[test]
fn unknown_and_missing_extensions_fall_back_to_plain() {
    assert_eq!(Format::from_extension("rs"), Format::PlainText);
    assert_eq!(Format::from_extension(""), Format::PlainText);
    assert_eq!(Format::from_name("notes"), Format::PlainText);
    assert_eq!(Format::from_name("2026-08-15-1548.xyz"), Format::PlainText);
}

#[test]
fn a_new_note_is_named_for_the_minute_it_was_created() {
    let now: Zoned = "2026-08-15T15:48:30-03[America/Sao_Paulo]".parse().unwrap();

    assert_eq!(new_note_name(&now, "md"), "2026-08-15-1548.md");
    assert_eq!(new_note_name(&now, "ts"), "2026-08-15-1548.ts");
}

#[test]
fn slug_is_kebab_case_and_ascii_folded() {
    assert_eq!(
        slug_from_first_line("Café über").as_deref(),
        Some("cafe-uber")
    );
    assert_eq!(
        slug_from_first_line("Meeting Notes: Q3 Planning").as_deref(),
        Some("meeting-notes-q3-planning")
    );
}

#[test]
fn leading_markdown_hashes_and_punctuation_are_stripped() {
    assert_eq!(slug_from_first_line("# Foo").as_deref(), Some("foo"));
    assert_eq!(
        slug_from_first_line("### Foo Bar").as_deref(),
        Some("foo-bar")
    );
    assert_eq!(
        slug_from_first_line("   !!! hello !!!").as_deref(),
        Some("hello")
    );
}

#[test]
fn a_line_with_no_usable_characters_stays_unslugged() {
    assert_eq!(slug_from_first_line(""), None);
    assert_eq!(slug_from_first_line("\n"), None);
    assert_eq!(slug_from_first_line("   \t  "), None);
    assert_eq!(slug_from_first_line("🎉🎉🎉"), None);
    assert_eq!(slug_from_first_line("!!!"), None);
    assert_eq!(slug_from_first_line("###"), None);
}

#[test]
fn only_the_first_line_feeds_the_slug() {
    assert_eq!(
        slug_from_first_line("title here\nbody that should not matter").as_deref(),
        Some("title-here")
    );
}

#[test]
fn a_long_line_is_cut_on_a_word_boundary() {
    let long = "the quick brown fox jumps over the lazy dog and keeps running for a while";
    let slug = slug_from_first_line(long).expect("long line yields a slug");

    assert!(slug.len() <= MAX_SLUG_LEN, "slug too long: {slug:?}");
    assert!(!slug.ends_with('-'), "slug ends mid-token: {slug:?}");
    // Cut on a boundary, so the last word survives whole rather than being clipped.
    assert!(
        long.replace(' ', "-").starts_with(&slug),
        "unexpected slug {slug:?}"
    );
}

#[test]
fn a_single_overlong_word_is_hard_truncated() {
    let word = "a".repeat(60);
    let slug = slug_from_first_line(&word).expect("a long word still yields a slug");

    assert_eq!(slug.len(), MAX_SLUG_LEN);
    assert_eq!(slug, "a".repeat(MAX_SLUG_LEN));
}

#[test]
fn slugging_never_panics_on_hostile_input() {
    let huge = "x".repeat(10 * 1024);
    assert!(slug_from_first_line(&huge).is_some());
    assert!(slug_from_first_line(&"🎉".repeat(4096)).is_none());
    assert!(slug_from_first_line("\n\n\n").is_none());
    assert!(slug_from_first_line("\0").is_none());
}

#[test]
fn slug_state_is_read_off_the_name_shape() {
    assert!(!is_slugged(&id("2026-08-15-1548.md")));
    assert!(!is_slugged(&id("2026-08-15-1548.ts")));

    assert!(is_slugged(&id("2026-08-15-1548-foo.md")));
    // A file the user made by hand counts as slugged, so it is never renamed.
    assert!(is_slugged(&id("shopping-list.txt")));
    assert!(is_slugged(&id("2026-08-15.md")));
}

#[test]
fn slugged_name_keeps_the_timestamp_prefix_and_extension() {
    assert_eq!(
        slugged_name(&id("2026-08-15-1548.md"), "meeting-notes"),
        "2026-08-15-1548-meeting-notes.md"
    );
    assert_eq!(
        slugged_name(&id("2026-08-15-1548.ts"), "parser"),
        "2026-08-15-1548-parser.ts"
    );
    assert_eq!(
        slugged_name(&id("2026-08-15-1548"), "no-extension"),
        "2026-08-15-1548-no-extension"
    );
}

#[test]
fn note_id_rejects_anything_that_could_escape_the_workspace() {
    let rejected = [
        "../x",
        "../../.ssh/id_rsa",
        "/etc/passwd",
        "a/b",
        "a\\b",
        "..",
        ".",
        "",
        ".hidden",
        ".scratchbox",
        "note\0.md",
        "./note.md",
        "sub/",
    ];
    for name in rejected {
        assert!(
            NoteId::new(name).is_err(),
            "{name:?} should not be a valid note name"
        );
    }
}

#[test]
fn note_id_rejections_say_why() {
    assert_eq!(NoteId::new(""), Err(InvalidNoteId::Empty));
    assert!(matches!(
        NoteId::new("a/b"),
        Err(InvalidNoteId::Separator(_))
    ));
    assert!(matches!(
        NoteId::new(".hidden"),
        Err(InvalidNoteId::Hidden(_))
    ));
    assert!(matches!(
        NoteId::new("note\0.md"),
        Err(InvalidNoteId::NulByte(_))
    ));
    assert!(matches!(
        NoteId::new(".."),
        Err(InvalidNoteId::NotPlainName(_))
    ));
}

#[test]
fn note_id_accepts_ordinary_note_names() {
    for name in [
        "2026-08-15-1548.md",
        "2026-08-15-1548-my-note.ts",
        "shopping list.txt",
        "note",
    ] {
        assert_eq!(
            NoteId::new(name).expect("should be valid").as_str(),
            name,
            "{name:?} should be a valid note name"
        );
    }
}
