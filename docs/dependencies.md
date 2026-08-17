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

**Scope** answers "does this reach a user's machine?". `ships` is a normal dependency of at
least one binary; `dev-only` is a `[dev-dependencies]` entry, absent from
`cargo tree -e normal` for both binaries and from the shipped artifacts.

| Crate | Where | Scope | Why |
| --- | --- | --- | --- |
| `anyhow` | both binaries | ships | Error context chaining at the top level, where the only consumer is a human reading stderr. |
| `thiserror` | `scratchbox-core` | ships | Typed errors across a library boundary, so the TUI can distinguish a conflict from an I/O failure. |
| `serde` + `toml` | `scratchbox-core` | ships | Config parsing. TOML only — there is one config file and it is hand-edited. |
| `crossbeam-channel` | core, TUI | ships | Watcher events crossing into the event loop; also what `notify-debouncer-full` speaks. |
| `jiff` | `scratchbox-core` | ships | Timestamps for note names. Chosen over `chrono` and `time`. |
| `std::sync::LazyLock` | `scratchbox-tui` | ships | Syntax set and theme, initialised once on first render. Standard library — no `lazy_static`, no `once_cell`. |
| `tracing` | `scratchbox-core`, `scratchbox-tui` | ships | The facade, `default-features = false`. See *File-only diagnostics* below. |
| `tracing-subscriber` | `scratchbox-log` | ships | The subscriber, `features = ["fmt", "env-filter"]`. |
| `tracing-appender` | `scratchbox-tui` | ships | Daily rotation for the one process that stays open for hours. Deliberately not in `scratchbox`. |
| `proptest` | `scratchbox-core` | dev-only | Properties for `naming` and `order::reconcile`. See *Property tests* below. |

### File-only diagnostics

The filesystem watcher is the acknowledged risky part of the design — CI runs the watcher
suite 20 times per OS because an intermittent pass there is a race, not a flake — and
diagnosing one used to mean reasoning about interleaving with no record of it. `eprintln!`
cannot help: the TUI owns the terminal and `scratchbox` must stay silent on a hotkey. So there
is a structured log to a file, off unless `RUST_LOG` names a `scratchbox` target.

`scratchbox-log` exists as its own crate because both binaries need the subscriber, the core
must not have one, and the TUI cannot be a dependency of the CLI without dragging ratatui into
a hotkey-fired binary. It is the one place the "a file, never a standard stream" rule lives.

Load-bearing choices, recorded because each one looks like an incidental detail:

- **The log lives at `<data_home>/scratchbox/log/`, outside the workspace.** A log line
  describing a filesystem event, written inside the watched tree, produces that same event one
  debounce window later, which is logged, forever. `scratchbox_log::overlaps` refuses to
  install anything when the two overlap, in both directions and with both sides resolved
  through symlinks, for the same reason `Config::trash_overlaps_workspace` does.
- **File appender only, and `ansi` is not enabled.** Omitting the feature means `nu-ansi-term`
  is never linked, which is a stronger guarantee than remembering `.with_ansi(false)`.
- **`parse_lossy` and `from_env_lossy` are banned.** Both `eprintln!` their complaint, which
  would break the rule from inside the crate meant to hold it. `filter()` returns `Option` and
  swallows a parse failure.
- **No `max_level_*` or `release_max_level_*` feature.** A directive above `STATIC_MAX_LEVEL`
  makes `EnvFilter` print a multi-line warning to stderr, which has no `MakeWriter` and so
  cannot be redirected into the file.
- **`tracing` is taken `default-features = false`.** The default set includes `attributes`,
  which would add `tracing-attributes` — a proc-macro crate — to the build graph of
  `scratchbox-core`, the crate both binaries depend on. `span!`/`event!` cost a few more lines
  than `#[instrument]` and keep it out; verified absent from the workspace's normal graph.
  Note the narrower claim: `syn`/`quote`/`proc-macro2` are *already* in core's graph through
  `serde_derive` and `thiserror-impl`, so what this avoids is a third proc-macro crate's
  compile, not the subtree itself.
