# Global hotkey setup

Scratchbox registers no global hotkey of its own. The desktop environment owns the
keyboard, so it is the desktop environment that binds the key — and on Wayland that is the
only approach that works at all, since its security model forbids background applications
from grabbing keys. The upshot is that you choose the key, and you choose what it runs.

There are two useful shapes for the shortcut.

**Capture** — the interesting one. A key that takes a line of text and appends it to the
active note, with no window and no context switch. This is what the CLI exists for.

**Open** — a key that launches the editor in your terminal.

Everything below assumes both binaries are on `PATH` (see the README's install section).
Use absolute paths if your desktop environment does not inherit your shell's `PATH`, which
is common; `command -v scratchbox` prints the one to use.

## macOS

### Shortcuts.app

1. Shortcuts → File → New Shortcut.
2. Add an **Ask for Text** action. Set the prompt to something like `Scratch:`.
3. Add a **Run Shell Script** action below it. Set Shell to `/bin/zsh`, set **Pass Input**
   to **to stdin**, and use this as the script:

   ```sh
   /Users/you/.cargo/bin/scratchbox
   ```

4. Name it, then Shortcut Details → **Add Keyboard Shortcut** and press your key.

Ask for Text feeds what you type straight into the CLI's stdin, which is exactly the
interface it wants. Nothing opens.

### skhd

If you already run [skhd](https://github.com/koekeishiya/skhd), `~/.skhdrc`:

```
# capture a line into the active note
cmd + shift - space : osascript -e 'text returned of (display dialog "Scratch:" default answer "")' | scratchbox

# open the editor in a new terminal window
cmd + shift - n : open -na Ghostty --args -e scratchbox-tui
```

Reload with `skhd --restart-service`.

## Linux — GNOME

Settings → Keyboard → **View and Customize Shortcuts** → **Custom Shortcuts** → **+**.

For capture, GNOME has no built-in text prompt, so use `zenity` (usually already
installed):

| Field | Value |
|---|---|
| Name | `Scratchbox capture` |
| Command | `sh -c 'zenity --entry --text="Scratch:" \| scratchbox'` |
| Shortcut | whatever you like — `Super+Shift+Space` is usually free |

For opening the editor, bind your terminal instead:

```sh
gnome-terminal -- scratchbox-tui
```

The `sh -c` wrapper matters: GNOME runs the command directly rather than through a shell,
so a bare pipe would be passed to `zenity` as arguments.

## Linux — KDE Plasma

System Settings → **Shortcuts** → **Add Command**.

Same two commands as GNOME. KDE ships `kdialog`, which fits better than `zenity`:

```sh
sh -c 'kdialog --inputbox "Scratch:" | scratchbox'
```

KDE also runs the command without a shell, so keep the `sh -c` wrapper around anything
containing a pipe.

## Windows

Deferred along with the rest of Windows support. Paths are handled portably throughout and
nothing here is Unix-specific by design, but no CI runner exercises Windows, so it is
untested rather than supported.

## Checking it works

From a terminal, before binding anything:

```sh
echo 'hotkey test' | scratchbox
```

Then open `scratchbox-tui` and confirm the line is at the end of the top note. If the
shortcut does nothing once bound, the cause is almost always `PATH` — run it with the
absolute path from `command -v scratchbox` and try again.
