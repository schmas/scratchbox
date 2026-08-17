//! Note model and naming tests. Pure logic — no temp dirs needed.
//!
//! The input/expected tables are `#[case]` rows; the tests whose *names* carry a decision are
//! left as they were. Folding those into anonymous rows would lose the documentation, which is
//! the one thing a table cannot express. Every case is labelled: an unlabelled `#[case]` reports
//! as `case_1`, which tells a reader nothing at 3am.

use rstest::rstest;
use scratchbox_core::jiff::Zoned;
use scratchbox_core::naming::{
    MAX_SLUG_LEN, is_slugged, new_note_name, slug_from_first_line, slugged_name,
};
use scratchbox_core::{Format, InvalidNoteId, NoteId};

fn id(name: &str) -> NoteId {
    NoteId::new(name).expect("test name should be valid")
}

#[rstest]
#[case::markdown("md", Format::Markdown)]
#[case::json("json", Format::Json)]
#[case::plain_text("txt", Format::PlainText)]
#[case::java("java", Format::Java)]
#[case::typescript("ts", Format::TypeScript)]
#[case::javascript("js", Format::JavaScript)]
#[case::css("css", Format::Css)]
#[case::html("html", Format::Html)]
fn every_declared_format_maps_from_its_extension(#[case] ext: &str, #[case] expected: Format) {
    assert_eq!(Format::from_extension(ext), expected, "extension {ext}");
    // Round-trip: the format's own default extension maps back to itself.
    assert_eq!(Format::from_extension(expected.extension()), expected);
}

#[rstest]
#[case::all_caps("MD", Format::Markdown)]
#[case::mixed_case("Json", Format::Json)]
#[case::all_caps_html("HTML", Format::Html)]
fn extension_matching_ignores_case(#[case] ext: &str, #[case] expected: Format) {
    assert_eq!(Format::from_extension(ext), expected);
    // The whole-name path folds case too, because it routes through `from_extension`.
    assert_eq!(
        Format::from_name(&format!("2026-08-15-1548.{ext}")),
        expected
    );
}

/// Split from the name-shaped cases below because it exercises a different function. Folding
/// the two into one table would need a row to say which one it meant, which is worse than two
/// tables that each say it once.
#[rstest]
#[case::unrecognized("rs")]
#[case::empty("")]
fn an_unknown_extension_falls_back_to_plain(#[case] ext: &str) {
    assert_eq!(Format::from_extension(ext), Format::PlainText);
}

#[rstest]
#[case::no_extension_at_all("notes")]
#[case::unrecognized_extension("2026-08-15-1548.xyz")]
fn an_unknown_or_missing_extension_on_a_name_falls_back_to_plain(#[case] name: &str) {
    assert_eq!(Format::from_name(name), Format::PlainText);
}

#[test]
fn a_new_note_is_named_for_the_minute_it_was_created() {
    let now: Zoned = "2026-08-15T15:48:30-03[America/Sao_Paulo]".parse().unwrap();

    assert_eq!(new_note_name(&now, "md"), "2026-08-15-1548.md");
    assert_eq!(new_note_name(&now, "ts"), "2026-08-15-1548.ts");
}

#[rstest]
#[case::accents_folded("Café über", "cafe-uber")]
#[case::punctuation_and_spaces("Meeting Notes: Q3 Planning", "meeting-notes-q3-planning")]
fn slug_is_kebab_case_and_ascii_folded(#[case] line: &str, #[case] expected: &str) {
    assert_eq!(slug_from_first_line(line).as_deref(), Some(expected));
}

#[rstest]
#[case::one_hash("# Foo", "foo")]
#[case::three_hashes("### Foo Bar", "foo-bar")]
#[case::leading_space_and_bangs("   !!! hello !!!", "hello")]
fn leading_markdown_hashes_and_punctuation_are_stripped(
    #[case] line: &str,
    #[case] expected: &str,
) {
    assert_eq!(slug_from_first_line(line).as_deref(), Some(expected));
}