- **`tracing-appender` is a dependency of `scratchbox-tui` alone.** It requires `time`
  non-optionally, plus `symlink` for a `latest` link nothing here asks for. A cargo feature on
  `scratchbox-log` could not have achieved the split — features unify across two normal
  dependents in one workspace build, so `scratchbox` would have linked the appender anyway.
  Verified: `cargo tree -e normal -p scratchbox-cli` contains none of `tracing-appender`,
  `time`, or `symlink`. In the TUI, `time` was already arriving through `ratatui-widgets` and
  `syntect`→`plist`, so the appender adds only `symlink` there that is genuinely new — the
  rejection of `time` below is about writing against its API, not about the lockfile.
- **Diagnostics can never fail the operation they observe.** `scratchbox` reads stdin to EOF
  before it opens the workspace, so an error propagated out of setup would drain the pipe and
  drop the captured thought. Nothing on that path returns a `Result` a caller must handle, and
  `RollingFileAppender::builder().build()` is used instead of `rolling::daily`, which *panics*
  on a directory failure.
- **The file is bounded.** The TUI rotates daily and keeps 7; `scratchbox` truncates once the
  file passes `scratchbox_log::MAX_BYTES`.

### Property tests

`naming` and `order::reconcile` are pure, allocation-only, and have sharp invariants, and
`reconcile` parses an untrusted file — its doc claims no hostile manifest line can reach a path
*even in principle*, which is a claim about all inputs. Thirteen properties state those
invariants in `tests/naming_properties.rs` and `tests/order_properties.rs`.

`default-features = false` drops `fork` and `timeout` and with them 10 transitive crates. Those
features exist to survive a test that hangs or aborts the process; nothing under test here
touches the filesystem.

Two things worth knowing before changing them:

- **Every property is covered by a break that reds it.** A property no break can falsify is one
  nobody has shown to be sensitive. Two breaks are recorded as *not* falsifying anything, so
  they are not worth retrying: removing the `seen` guard in `reconcile` (the output duplicate is
  prevented by `unclaimed.remove`, not by `seen`), and dropping `unclaimed.remove` (which reds
  no-duplicates rather than set equality — the output holds the note twice, but as a *set* it
  still equals the disk).
- **Two properties are deliberately narrower than they look.** Order preservation and
  idempotence exclude names carrying surrounding whitespace and manifests that reach the
  rename-repair branch, because `reconcile` is genuinely wrong there — issue #19, reproduced,
  ordering-only, and pre-existing. The narrowing is commented at each property with the issue
  number rather than quietly applied, and the generators still produce whitespace so nothing is
  hidden.

Regression seeds land in `crates/scratchbox-core/tests/<name>.proptest-regressions` — sibling
files, not a directory — and are not gitignored, so a genuine counterexample is committed and
replayed before any novel case.

## Worth adopting

Tracked as issues rather than applied directly, because each one deserves review on its own.

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

### `clap` — once the CLI grows a second verb

Both binaries hand-roll argument parsing, about 40 lines each. That is correct today and
covered by tests, so there is nothing to fix yet. What makes it worth replacing is not the
parsing but the *rules between* flags.

`scratchbox-cli` already has one: `--yes` is meaningless without `--purge-trash`, rejected
by hand. Issue #8 adds `--new` and `--overwrite-current`, which are mutually exclusive with
each other and with the default append. That turns one hand-written conflict rule into
several, spread across a `while let` loop — which is where hand-rolled parsing starts
quietly accepting nonsense combinations. `conflicts_with` and `ArgGroup` state those rules
once, declaratively, and `--help` stays correct for free.

Startup cost was the original objection and it does not hold up. Measured on macOS, 300
warm execs of a stripped release binary taking the same flags:

| | hand-rolled | `clap` derive |
| --- | --- | --- |
| mean per-exec | 1.05 ms | 0.98 ms |
| stripped size | 342 KB | 729 KB |
| cold build | 1.2 s | 5.5 s |

Latency is identical — argument parsing is argv iteration either way, and the difference
disappears into process spawn. The real costs are 388 KB of binary, which is page-cache
resident for something fired repeatedly from a hotkey, and proc-macro build time.

Do it with #8, not before: today's four stable flags genuinely do not need it, and adopting
it alongside the new verbs means the conflict rules are declarative from the start rather
than hand-written and then migrated.

## Deliberately not used

### Rejected on merit

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
