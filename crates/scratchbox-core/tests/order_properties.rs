//! `order::reconcile`'s invariants, stated over all inputs.
//!
//! `reconcile` parses an untrusted file — the manifest is plain text, invites hand editing, and
//! arrives from other devices through whatever syncs the workspace — and its doc claims no
//! hostile line can reach a path *even in principle*. That is a claim about all inputs,
//! currently tested with the inputs someone thought of.
//!
//! Its own file rather than rows in `tests/order.rs`, whose every `reconcile` test encodes one
//! named rule and reads better for it.
//!
//! Pure and allocation-only: no filesystem, no watcher, nothing to be flaky about.

use std::collections::HashSet;
use std::time::SystemTime;

use proptest::prelude::*;
use scratchbox_core::note::NoteMeta;
use scratchbox_core::{NoteId, reconcile};

/// Collections are capped because `reconcile` is O(manifest × disk) — a linear scan per entry —
/// so an uncapped generator costs `cargo test` wall time without buying any coverage.
const MAX_NOTES: usize = 32;
const MAX_LINES: usize = 32;

// --- generators -------------------------------------------------------------------------

/// A name `NoteId::new` accepts.
///
/// Not `any::<String>()`: that would spend nearly every case being rejected at the door, and
/// rejection is what the hostile-manifest property covers.
///
/// Trailing whitespace is deliberately reachable — `NoteId::new("a ")` is `Ok` — because
/// narrowing a generator to hide the defect at issue #19 is what the review of this plan
/// explicitly forbade.
fn note_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 _-]{0,30}(\\.[a-z]{1,5})?"
}

/// A name that its own manifest line can match.
///
/// No surrounding whitespace, because `reconcile` parses each line with
/// `NoteId::new(line.trim())` while the disk id stays untrimmed — so a name carrying whitespace
/// never matches itself and loses its position on every reconcile. That is defect (a) of issue
/// #19, and this generator is where the order property steps around it. Everything else here
/// uses [`note_name`], which does reach it.
fn trackable_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9_-]{0,20}(\\.[a-z]{1,5})?"
}

/// `YYYY-MM-DD-HHMM`, the stem shape `naming::is_timestamp` accepts.
fn timestamp() -> impl Strategy<Value = String> {
    "[0-9]{4}-[0-9]{2}-[0-9]{2}-[0-9]{4}"
}

/// Anything a hand-edited or synced manifest might contain.
fn manifest_line() -> impl Strategy<Value = String> {
    prop_oneof![
        note_name(),
        Just("../../.ssh/id_rsa".to_owned()),
        Just("/etc/passwd".to_owned()),
        Just("..".to_owned()),
        Just(".hidden".to_owned()),
        Just(".scratchbox".to_owned()),
        Just("note\0.md".to_owned()),
        // `NoteId::new` rejects NUL, separators, and a leading dot. It does **not** reject
        // `\n`, `\t`, or `\x1b`, so these sit inside the reachable domain and are the half a
        // traversal property most needs to cover.
        "[\\x00-\\x1f\\x7f]{1,4}[a-z]{0,10}(\\.md)?",
        Just("a\nb.md".to_owned()),
        Just("x\u{1b}[2J.md".to_owned()),
        // The complement of Unicode category Other, so this arm generates **no** control
        // characters. It is the printable space and nothing more; the arms above are what
        // widen the domain past it. Worth stating because this arm was once annotated as
        // covering "whitespace-only, including control characters", which is its inverse — and
        // the whole generator was closed under "no control characters" as a result.
        "\\PC{0,40}",
    ]
}

/// A manifest line that names nothing on disk and cannot reach the rename-repair branch.
///
/// No timestamp stem, so `renamed_to` returns `None` before it looks at a single candidate.
fn junk_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("   ".to_owned()),
        Just("../../.ssh/id_rsa".to_owned()),
        Just("/etc/passwd".to_owned()),
        Just("..".to_owned()),
        Just(".hidden".to_owned()),
        Just("note\0.md".to_owned()),
        "zz-gone-[a-z]{1,10}(\\.md)?",
    ]
}

