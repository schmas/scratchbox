//! Note naming: timestamp names and the frozen slug.
//!
//! A note is born as `YYYY-MM-DD-HHMM.<ext>`. On the first save where line 1 has content
//! it is renamed once to `YYYY-MM-DD-HHMM-<slug>.<ext>`, and the name freezes there —
//! later edits to line 1 never re-slug, so a note's name stays stable enough to reference.

use jiff::Zoned;

use crate::note::NoteId;

/// Longest slug appended to a note name.
pub const MAX_SLUG_LEN: usize = 40;

/// Width of the `YYYY-MM-DD-HHMM` prefix.
const TIMESTAMP_LEN: usize = 15;

const TIMESTAMP_FORMAT: &str = "%Y-%m-%d-%H%M";

/// Name for a freshly created note: `YYYY-MM-DD-HHMM.<ext>` in local time.
///
/// Two notes created in the same minute collide. That is resolved where the directory
/// listing is available, at the file-creation call site.
pub fn new_note_name(now: &Zoned, ext: &str) -> String {
    format!("{}.{ext}", now.strftime(TIMESTAMP_FORMAT))
}

/// Derive a slug from a note's first line, or `None` if the line yields nothing usable.
///
/// `None` means "still unslugged" — the caller retries on a later save rather than
/// inventing a name.
pub fn slug_from_first_line(text: &str) -> Option<String> {
    let first = text.lines().next().unwrap_or_default();
    // Line 1 is a Markdown heading often enough that the hashes are decoration, not words.
    let first = first.trim_start().trim_start_matches('#');

    // `slugify` transliterates rather than drops, so an emoji-only line comes back as its
    // Unicode name — "🎉🎉🎉" becomes "tada-tada-tada". A line carrying no letter or digit
    // has no title in it, so it leaves the note unslugged until a later save.
    if !first.chars().any(char::is_alphanumeric) {
        return None;
    }

    let slug = slug::slugify(first);
    let slug = truncate_on_word_boundary(&slug, MAX_SLUG_LEN);

    if slug.is_empty() { None } else { Some(slug) }
}

/// Has this note already been renamed to its frozen slug?
///
/// Derived from the name's shape: a stem that is exactly a timestamp has not been slugged
/// yet. Anything else — including a file the user created by hand — counts as slugged and
/// is never renamed.
pub fn is_slugged(id: &NoteId) -> bool {
    let (stem, _) = split_extension(id.as_str());
    !is_timestamp(stem)
}

/// Build the slugged name, keeping the original timestamp prefix and extension.
pub fn slugged_name(original: &NoteId, slug: &str) -> String {
    let (stem, ext) = split_extension(original.as_str());
    let prefix = match stem.split_at_checked(TIMESTAMP_LEN) {
        Some((head, _)) if is_timestamp(head) => head,
        _ => stem,
    };

    match ext {
        Some(ext) => format!("{prefix}-{slug}.{ext}"),
        None => format!("{prefix}-{slug}"),
    }
}

/// Disambiguate a name that is already taken: `note.md` becomes `note-2.md`.
///
/// Used for two notes created in the same minute, for a rename onto an existing name, and
/// for two same-named notes deleted into one trash directory.
pub fn with_suffix(name: &str, n: usize) -> String {
    let (stem, ext) = split_extension(name);
    match ext {
        Some(ext) => format!("{stem}-{n}.{ext}"),
        None => format!("{stem}-{n}"),
    }
}

/// Split a note name into stem and extension. Note names never start with `.`, so there is
/// no dotfile ambiguity here.
pub(crate) fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    }
}

/// Does this string have the exact shape `YYYY-MM-DD-HHMM`?
fn is_timestamp(s: &str) -> bool {
    const DASHES: [usize; 3] = [4, 7, 10];
    let bytes = s.as_bytes();
    bytes.len() == TIMESTAMP_LEN
        && bytes.iter().enumerate().all(|(i, byte)| {
            if DASHES.contains(&i) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// Cut a slug to `max`, preferring a `-` boundary so words survive intact. A single word
/// longer than the limit is hard-truncated — a too-long name beats no name.
///
/// Slug output is ASCII, so byte indexing is character indexing here.
fn truncate_on_word_boundary(slug: &str, max: usize) -> String {
    if slug.len() <= max {
        return slug.to_owned();
    }
    let head = &slug[..max];
    let cut = match head.rfind('-') {
        Some(0) | None => head,
        Some(boundary) => &head[..boundary],
    };
    cut.trim_end_matches('-').to_owned()
}
