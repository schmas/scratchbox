//! `naming`'s invariants, stated over all inputs rather than over the ones somebody thought
//! of.
//!
//! Its own file rather than rows added to `tests/naming.rs`: a property carries a generator
//! preamble and a different failure signature from a table row, and the named tests there
//! document decisions that a generator cannot. Both are worth having.
//!
//! Pure and allocation-only — no filesystem, no watcher, so nothing here can be flaky.

use proptest::prelude::*;
use scratchbox_core::NoteId;
use scratchbox_core::naming::{
    MAX_SLUG_LEN, is_slugged, slug_from_first_line, slugged_name, timestamp_prefix, with_suffix,
};

/// Width of the `YYYY-MM-DD-HHMM` prefix `naming` recognises.
const TIMESTAMP_LEN: usize = 15;

/// A name [`NoteId::new`] accepts.
///
/// Not `any::<String>()`: that would spend nearly every case being rejected at the door, and
/// rejection is what `order_properties`' hostile-manifest property is for.
///
/// Trailing whitespace is deliberately reachable — `NoteId::new("a ")` is `Ok` — because
/// narrowing a generator to hide the defect at issue #19 is what the review of this plan
/// explicitly forbade.
fn note_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 _-]{0,30}(\\.[a-z]{1,5})?"
}

/// `YYYY-MM-DD-HHMM`: the shape `naming` reads as "not slugged yet".
///
/// Any digits at all. This is a name shape rather than a date, and `naming` never parses it
/// as one — `is_timestamp` checks digits with dashes at 4, 7, and 10 and nothing more.
fn timestamp() -> impl Strategy<Value = String> {
    "[0-9]{4}-[0-9]{2}-[0-9]{2}-[0-9]{4}"
}

fn extension() -> impl Strategy<Value = String> {
    "[a-z]{1,5}"
}

/// A name whose stem is exactly a timestamp — a note that has not been renamed yet.
fn unslugged_name() -> impl Strategy<Value = String> {
    (timestamp(), proptest::option::of(extension())).prop_map(|(stem, ext)| match ext {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem,
    })
}

/// What `slug_from_first_line` actually produces: lowercase ASCII kebab, non-empty, no
/// leading or trailing dash.
fn slug() -> impl Strategy<Value = String> {
    "[a-z0-9]+(-[a-z0-9]+){0,6}"
}

fn id(name: &str) -> NoteId {
    NoteId::new(name).expect("the generator should only produce valid note names")
}

proptest! {
    /// The bound `a_long_line_is_cut_on_a_word_boundary` asserts for one input, over every
    /// input. `\PC` is the complement of Unicode category Other, so this is the printable
    /// space; the control-character half of the domain is covered by `never_panics` below.
    #[test]
    fn a_slug_is_ascii_bounded_and_never_edged_with_a_dash(text in "\\PC{0,4096}") {
        if let Some(slug) = slug_from_first_line(&text) {
            prop_assert!(slug.is_ascii(), "slug is not ASCII: {:?}", slug);
            prop_assert!(
                slug.len() <= MAX_SLUG_LEN,
                "slug is {} bytes, over the {} cap: {:?}",
                slug.len(),
                MAX_SLUG_LEN,
                slug
            );
            prop_assert!(!slug.is_empty(), "an empty slug should have been `None`");
            prop_assert!(
                !slug.starts_with('-') && !slug.ends_with('-'),
                "slug is edged with a dash, so a name built from it reads as cut off: {:?}",
                slug
            );
        }
    }

    /// Implied by the property above over the printable space, and kept separate to reach the
    /// half that one cannot: control characters, lone surrogates' worth of odd Unicode, and
    /// whatever else an arbitrary `String` produces. A note's first line is the user's text.
    #[test]
    fn slugging_never_panics_on_any_first_line(text in any::<String>()) {
        let _ = slug_from_first_line(&text);
    }

    /// **Restricted to unslugged ids on purpose, and this is not a convenience.** Over all ids
    /// the property is false: `slugged_name("2026-08-15.md", "1548")` is
    /// `"2026-08-15-1548.md"`, whose stem is exactly a timestamp, so `is_slugged` reports
    /// `false` for a name that was just slugged. Unreachable in production because
    /// `save::slug_target` refuses to slug an already-slugged name in the first place — but it
    /// reds on run one, and the reader deserves to know the region exists.
    #[test]
    fn slugging_an_unslugged_name_makes_it_slugged(
        name in unslugged_name(),
        slug in slug(),
    ) {
        let slugged = slugged_name(&id(&name), &slug);

        prop_assert!(
            is_slugged(&id(&slugged)),
            "{:?} + {:?} produced {:?}, which still reads as unslugged",
            name,
            slug,
            slugged
        );
    }

    /// The immutability a stale manifest entry is matched by: the rename appends a slug and
    /// leaves the timestamp alone, which is what lets `order::reconcile` repair an entry whose
    /// file was renamed out from under it.
    #[test]
    fn slugging_keeps_the_timestamp_prefix(
        name in unslugged_name(),
        slug in slug(),
    ) {
        let slugged = slugged_name(&id(&name), &slug);

        prop_assert_eq!(
            timestamp_prefix(&slugged),
            timestamp_prefix(&name),
            "{:?} + {:?} produced {:?}, moving the prefix a repair depends on",
            name,
            slug,
            slugged
        );
        prop_assert_eq!(
            timestamp_prefix(&name).map(str::len),
            Some(TIMESTAMP_LEN),
            "the generator should only produce timestamp-stemmed names"
        );
    }

    /// Over *all* names, not just unslugged ones: `split_extension` splits on the last dot and
    /// a generated slug contains none, so the extension survives whatever the stem looks like.
    /// A note's extension is its syntax highlighting and its `Format`, so losing one silently
    /// changes how the note is read.
    #[test]
    fn slugging_preserves_the_extension(name in note_name(), slug in slug()) {
        let slugged = slugged_name(&id(&name), &slug);

        prop_assert_eq!(
            extension_of(&slugged),
            extension_of(&name),
            "{:?} + {:?} produced {:?}, changing the extension",
            name,
            slug,
            slugged
        );
    }

    /// `with_suffix` disambiguates a name that is already taken, so a suffixed name that
    /// equalled the original would make `free_name` spin to its attempt cap and fail.
    #[test]
    fn a_suffixed_name_keeps_its_extension_and_differs_from_the_original(
        name in note_name(),
        n in 1usize..1000,
    ) {
        let suffixed = with_suffix(&name, n);

        prop_assert_ne!(
            &suffixed,
            &name,
            "suffix {} left the name unchanged, so collision probing would never terminate",
            n
        );
        prop_assert_eq!(
            extension_of(&suffixed),
            extension_of(&name),
            "suffix {} moved the extension: {:?}",
            n,
            suffixed
        );
    }
}

/// The extension as `naming::split_extension` sees it, reimplemented here because that
/// function is `pub(crate)`.
///
/// One line, and deliberately not a wider export: the properties need to read an extension,
/// not to widen the crate's surface for a test's convenience.
fn extension_of(name: &str) -> Option<String> {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext.to_owned()),
        _ => None,
    }
}
