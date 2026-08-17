//! The core numbers behind the 100ms startup budget.
//!
//! Everything here is on the path a startup actually walks: `FolderSync::list` reads the
//! workspace, `reconcile` merges the manifest with it, and `slug_from_first_line` runs on every
//! save that has not been named yet. Their costs were asserted in source comments; these turn
//! them into measurements with confidence intervals.
//!
//! State is built in the closure's outer scope, never inside `iter` — a bench that includes its
//! own setup measures the setup.

use std::fs;
use std::time::{Duration, SystemTime};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use scratchbox_core::note::{NoteId, NoteMeta};
use scratchbox_core::{FolderSync, Store, naming, order};

/// Note counts worth knowing the curve between.
///
/// `reconcile` is O(manifest × disk) with a linear scan per entry. Ten notes is the realistic
/// case and a thousand is well past it; the point is to have the shape as a measurement rather
/// than as an assertion in a comment.
const SIZES: [usize; 3] = [10, 100, 1000];

fn note_names(count: usize) -> Vec<String> {
    (0..count)
        .map(|n| format!("2026-08-{:02}-{:04}-note-{n}.md", n % 28 + 1, n % 1440))
        .collect()
}

fn disk_from(names: &[String]) -> Vec<NoteMeta> {
    names
        .iter()
        .enumerate()
        .map(|(n, name)| {
            NoteMeta::from_id(
                NoteId::new(name).expect("generated names are valid"),
                SystemTime::UNIX_EPOCH + Duration::from_secs(n as u64),
            )
        })
        .collect()
}

/// A deterministic permutation of `names`.
///
/// Deterministic because a bench whose input differs from run to run cannot be compared from run
/// to run, and `rand` is deliberately not a dependency of this project. A stride coprime with
/// the length visits every index exactly once, so this is a permutation rather than a sample —
/// 7 is coprime with 10, 100, and 1000.
fn shuffled(names: &[String]) -> Vec<String> {
    let len = names.len();
    (0..len).map(|i| names[i * 7 % len].clone()).collect()
}

fn reconcile(c: &mut Criterion) {
    let mut group = c.benchmark_group("reconcile");

    for count in SIZES {
        let names = note_names(count);
        let disk = disk_from(&names);
        // Every line names a note that is there, and in a different order, so the claim branch
        // runs `count` times. A manifest of junk would measure the rejection path instead.
        let manifest = shuffled(&names);

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| std::hint::black_box(order::reconcile(&manifest, &disk)));
        });
    }

    group.finish();
}

fn slug_from_first_line(c: &mut Criterion) {
    // A realistic first line: a Markdown heading with punctuation and an accent, so the
    // transliteration and the word-boundary truncation both run.
    let line = "# Meeting Notes: Q3 Planning — Café Discussion and Follow-up Items";

    c.bench_function("slug_from_first_line", |b| {
        b.iter(|| {
            std::hint::black_box(naming::slug_from_first_line(std::hint::black_box(line)));
        });
    });
}

fn folder_sync_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("folder_sync_list");

    for count in SIZES {
        // One populated workspace per size, built in setup: `list` reads the directory and
        // mutates nothing, so every iteration sees the same state.
        let tmp = tempfile::tempdir().expect("a temp dir");
        let workspace = tmp.path().join("notes");
        let store =
            FolderSync::new(workspace.clone(), tmp.path().join("trash")).expect("a workspace");
        for name in note_names(count) {
            fs::write(workspace.join(&name), "body").expect("a note");
        }

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| std::hint::black_box(store.list().expect("list should succeed")));
        });
    }

    group.finish();
}

criterion_group!(benches, reconcile, slug_from_first_line, folder_sync_list);
criterion_main!(benches);
