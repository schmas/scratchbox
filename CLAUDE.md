# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Ephemeral terminal scratchpad. Notes are plain files in a plain directory — no database, no daemon, no network. Two binaries share one workspace: `scratchbox-tui` (editor) and `scratchbox` (CLI that appends stdin to the active note).

**Further reading:** `README.md` (user-facing behaviour), `docs/hotkeys.md`.

## Tech Stack

- **Language:** Rust 1.97.1 (pinned in `rust-toolchain.toml`, includes rustfmt + clippy)
- **TUI:** ratatui + edtui (editor widget) + crossterm
- **Filesystem watching:** notify / notify-debouncer-full
- **Config/paths:** directories (XDG, honored on macOS too — not `~/Library/Application Support`)
- **Serialization:** serde + toml; **time:** jiff

## Commands

```bash
# Build
cargo build --workspace

# Run
cargo run -p scratchbox-tui
echo 'text' | cargo run -p scratchbox-cli

# Test (whole workspace)
cargo test --workspace

# Test (single test)
cargo test -p scratchbox-core --test watcher
cargo test -p scratchbox-tui --test save_flow repeated_saves_under_a_live_watcher

# Lint / format (CI-enforced, -D warnings)
# Note: --all-targets lints the criterion benches too, so bench code is held to the same bar.
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

# Benchmarks (criterion, dev-only). `--benches` is not optional shorthand — without it cargo
# also selects lib and bin targets, whose libtest harness rejects any criterion flag after `--`.
cargo bench --workspace --benches
cargo bench --workspace --benches -- --quick     # one fast pass, no full statistics

# Diagnostics: off unless RUST_LOG names a `scratchbox` target. Written to
# <data_home>/scratchbox/log/, never to stdout or stderr.
RUST_LOG=scratchbox=debug cargo run -p scratchbox-tui
SCRATCHBOX_LOG_DIR=/tmp/sbx-log RUST_LOG=scratchbox=debug \
  cargo test -p scratchbox-core --test watcher   # test binaries log here

# edtui/ratatui compatibility spike (CI risk gate)
cargo run -p scratchbox-tui --example spike_editor
```

## Directory Structure

```
.
├── crates/
│   ├── scratchbox-core/   # headless: config, note, store, order, watcher, foldersync — no terminal/widget types
│   ├── scratchbox-log/    # file-only diagnostics: activation gate, writer, subscriber — never a standard stream
│   ├── scratchbox-cli/    # `scratchbox` bin: append stdin to active note
│   └── scratchbox-tui/    # `scratchbox-tui` bin: editor pane + note list
└── docs/
    ├── hotkeys.md         # per-OS global hotkey binding (app registers none itself)
    └── dependencies.md    # what we depend on, what we refuse, and why
```

## Conventions

- `scratchbox-core` stays headless — no ratatui/crossterm types leak into it; TUI and CLI both depend on it. It takes the `tracing` facade only: a library that installed a subscriber would decide for both binaries where diagnostics go.
- Diagnostics are file-only and off unless `RUST_LOG` names a `scratchbox` target. Nothing in this project writes a diagnostic to stdout or stderr — the TUI owns the terminal and `scratchbox` runs from a hotkey. `eprintln!` is reserved for fatal errors and config warnings, which are user-facing messaging rather than diagnostics.
- The trash directory lives outside the workspace on purpose (deleted notes may hold secrets; must never sync to a cloud-synced workspace).
- The normal-mode keymap is declared once, in `keys::BINDINGS`: dispatch scans it, and the status line and the help panel read it, so a rebind moves the behaviour and the text describing it together. The two prompt keymaps (`map_confirmation`, `map_conflict`) are the exception — their rule is a catch-all rather than a set of chords, so their help rows are written out and held to the code by an agreement test in `tests/help_panel.rs`.
- Key routing lives in `input::handle_key`, not beside the event loop, so which modal owns the keyboard can be driven from a test without a terminal.
- Filesystem-watcher tests are inherently racy across inotify/FSEvents; CI runs the watcher suite and the live-watcher save test 20x per OS rather than once.

## Rules

### Always

- Run `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` before considering work done — CI fails the build otherwise.
- Keep `scratchbox-core` free of terminal/widget dependencies.
- Before adding, removing, or upgrading a dependency, read `docs/dependencies.md`. It records what is already in use, what was turned down and why, and which rejections have a stated condition for revisiting. Several obvious-looking suggestions (`clap`, `mockall`, `itertools`, `uuid`, `chrono`) are already decided there.

### Never

- Add a database, daemon, or network dependency — the project's core premise is plain files, no daemon, no network. See `docs/dependencies.md` for the full rejection list.
- Have the app register its own global hotkey — desktop-environment binding is deliberate (Wayland forbids background key grabs).
