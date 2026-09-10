# Mac interaction mode

ChonkStep's Mac profile runs in **chonkstep-wayland**, including applications
running through XWayland. Command and Option retain their physical identities;
the compositor translates complete application shortcuts and owns desktop
commands. This works with Linux on Apple hardware; it does not run macOS apps.

The implementation and tests below cover the current profile. The broader
[macOS parity plan](mac-mode-plan.md) also includes work that is still outstanding;
this profile does not claim complete macOS parity.

## Enable and inspect

Put this at the top level of `~/.config/chonkstep/config.toml`:

```toml
interaction_mode = "mac"
```

It can coexist with `desktop = "omarchy"`: theme, shell, input-device settings,
and service commands remain available. Remove an explicit `keymap` setting;
Mac mode supplies its own bindings. `[keybindings]` overrides still win.
Mac mode defaults to click-to-focus and disables modifier-drag gestures so
Option remains available to applications.

```sh
chonkstep-wayland --check-config
chonkstep-wayland --print-config
# With the new binary running, apply configuration edits:
/path/to/chonkstep/scripts/reload.sh
```

`--print-config` includes the effective Mac system shortcuts, disabled defaults,
and application overrides. An invalid reload retains the working configuration.
Set `interaction_mode = "desktop"` to return to the existing desktop profile.
Hidden windows and fullscreen desktops are restored when leaving Mac mode.

The standalone `chonkstep` X11 session rejects Mac mode because it does not
implement the client delivery mechanism. X11 applications work inside the
Wayland session through XWayland.

## Desktop shortcuts

| Shortcut | Implemented behavior |
| --- | --- |
| Command-Tab / Command-Shift-Tab | Switch applications in MRU order; commit when both Command keys are released; Escape cancels |
| Command-grave / Command-Shift-grave | Cycle visible windows of the current application |
| Command-Q | Request closure of application windows through their close protocol; preserve save prompts; finish clipboard handoff before closure |
| Command-Option-Escape | Choose an application to force quit, then explicitly confirm; Escape and the default Cancel row preserve it |
| Command-H / Command-Option-H | Hide the application / hide other applications |
| Command-M / Command-Option-M | Minimize the window / the application's windows |
| Control-Up or F3 | Window overview with desktop controls |
| Control-Down | Overview filtered to the current application |
| Control-Left / Control-Right | Previous / next existing desktop; an edge does not create a desktop |
| Control-Command-F | Enter or leave a dedicated fullscreen desktop; restore geometry and remove the empty desktop on exit |
| Command-F3 or F11 | Show Desktop; repeat to restore the previously visible windows |
| Command-Space | ChonkStep application/root menu |
| Command-Option-D | Show or hide the Dock |
| Control-Command-Q | Lock through the configured `lock_command`, or the `mac-lock` provider |

Applications are grouped by app ID / WM_CLASS, including later identity updates,
and transient windows belong to their application. Hide, minimize, and Show
Desktop maintain separate state. The overview's existing desktop controls create
and remove desktops; deleting a live fullscreen desktop is declined until its
window exits fullscreen. Desktop membership is currently global across displays.

Command-Q requests graceful window closure; it never falls back to force-killing
an X11 client that lacks WM_DELETE_WINDOW. A Linux app can keep a background
process after closing its windows. Confirmed Force Quit disconnects all matching
application clients, including XWayland clients that ignore polite close. It can
discard unsaved changes; the chooser defaults to Cancel.

## Application commands

For ordinary GUI applications, common Command-letter commands use their native
Control equivalents: copy, cut, paste, select all, undo/redo, new/open/save/print,
find, tabs, and zoom. This preserves the application's clipboard formats, editing
context, save dialogs, and tab behavior. Command-W goes to the app, rather than
closing its whole window through the compositor.

Command-Left/Right become Home/End; Command-Up/Down become Control-Home/End.
Shift preserves selection. Option-arrow and Option-Backspace/Delete use the
corresponding Control editing operations in GUI applications. Command-period
sends Escape; Command-Option-Shift-V uses the application's plain-paste shortcut.
Option character composition and AltGr are not globally remapped.

Terminal copy/paste use Control-Shift-C/V. Physical Control-C still reaches the
terminal as Control-C. Single-window terminals (foot, Alacritty, xterm, st) close
through their window protocol for Command-W. Terminal navigation and other raw
Control input remain with the terminal application.

Browser profiles also translate Command-brackets to Back/Forward,
Command-Shift-brackets and Command-Option-arrows to adjacent tabs, and
Command-Option-I/J/U to developer tools, console, and source.
File-manager profiles use native Open, enclosing-folder, Trash, Properties,
Home, and location operations. Command-D is not translated into Linux's
unrelated bookmark operation.

A shortcut only performs an operation an application actually supports. Contexts
such as VS Code's integrated terminal require application bindings for full
fidelity; a compositor cannot infer every focused widget from app ID alone.

### Application overrides

```toml
[mac]
clipboard_persistence = true

[mac.applications]
"my-renamed-foot" = "terminal-window"
"custom-browser" = "browser"
"my-editor-with-command-bindings" = "native"
"my-remote-desktop" = "passthrough"
```