/// Notes on disk, names able to carry the whitespace at issue #19.
fn disk() -> impl Strategy<Value = Vec<NoteMeta>> {
    disk_of(note_name())
}

/// Notes on disk, every one matchable by its own manifest line.
fn trackable_disk() -> impl Strategy<Value = Vec<NoteMeta>> {
    disk_of(trackable_name())
}

/// Ids are unique, which is `reconcile`'s unstated precondition and what a directory listing
/// gives: `FolderSync::list` reads a directory, so it cannot produce the same name twice.
///
/// A `btree_map` rather than a `hash_map`, and that matters: `HashMap` iteration order is
/// randomised per process, so building a manifest from one would make a shrunk counterexample
/// unreproducible on the next run — which is the one thing a regression seed has to be.
fn disk_of(name: impl Strategy<Value = String>) -> impl Strategy<Value = Vec<NoteMeta>> {
    proptest::collection::btree_map(name, any::<SystemTime>(), 0..=MAX_NOTES)
        .prop_map(|notes| notes.iter().map(|(name, at)| meta(name, *at)).collect())
}

/// A stale manifest line and the disk name it was renamed to.
///
/// The repair branch needs a coincidence independent generators will not produce inside 256
/// cases: a line that parses as a `NoteId`, is absent from disk, and whose stem is exactly a
/// 15-character timestamp, plus exactly one disk note sharing both that prefix and that
/// extension. So the pair is constructed rather than hoped for.
fn renamed_pair() -> impl Strategy<Value = (String, String)> {
    (timestamp(), "[a-z0-9]+(-[a-z0-9]+){0,4}", "[a-z]{1,5}").prop_map(
        |(stem, slug, ext): (String, String, String)| {
            (format!("{stem}.{ext}"), format!("{stem}-{slug}.{ext}"))
        },
    )
}

/// A hostile manifest against a disk, with a constructed rename-repair pair mixed in.
fn hostile_pair() -> impl Strategy<Value = (Vec<NoteMeta>, Vec<String>)> {
    pair_over(disk())
}

/// [`hostile_pair`] over a disk whose every name can be matched by its own manifest line.
///
/// Idempotence needs this and the set-shaped properties do not: a whitespace-bearing disk name
/// never matches the line naming it, so it is treated as new on every pass, and a first
/// reconcile that moves it to the top differs from the no-manifest pass before it. Third
/// symptom of issue #19, found by this property rather than assumed.
fn trackable_hostile_pair() -> impl Strategy<Value = (Vec<NoteMeta>, Vec<String>)> {
    pair_over(trackable_disk())
}

fn pair_over(
    disk: impl Strategy<Value = Vec<NoteMeta>>,
) -> impl Strategy<Value = (Vec<NoteMeta>, Vec<String>)> {
    (
        disk,
        proptest::collection::vec(manifest_line(), 0..=MAX_LINES),
        proptest::option::of(renamed_pair()),
        any::<SystemTime>(),
        0usize..=MAX_NOTES,
    )
        .prop_flat_map(|(mut disk, mut lines, pair, at, remember)| {
            // Injected only when the constructed names do not collide with a generated one:
            // "absent from disk" is half the coincidence, and a collision would dissolve it.
            if let Some((stale, landed)) = pair
                && !disk
                    .iter()
                    .any(|note| note.id.as_str() == stale || note.id.as_str() == landed)
            {
                lines.push(stale);
                disk.push(meta(&landed, at));
            }

            // Lines naming notes that really are on disk.
            //
            // Load-bearing, and it took falsification to notice: two independent draws from a
            // 31-character name space never collide, so a manifest built only from
            // `manifest_line()` claims *nothing*, and every property here would silently be
            // exercising the "everything is new" path alone. Dropping `unclaimed.remove` from
            // the claim branch left the no-duplicates property green until these existed.
            lines.extend(
                disk.iter()
                    .take(remember)
                    .map(|note| note.id.as_str().to_owned()),
            );

            (Just(disk), Just(lines).prop_shuffle())
        })
}

