# scratchbox

Ephemeral terminal scratchpad. Notes are plain files in a plain directory — no database,
no daemon, no network.

- `scratchbox-tui` — the TUI: ordered note list plus an editor pane, autosave on idle,
  live reflection of changes made outside the app.
- `scratchbox` — CLI that appends stdin to the active note without opening the TUI.

## Install

```sh
cargo install --path crates/scratchbox-tui
cargo install --path crates/scratchbox-cli
```

That puts `scratchbox-tui` and `scratchbox` in `~/.cargo/bin`. Both read the same
configuration and the same workspace.

## Using it

Open the editor:

```sh
scratchbox-tui                      # the configured workspace
scratchbox-tui --workspace ~/notes  # or a different one
```

Capture a thought without opening anything:

```sh
echo 'ask about the invoice' | scratchbox
pbpaste | scratchbox
some-command 2>&1 | scratchbox
```

The text is appended to the **active note** — the one at the top of the list, which is the
note the editor opens on. A newline is inserted first if the note does not already end in
one, so nothing is ever joined onto an existing line. An empty workspace gets its first
note created for it.

Piped input is required. Run `scratchbox` with nothing on stdin and it prints this usage
and exits rather than sitting there looking like it has hung.

If the editor happens to be open on the same note, its watcher picks the append up within
about a second. Unsaved edits are never overwritten: the editor shows
`external change — [k]eep mine · [t]ake theirs` and waits for you to choose.

## Keybindings

Press `^H` (or `F1`) inside the editor for the full list. `/` searches it, `⏎` runs the
selected binding, `esc` closes it. The list is generated from the app's own keymap rather than
written out beside it, so it cannot fall behind the keys it describes — which is why there is no
table of them here. The two prompts' keys are the exception, and a test keeps those honest.

The editor pane is [edtui](https://github.com/preiter93/edtui) in **emacs mode**, and it is
**modeless**: there is no normal/insert distinction to leave. `^N`, `^D`, and `^H` are taken by
scratchbox and do not reach the editor; everything else does. Nothing is lost to `^H` — it
would have been edtui's delete-character-backward, which is what `Backspace` already does.

## Paths

XDG is honored on macOS too, following terminal-tool convention rather than
`~/Library/Application Support`.

| Purpose | Default |
|---|---|
| Config | `~/.config/scratchbox/config.toml` |
| Workspace | `~/.local/share/scratchbox/notes` |
| Trash | `~/.local/share/scratchbox/trash` |
| Diagnostic log | `~/.local/share/scratchbox/log` — only exists if you ask for one, see [Diagnostics](#diagnostics) |

`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` are honored when set.

## Trash

Deleting a note moves it to the trash directory. That directory lives **outside the
workspace on purpose**: deleted notes routinely hold API keys, passwords, and half-written
messages, and a workspace pointed at a cloud folder would otherwise upload them and keep
them there indefinitely. The trash is local, and it never syncs.

Nothing is ever deleted from it automatically — a scratchpad that quietly destroys things
on a timer is worse than one that uses some disk. Empty it when you want to:

```sh
scratchbox --purge-trash        # asks first
scratchbox --purge-trash --yes  # for scripts and cron
```

Purging only ever touches the trash directory. If `trash` is misconfigured so that it and
the workspace sit inside one another, the purge refuses outright rather than risk taking
live notes with it.

## Configuration

`~/.config/scratchbox/config.toml`, all keys optional:

```toml
workspace = "~/notes"
trash = "~/.local/share/scratchbox/trash"
```

A missing config file means defaults. `--workspace <path>` overrides the config key.

## Diagnostics

**Off by default, and off means nothing exists** — no directory, no file, no thread. Turn it on
by naming a `scratchbox` target in `RUST_LOG`:

```sh
RUST_LOG=scratchbox=debug scratchbox-tui
RUST_LOG=scratchbox=trace echo 'a thought' | scratchbox
```

A bare `RUST_LOG=info` deliberately does **not** switch it on. `RUST_LOG` is shared across the
whole Rust ecosystem, and having it in your shell profile should not silently earn you a log
directory here.

The log goes to a file and never to your terminal — the editor owns the screen, and `scratchbox`
runs from a hotkey where a stray line would be a visible defect. What lands there:

- Note **ids and byte counts**, never note text. A control character in a note's name is escaped
  rather than written through, so reading the file cannot execute anything in your terminal.
- The watcher's events, the suppression decisions, saves, conflicts, and reloads — in order, with
  millisecond timestamps. This is what makes an intermittent "the editor reloaded over my edit"
  report diagnosable rather than a matter of reasoning about interleavings.

The directory is created `0700` and the log `0600`, for the same reason the trash sits outside the
workspace: a scratchpad collects API keys and passwords whether or not it was meant to. It also
lives outside the workspace so writing to it cannot wake the watcher that is describing it.

It is bounded: `scratchbox-tui` rotates daily and keeps seven days, `scratchbox` truncates once the
file passes 8 MB. Nothing here can fail the thing it is observing — a bad `RUST_LOG` or an
unwritable directory costs you the log and nothing else, and `scratchbox` still captures the note.

Remove it whenever you like; nothing depends on it:

```sh
rm -rf ~/.local/share/scratchbox/log
```

## Global hotkey

`scratchbox` is built to be the target of a system-wide shortcut: press a key, type a
thought, and it lands in the top note without a window opening. Binding it is a job for the
desktop environment rather than the app — see [docs/hotkeys.md](docs/hotkeys.md) for
macOS, GNOME, and KDE.

Scratchbox registers no global hotkey itself, and that is deliberate rather than an
omission. Wayland's security model forbids background applications from grabbing keys, so
binding through the desktop environment is the approach that actually works — and it leaves
the choice of key, and of terminal, with the user.

## Platform support

Developed and tested on macOS and Linux. Windows is untested — paths are handled with
`PathBuf` throughout, but no CI runner exercises it.

## Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

What the project depends on, and what it turns down on purpose, is recorded in
[docs/dependencies.md](docs/dependencies.md).
