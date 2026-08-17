//! Two different syntax costs, measured separately because they are paid at different times.
//!
//! `syntax_set_load` is the one-time cost `syntax.rs` puts at "about 3ms against a 100ms startup
//! budget". `highlighter_per_frame` is what every repaint spends. Conflating them is easy and
//! wrong in both directions.

use criterion::{Criterion, criterion_group, criterion_main};
use scratchbox_core::Format;
use scratchbox_tui::syntax;

/// The load, called directly rather than through the public path.
///
/// Not `syntax::syntax_name` and not `syntax::highlighter`: both read the `SYNTAXES`
/// `LazyLock`, so the first iteration would measure a real load and every one after it would
/// measure something else entirely. The number under test here is the load, so this calls the
/// loader. A figure in nanoseconds rather than milliseconds means the `LazyLock` got into the
/// path anyway.
fn syntax_set_load(c: &mut Criterion) {
    c.bench_function("syntax_set_load", |b| {
        b.iter(|| std::hint::black_box(two_face::syntax::extra_newlines()));
    });
}

/// What a repaint pays, with the `LazyLock` already warm.
///
/// Beyond issue #17's candidate list, and here on purpose rather than smuggled in.
/// `highlighter()` is not the `Arc` clone it looks like: on every call it clones the `Arc`, scans
/// 220 syntaxes' extension lists, clones the matched `SyntaxReference`, and clones a whole
/// `Theme` and `ThemeSet` — and `ui::render` calls it once per frame. This repo already treats
/// repaint cost as load-bearing (`c2322b1 test(cli): bound the repaints one append causes`), so
/// the per-frame figure is the more decision-relevant of the two.
fn highlighter_per_frame(c: &mut Criterion) {
    // Force the load out of the measurement. Without this the first iteration would carry the
    // whole syntax set and the reported mean would be the load divided by the sample size.
    let _ = syntax::syntax_name(Format::Markdown);

    c.bench_function("highlighter_per_frame", |b| {
        b.iter(|| {
            std::hint::black_box(syntax::highlighter(std::hint::black_box(Format::Markdown)));
        });
    });
}

criterion_group!(benches, syntax_set_load, highlighter_per_frame);
criterion_main!(benches);