/// A disk, and a manifest whose lines either name a note on it or name nothing at all.
///
/// The junk lines cannot reach the rename-repair branch and the names cannot carry whitespace,
/// so neither defect at issue #19 is in play — which is what makes order preservation true over
/// this domain and only this one. The repair branch is still covered, by the set, subset,
/// duplicate, and idempotence properties, none of which care where in the order it lands.
fn remembered_and_new() -> impl Strategy<Value = (Vec<NoteMeta>, Vec<String>)> {
    (
        proptest::collection::btree_map(trackable_name(), any::<SystemTime>(), 0..=MAX_NOTES),
        0usize..=MAX_NOTES,
        proptest::collection::vec(junk_line(), 0..=6),
    )
        .prop_flat_map(|(notes, remember, junk)| {
            let disk: Vec<NoteMeta> = notes.iter().map(|(name, at)| meta(name, *at)).collect();

            // The manifest knows about the first `remember` of them and has never heard of the
            // rest, so the output has both a remembered tail and a newest-first head.
            let mut lines: Vec<String> = notes.into_keys().take(remember).collect();
            lines.extend(junk);

            (Just(disk), Just(lines).prop_shuffle())
        })
}

// --- helpers ----------------------------------------------------------------------------

fn meta(name: &str, modified: SystemTime) -> NoteMeta {
    NoteMeta::from_id(
        NoteId::new(name).expect("the generators should only produce valid note names"),
        modified,
    )
}

fn names(order: &[NoteId]) -> Vec<&str> {
    order.iter().map(NoteId::as_str).collect()
}

/// The ids the manifest actually claimed, in the order it claimed them.
///
/// An oracle rather than a second copy of `reconcile`: trim each line, parse it, keep first
/// occurrences, and retain those whose exact name is on disk. It says nothing about where an
/// unclaimed note goes, which is the part `reconcile` decides.
fn claimed_ids(manifest: &[String], disk: &[NoteMeta]) -> Vec<String> {
    let on_disk: HashSet<&str> = disk.iter().map(|note| note.id.as_str()).collect();

    let mut seen = HashSet::new();
    let mut claimed = Vec::new();
    for line in manifest {
        let Ok(id) = NoteId::new(line.trim()) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        if on_disk.contains(id.as_str()) {
            claimed.push(id.as_str().to_owned());
        }
    }
    claimed
}

// --- properties -------------------------------------------------------------------------