Profiles are exact, case-insensitive IDs: `gui`, `terminal`, `terminal-window`,
`browser`, `files`, `native`, and `passthrough`. Native skips application
translation while retaining desktop shortcuts. Passthrough leaves desktop
shortcuts with the client too. Common remote-desktop and VM applications default
to passthrough. Wayland shortcut inhibitors and XWayland keyboard grabs also
receive original input. Session locks retain their own keyboard input.

## Clipboard and capture

The existing Wayland/XWayland bridge preserves text, HTML, PNG, URI lists, and
binary representations without converting everything to plain text. PRIMARY
selection stays separate from the clipboard.

After Command-C/X, subsequent keyboard commands wait for the source's clipboard
offer before switching or pasting. This preserves a rapid copy → switch → paste
sequence when a toolkit publishes its copy asynchronously. Releases needed by
the copying application still arrive immediately; client dispatch and pointer
motion continue. A 250 ms deadline releases the sequence if the application has
no selection or does not publish an offer. The queue is bounded and resets on
lock/resume. This does not grant an unfocused client clipboard ownership.

Mac mode retains one clipboard snapshot in memory, with no disk history, and
restores it when its owner exits. Data-control clipboard managers remain usable;
ChonkStep does not continuously claim their live offers. Old protocol offers
hold weak references so clients retaining stale offers cannot retain every
clipboard generation. Active transfers have bounded lifetime and memory.

Persistence accepts up to 32 formats and 64 MiB total per offer, with a ten-second
transfer deadline and at most 16 concurrent consumers. Confidential-data hints
and `application/x-nopersist` disable retention. Failed or oversized snapshots
leave the original live offer in place and report a diagnostic; persistence of
such an offer after its owner exits is not guaranteed. Set
`mac.clipboard_persistence = false` to disable new snapshots.

| Shortcut | Capture destination |
| --- | --- |
| Command-Shift-3 | Save the desktop image |
| Command-Shift-4 | Select an area; Space selects a window; Escape cancels |
| Command-Shift-5 | Screenshot and recording toolbar |
| Control-Command-Shift-3 / 4 | Copy the screenshot to the clipboard |
| Control held when committing an area/window selection | Copy instead of saving |
| Control-Command-Escape | Stop recording |

Mac captures default to the XDG Desktop directory. Explicit
`OMARCHY_SCREENSHOT_DIR` and `OMARCHY_SCREENRECORD_DIR` overrides still apply.
Saving an image leaves the clipboard untouched. Clipboard-only capture uses a
private, temporary runtime file removed after publication and creates no image
in the user's capture directory. Mac capture does not automatically open a
viewer/editor. Legacy capture behavior remains unchanged in desktop mode.

Volume, brightness, keyboard-backlight, and media keys use replaceable
`[commands]` providers named `mac-volume-*`, `mac-brightness-*`,
`mac-keyboard-brightness-*`, `mac-play-pause`, and `mac-media-*`. Defaults use
`wpctl`, `brightnessctl`, and `playerctl`; standalone locking uses `swaylock`.
These programs and the corresponding hardware must be available. Media controls
remain usable while locked. No firmware Fn setting is changed.

## Validation and remaining parity

The [validation report](mac-mode-validation.md) records versions, the original
94-case run and final GPU-backed 144-case regression run, unit/lint/doc checks,
and the limits of that evidence.

Run the real client workflows in an isolated headless Weston session:

```sh
scripts/e2e.sh --headless --test mac_mode
scripts/e2e.sh --headless --test selection_transfer
scripts/e2e.sh --headless --test capture_tool
cargo test -p wm-core -p wm-config -p chonk-shell -p wm-wayland --lib
```

`mac_mode` injects physical seat events. Chromium DevTools observes only private
fixtures; it does not issue keyboard or clipboard commands. Tests cover browser
copy/cut/paste/undo/redo across Wayland and XWayland, terminal copy/paste and raw
Control, source quit, file copy through Nautilus, document exchange with Writer,
application switching/hide, Force Quit confirmation, Show Desktop, fullscreen desktops, capture and recording
destinations, invalid reload, navigation, resume, focus changes, and passthrough.
Additional input tests cover both Command keys, Caps Lock, overlapping physical
and translated navigation, mode rollback with a key held, and German/French
letters and shifted screenshot digits. This does not qualify every layout or
the firmware behavior of a physical Apple keyboard.
Rich-format persistence tests compare exact HTML, PNG, URI, and large binary
payloads after source closure. Optional applications are reported as skipped
when absent; CI treats missing required clients as failures.

The full plan still requires per-display Spaces and fullscreen combinations,
app-specific widget/pane adapters, Finder-specific operations such as
Command-Option-V move and Return-to-rename,
Spotlight-style system search, character/dictation/input-source services,
accessibility and menu-bar keyboard navigation, exact Option/dead-key maps for
all layouts, and standalone X11 delivery. Apple Fn/Globe behavior, multiple
physical keyboards, and Apple hardware/hotplug need physical qualification.
Those items are not represented as completed by the tests above.