#[rstest]
#[case::empty("")]
#[case::newline_only("\n")]
#[case::whitespace_only("   \t  ")]
// `slugify` transliterates rather than drops, so this would otherwise become
// "tada-tada-tada" — a name derived from Unicode character names, not from the user's line.
#[case::emoji_only("🎉🎉🎉")]
#[case::punctuation_only("!!!")]
#[case::hashes_only("###")]
fn a_line_with_no_usable_characters_stays_unslugged(#[case] line: &str) {
    assert_eq!(slug_from_first_line(line), None);
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

/// Kept even though `naming_properties` now covers the same ground over every input. It runs in
/// milliseconds, it names the concern in the test list, and deleting a cheap named regression
/// test because a generator overlaps it is a bad trade.
#[test]
fn slugging_never_panics_on_hostile_input() {
    let huge = "x".repeat(10 * 1024);
    assert!(slug_from_first_line(&huge).is_some());
    assert!(slug_from_first_line(&"🎉".repeat(4096)).is_none());
    assert!(slug_from_first_line("\n\n\n").is_none());
    assert!(slug_from_first_line("\0").is_none());
}

#[rstest]
#[case::bare_timestamp_md("2026-08-15-1548.md", false)]
#[case::bare_timestamp_ts("2026-08-15-1548.ts", false)]
#[case::timestamp_with_slug("2026-08-15-1548-foo.md", true)]
// A file the user made by hand counts as slugged, so it is never renamed.
#[case::hand_made_file("shopping-list.txt", true)]
// A date is not a timestamp: the stem is 10 characters, not 15.
#[case::date_without_a_time("2026-08-15.md", true)]
fn slug_state_is_read_off_the_name_shape(#[case] name: &str, #[case] expected: bool) {
    assert_eq!(is_slugged(&id(name)), expected);
}

#[rstest]
#[case::markdown(
    "2026-08-15-1548.md",
    "meeting-notes",
    "2026-08-15-1548-meeting-notes.md"
)]
#[case::typescript("2026-08-15-1548.ts", "parser", "2026-08-15-1548-parser.ts")]
#[case::no_extension("2026-08-15-1548", "no-extension", "2026-08-15-1548-no-extension")]
fn slugged_name_keeps_the_timestamp_prefix_and_extension(
    #[case] original: &str,
    #[case] slug: &str,
    #[case] expected: &str,
) {
    assert_eq!(slugged_name(&id(original), slug), expected);
}

#[rstest]
#[case::parent_traversal("../x")]
#[case::deep_traversal("../../.ssh/id_rsa")]
#[case::absolute_path("/etc/passwd")]
#[case::forward_separator("a/b")]
#[case::back_separator("a\\b")]
#[case::parent_dir("..")]
#[case::current_dir(".")]
#[case::empty("")]
#[case::hidden(".hidden")]
#[case::app_dir(".scratchbox")]
#[case::nul_byte("note\0.md")]
#[case::dot_slash_prefix("./note.md")]
#[case::trailing_separator("sub/")]
fn note_id_rejects_anything_that_could_escape_the_workspace(#[case] name: &str) {
    assert!(
        NoteId::new(name).is_err(),
        "{name:?} should not be a valid note name"
    );
}

/// Asserted against the exact variant rather than with `matches!`, so a rejection reported for
/// the wrong reason is a failure. The variants are what the TUI shows the user.
#[rstest]
#[case::empty("", InvalidNoteId::Empty)]
#[case::separator("a/b", InvalidNoteId::Separator("a/b".to_owned()))]
#[case::hidden(".hidden", InvalidNoteId::Hidden(".hidden".to_owned()))]
#[case::nul_byte("note\0.md", InvalidNoteId::NulByte("note\0.md".to_owned()))]
#[case::parent_dir("..", InvalidNoteId::NotPlainName("..".to_owned()))]
fn note_id_rejections_say_why(#[case] name: &str, #[case] expected: InvalidNoteId) {
    assert_eq!(NoteId::new(name), Err(expected));
}

#[rstest]
#[case::bare_timestamp("2026-08-15-1548.md")]
#[case::timestamp_with_slug("2026-08-15-1548-my-note.ts")]
#[case::spaces_in_the_name("shopping list.txt")]
#[case::no_extension("note")]
fn note_id_accepts_ordinary_note_names(#[case] name: &str) {
    assert_eq!(
        NoteId::new(name).expect("should be valid").as_str(),
        name,
        "{name:?} should be a valid note name"
    );
}
