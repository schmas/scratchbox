//! Which syntax highlights a note, and the one theme that does it.
//!
//! Kept out of `scratchbox-core`: [`Format`] is a plain enum over file extensions, and the
//! headless rule says syntect types stop at the TUI boundary.

use std::sync::{Arc, LazyLock};

use edtui::syntect::highlighting::{Theme, ThemeSet};
use edtui::syntect::parsing::SyntaxSet;
use edtui::{SyntaxHighlighter, THEME_SET};
use scratchbox_core::Format;

/// The one theme, hard-coded.
///
/// Chosen by measurement rather than taste, because edtui applies only the foreground of a
/// highlighted span and drops the theme's background: every colour has to be legible
/// against a white terminal and a black one alike. Ranking all 43 bundled themes by the
/// worst contrast ratio their palette can produce against both extremes puts Solarized
/// first at 2.67:1, well clear of the next real candidate at 1.86:1 — which is the whole
/// point of Solarized, whose accents sit at a fixed mid-luminance so one palette serves
/// both its light and dark backgrounds. The ceiling for any colour readable on both is
/// 4.58:1, so 2.67 is a reasonable share of what is achievable.
pub const THEME_NAME: &str = "solarized-dark";

/// bat's syntax set rather than syntect's defaults.
///
/// syntect's bundled 75 syntaxes cover seven of the eight declared formats and miss
/// TypeScript outright. two-face ships bat's 220, which has it.
///
/// Measured at **0.79ms** to load against a 100ms startup budget, and only when a note is first
/// rendered — `benches/syntax.rs`, release, Apple M1 Pro. This comment previously said "about
/// 3ms", which was an estimate rather than a measurement and was roughly four times too
/// pessimistic. The number is worth having right in both directions: it is what the budget is
/// spent against.
///
/// Note that the *load* is not the cost a repaint pays. See [`highlighter`].
static SYNTAXES: LazyLock<Arc<SyntaxSet>> =
    LazyLock::new(|| Arc::new(two_face::syntax::extra_newlines()));

/// Resolved once so a mistyped name shows up as an unstyled editor in one place rather
/// than as a fresh lookup failure on every frame. `syntax_theme_resolves` guards the name.
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    THEME_SET
        .themes
        .get(THEME_NAME)
        .cloned()
        .unwrap_or_else(Theme::default)
});

/// The highlighter for a note of this format.
///
/// Never fails. An extension the syntax set does not know falls back to plain text, which
/// renders every line in the base style — the same thing highlighting-off would do, and a
/// far better answer than refusing to show the note.
///
/// **Called once per frame, from `ui::render`, and it is not free.** Every call clones the
/// `Arc`, scans 220 syntaxes' extension lists, clones the matched `SyntaxReference`, and clones
/// a whole `Theme` and `ThemeSet`. Measured at **8.0µs** per call with the `LazyLock` warm —
/// `benches/syntax.rs`, release, Apple M1 Pro. Comfortable against a repaint, and worth knowing
/// before anything makes this loop harder.
pub fn highlighter(format: Format) -> SyntaxHighlighter {
    let syntaxes = SYNTAXES.clone();
    let syntax = syntaxes
        .find_syntax_by_extension(format.extension())
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
        .clone();

    SyntaxHighlighter::with_sets(THEME.clone(), theme_set(), syntax, syntaxes)
}

/// The name of the syntax a format resolves to. For tests and nothing else — the
/// highlighter itself does not expose what it picked.
pub fn syntax_name(format: Format) -> &'static str {
    SYNTAXES
        .find_syntax_by_extension(format.extension())
        .map_or("Plain Text", |syntax| syntax.name.as_str())
}

fn theme_set() -> Arc<ThemeSet> {
    THEME_SET.clone()
}