proptest! {
    /// Nothing lost and nothing invented, whatever the manifest holds. The manifest is a hint;
    /// the disk is the truth.
    #[test]
    fn the_output_set_equals_the_disk_set((disk, manifest) in hostile_pair()) {
        let order = reconcile(&manifest, &disk);

        let got: HashSet<&str> = order.iter().map(NoteId::as_str).collect();
        let expected: HashSet<&str> = disk.iter().map(|note| note.id.as_str()).collect();

        prop_assert_eq!(
            got,
            expected,
            "reconcile changed which notes exist; manifest was {:?}",
            manifest
        );
    }

    /// A note listed twice would be selectable twice and deletable twice. The guard is
    /// `unclaimed.remove` in the claim branch, not the `seen` set — see the note on
    /// `reconcile`.
    #[test]
    fn the_output_never_repeats_a_note((disk, manifest) in hostile_pair()) {
        let order = reconcile(&manifest, &disk);

        let mut unique = HashSet::new();
        for id in &order {
            prop_assert!(
                unique.insert(id.as_str()),
                "{:?} appears more than once in {:?}",
                id.as_str(),
                names(&order)
            );
        }
    }

    /// The traversal claim, with content.
    ///
    /// "No output id fails `NoteId::new`" would be vacuous — the return type is `Vec<NoteId>`,
    /// so the type system already guarantees it and the assertion would test nothing. What has
    /// content is that every id came from the *disk*: a manifest line reading
    /// `../../.ssh/id_rsa` must not become a listed, selectable, deletable note however the
    /// rest of the file is arranged.
    #[test]
    fn every_output_id_came_from_disk((disk, manifest) in hostile_pair()) {
        let order = reconcile(&manifest, &disk);
        let on_disk: HashSet<&str> = disk.iter().map(|note| note.id.as_str()).collect();

        for id in &order {
            prop_assert!(
                on_disk.contains(id.as_str()),
                "{:?} is in the output but not on disk; the manifest was {:?}",
                id.as_str(),
                manifest
            );
        }
    }

    /// The order the user chose survives reconciliation.
    ///
    /// **Stated over claimed ids and over a domain neither defect at issue #19 can reach.**
    /// Over the general domain it is false, twice over: a name carrying whitespace never
    /// matches its own line, and a repaired id is pushed at the *stale* line's position, which
    /// can overtake a later line naming it directly. Both are ordering-only and pre-existing,
    /// both are reproduced in #19, and neither is fixed here — this phase adds a
    /// dev-dependency and must not change shipped behaviour.
    #[test]
    fn the_notes_the_manifest_claims_keep_their_relative_order(
        (disk, manifest) in remembered_and_new(),
    ) {
        let order = reconcile(&manifest, &disk);
        let claimed = claimed_ids(&manifest, &disk);

        let got: Vec<&str> = order
            .iter()
            .map(NoteId::as_str)
            .filter(|name| claimed.iter().any(|claim| claim == name))
            .collect();
        let expected: Vec<&str> = claimed.iter().map(String::as_str).collect();

        prop_assert_eq!(
            got,
            expected,
            "the manifest's own order was not kept; manifest was {:?}",
            manifest
        );
    }

    /// Feeding the output back in as the manifest is a fixed point.
    ///
    /// This is what makes reconciliation safe to run on every refresh: a startup, a rescan, and
    /// an external create all call it, and an order that drifted each time would walk the
    /// user's arrangement away from them one event at a time.
    ///
    /// **Stated over a disk whose names carry no surrounding whitespace, and that exclusion was
    /// discovered here rather than assumed.** A whitespace-bearing name never matches the
    /// manifest line naming it, so it is treated as new on every pass: the first reconcile
    /// moves it to the top and differs from the pass before it. It then converges, but to a
    /// fixed point where that note is pinned near the top and cannot be reordered at all,
    /// because every manifest `move_up` writes is discarded on the next reconcile. Filed as the
    /// third symptom on issue #19; not fixed here, which is a dev-dependency-only phase.
    #[test]
    fn reconcile_is_idempotent((disk, manifest) in trackable_hostile_pair()) {
        let once = reconcile(&manifest, &disk);
        let lines: Vec<String> = once.iter().map(|id| id.as_str().to_owned()).collect();
        let twice = reconcile(&lines, &disk);

        prop_assert_eq!(
            names(&twice),
            names(&once),
            "a second reconcile moved things; manifest was {:?}",
            manifest
        );
    }

    /// A mangled manifest costs the user their ordering and nothing else — least of all a
    /// panic, which would take the whole session down at startup.
    #[test]
    fn reconcile_never_panics(
        manifest in proptest::collection::vec(manifest_line(), 0..=MAX_LINES),
        disk in disk(),
    ) {
        let _ = reconcile(&manifest, &disk);
    }

    /// Proof that [`renamed_pair`] reaches the branch it exists for, rather than an assumption
    /// that a generator found the coincidence.
    ///
    /// The state a crash between the rename and the manifest write leaves behind: the manifest
    /// names a file that is gone while an apparently new file sits on disk. The stale line goes
    /// *second* on purpose — if the repair does not fire, the note is treated as new and lands
    /// on top, so the two outcomes are distinguishable rather than coincidentally equal.
    #[test]
    fn a_constructed_rename_pair_reaches_the_repair_branch(
        (stale, landed) in renamed_pair(),
        at in any::<SystemTime>(),
    ) {
        let manifest = vec!["zz-kept.md".to_owned(), stale];
        // The renamed note is the newest, so without the repair it would jump to the top.
        let disk = [meta("zz-kept.md", SystemTime::UNIX_EPOCH), meta(&landed, at)];

        let order = reconcile(&manifest, &disk);

        prop_assert_eq!(
            names(&order),
            vec!["zz-kept.md", landed.as_str()],
            "the repaired note did not hold the stale line's position"
        );
    }
}
