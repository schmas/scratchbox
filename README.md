# scratchbox

Ephemeral terminal scratchpad. Notes are plain files in a plain directory — no database,
no daemon, no network.

- `scratchbox-tui` — the TUI: ordered note list plus an editor pane, autosave on idle,
  live reflection of changes made outside the app.
- `scratchbox` — CLI that appends stdin to the active note without opening the TUI.

## Paths

XDG is honored on macOS too, following terminal-tool convention rather than
`~/Library/Application Support`.

| Purpose | Default |
|---|---|
| Config | `~/.config/scratchbox/config.toml` |
| Workspace | `~/.local/share/scratchbox/notes` |
| Trash | `~/.local/share/scratchbox/trash` |

The trash lives outside the workspace on purpose: deleted notes routinely hold secrets,
and a workspace pointed at a cloud folder would otherwise sync them.

`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` are honored when set.

## Configuration

`~/.config/scratchbox/config.toml`, all keys optional:

```toml
workspace = "~/notes"
trash = "~/.local/share/scratchbox/trash"
```

A missing config file means defaults. `--workspace <path>` overrides the config key.

## Platform support

Developed and tested on macOS and Linux. Windows is untested — paths are handled with
`PathBuf` throughout, but no CI runner exercises it.

## Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
