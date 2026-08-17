# Dependency policy

What scratchbox depends on, what it deliberately does not, and why. Recorded so the same
general-purpose "production Rust stack" list can be answered once instead of re-litigated
every time it appears.

## The constraint

Scratchbox is a local, single-user, offline scratchpad: two small binaries over a directory
of plain files. No database, no daemon, no network — that is the product, not an
implementation detail. Total source is under 2k lines.

Most "production-grade Rust stack" advice is written for network services: async runtimes,
HTTP clients, SQL drivers, metrics exporters, allocator swaps. None of that has a job here,
and each dependency costs compile time, audit surface, and — for `scratchbox`, which runs
on a global hotkey — process startup.

A dependency earns its place by removing a real problem this codebase has. Convenience for
code that already works and is tested is not a real problem.

## In use

| Crate | Where | Why |
| --- | --- | --- |
| `anyhow` | both binaries | Error context chaining at the top level, where the only consumer is a human reading stderr. |
| `thiserror` | `scratchbox-core` | Typed errors across a library boundary, so the TUI can distinguish a conflict from an I/O failure. |
| `serde` + `toml` | `scratchbox-core` | Config parsing. TOML only — there is one config file and it is hand-edited. |
| `crossbeam-channel` | core, TUI | Watcher events crossing into the event loop; also what `notify-debouncer-full` speaks. |
| `jiff` | `scratchbox-core` | Timestamps for note names. Chosen over `chrono` and `time`. |
| `std::sync::LazyLock` | `scratchbox-tui` | Syntax set and theme, initialised once on first render. Standard library — no `lazy_static`, no `once_cell`. |

## Worth adopting

These are tracked as issues rather than applied directly, because each one is a change to
how the project is tested or diagnosed and deserves review on its own.

### `proptest` — property tests for the pure logic

`naming` and `order::reconcile` are pure functions with sharp invariants, and `reconcile`
parses an untrusted file. Example-based tests cover the cases someone thought of; the
interesting failures here are the ones nobody thought of.

Invariants worth stating as properties: a slug never exceeds `MAX_SLUG_LEN`; a name that
survives `slugged_name` reports `is_slugged`; `reconcile` returns exactly the set of notes
on disk with no duplicates and no invented entries, whatever the manifest contains,
including `../../.ssh/id_rsa`, absolute paths, and NUL bytes.

### `tracing` + `tracing-subscriber` + `tracing-appender` — file-only diagnostics

The filesystem watcher is the acknowledged risky part of the design — CI runs the watcher
suite 20 times per OS because an intermittent pass there is a race, not a flake. Diagnosing
one currently means reasoning about interleaving without a record of it.

`eprintln!` cannot help: the TUI owns the terminal, and `scratchbox` must stay silent on a
hotkey. A structured log to a file, off unless `RUST_LOG` says otherwise, gives watcher and
suppression events a timeline — and gives a CI failure an artifact to upload.

Constraint: file appender only. Nothing in this project may write diagnostics to stdout or
stderr during normal operation.

### `rstest` — parameterized cases for table-shaped tests

`naming`, `order`, and `config` tests are largely input/expected tables written out as
individual `#[test]` functions. `#[case]` collapses the repetition while keeping one
reported failure per input.

### `criterion` — statistics behind the startup budget

The budget is already asserted in comments — a 100ms startup target, ~3ms to load the
syntax set — and `scratchbox-tui --bench-first-frame` exists to measure it by hand.
Criterion turns that into a repeatable measurement with confidence intervals over
`reconcile` at realistic note counts, syntax-set load, and slug derivation. Dev-dependency
only; nothing ships.

## Deliberately not used

### Rejected on merit

**`clap`.** Argument parsing is roughly 40 lines per binary, correct, and covered by tests.
`scratchbox` is fork-exec'd from a keypress, so its startup is user-visible latency, and it
takes four flags that have not changed. Revisit if the flag surface grows enough that
hand-rolled parsing starts dropping cases — not before.

**`itertools`.** Would tighten perhaps three loops. Not a dependency's worth of value.

**`rustc-hash`.** The only hash set in the codebase holds a handful of note names during
reconciliation. Faster hashing of ten elements is not a measurable win.

**`mockall`.** The seam under test is the filesystem, and the tests drive a real one through
`tempfile`. For an application whose entire behaviour is what it does to files, real files
are a better oracle than a mocked trait — a mock cannot reproduce an FSEvents coalesce.

### Rejected as out of scope

Not applicable to a local, offline, file-backed tool with no server component:

- **Async and web** — `tokio`, `axum`, `reqwest`, `tonic`/`prost`, `sqlx`. There is no
  network and no database, by design. There is no async: the TUI is a blocking event loop
  reading a channel, which is the right shape for it.
- **Metrics** — `metrics`, `prometheus`. Nothing to scrape and nowhere to scrape it from.
- **`uuid`** — notes are identified by their filename, deliberately, so they stay
  greppable and meaningful outside the app. An opaque identifier would undo that.
- **`chrono` / `time`** — `jiff` already covers this and is the newer design.
- **`serde_json`, `serde_yaml`, `bincode`** — one TOML config file, and notes are the
  user's own text. No cache and no wire format to encode.
- **`rayon`** — a workspace holds tens of files; there is no data-parallel work.
- **`tikv-jemallocator`** — allocator tuning targets long-lived multi-threaded servers
  under fragmentation pressure. This is a short-lived process editing strings.
- **`flume`** — `crossbeam-channel` is already here and already required by the debouncer.
- **`indicatif`** — nothing runs long enough to need a progress bar.
- **`inquire` / `dialoguer`** — the one prompt in the codebase is the trash-purge
  confirmation, ten lines of `std`, and it must degrade correctly when stdin is not a
  terminal.
- **`owo-colors` / `colored`** — ratatui owns styling inside the TUI, and the CLI's output
  is a handful of plain lines.

## Local tooling

Not dependencies — install them if you want them. `cargo-watch` or `bacon` gives a
recompile-and-test loop while editing. Neither appears in any manifest.
