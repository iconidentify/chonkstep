**ChonkStep Mac mode — implementation plan**

> Implementation status: [docs/mac-mode.md](mac-mode.md) records the implemented profile, conformance tests, and outstanding parity work. The roadmap below is not a claim that every milestone is complete.

Draft for implementation, 2026-09-09. Repository inspected at `c7dad60` (0.4.4).
This document proposes behavior and work; it does not enable the mode.

Build a complete Mac interaction mode for ChonkStep running **Linux on Apple
hardware**, covering application commands, clipboard interchange, text editing,
application/window management, Spaces, capture, keyboard navigation, and device
behavior. Make consistency across applications a release criterion.

Use macOS Tahoe 26 documentation as of this date as the reference, with versioned
entries for newer Fn/Globe commands and older hardware. Apple documents contextual
differences between applications; the compatibility specification must record
context as well as the keys. [Apple keyboard guidance](https://developer.apple.com/design/human-interface-guidelines/keyboards/)

**Product contract and activation**

Proposed single user-facing switch:

```toml
interaction_mode = "mac"
```

The switch activates the entire behavior profile. Appearance and desktop-shell
selection remain independent: it must work with `desktop = "omarchy"` and the
standalone ChonkStep shell. Offer the same switch in settings, with a searchable
shortcut reference, conflict report, per-app overrides, and restore-defaults.
Apple hardware detection can suggest the mode; activation is an explicit choice.

Use these implementation rules:

- Preserve physical Command, Control, Option, Shift, and Fn/Globe as distinct
  inputs. Both Command keys work. Control remains available to terminals and
  accessibility tools.
- Resolve Mac defaults as a complete profile. Existing Omarchy bindings such as
  Super+L, Super+T, Super+arrows, Super+Tab, and Super+Shift+digits must not steal
  Mac application, text-navigation, switching, or screenshot commands.
- Continue importing Omarchy's relevant services and non-shortcut configuration.
  Filter its bindings and binding flags through explicit profile ownership.
  Preserve commands needed by the shell even when their old shortcuts disappear.
- Explicit ChonkStep overrides win and mark the profile as customized. A
  contradictory explicit legacy `keymap` setting produces a useful diagnostic;
  it must not silently produce half of each mode.
- Mode changes validate first and commit at a neutral input boundary. A failed
  reload retains the working profile. App configuration changes that need an app
  restart are shown as pending; the UI must not claim they already took effect.
- Turning the mode off restores prior configuration, removing only
  ChonkStep-managed app settings. Preserve edits made by the user in the meantime.
- Ship native Wayland **and XWayland applications** in the first release gate.
  The standalone X11 session has its own implementation and validation milestone;
  show its support status accurately until that gate passes.

The compatibility promise is tied to observable behavior and tested app versions.
Every standard below needs an owner, implementation status, and test. Proprietary
Apple services and unsupported hardware are explicit entries, so “Mac mode” does
not silently turn their keys into unrelated Linux actions.

**What already exists, and what needs to change**

| Area | Inspected foundation | Required work |
| --- | --- | --- |
| Configuration | [preset.rs](../crates/wm-config/src/preset.rs), [lib.rs](../crates/wm-config/src/lib.rs): two keymaps, overrides, import precedence, provenance | Add the interaction profile, typed command registry, context/profile resolution, and conflict diagnostics. Generalize the current explicit-ChonkStep-keymap preservation around live imports. |
| Keyboard model | [types.rs](../crates/wm-core/src/types.rs): four modifiers and one keysym per combo | Add semantic modifier names, device-aware Fn capability, sequence/state handling, and layout-aware matching. |
| Wayland routing | [input.rs](../crates/wm-wayland/src/input.rs), [keyboard.rs](../crates/wm-wayland/src/input/keyboard.rs): physical delivery, suppressed releases, repeat, atomic XKB updates | Add application command dispatch/translation without breaking focus ownership, grabs, IMEs, or held-key accounting. |
| Switching | [manager.rs](../crates/wm-core/src/manager.rs): modal Alt-Tab over windows, commits on Alt release | Make switching configurable and add a distinct application MRU model plus within-app window cycling. |
| Application identity | [xdg.rs](../crates/wm-wayland/src/xdg.rs): app identity cached in backend, can change after mapping | Expose stable application identity and lifecycle to core; reconcile late identity changes and transient windows. |
| Clipboard | [selection.rs](../crates/wm-wayland/src/selection.rs), [xwayland.rs](../crates/wm-wayland/src/xwayland.rs), [data_control.rs](../crates/wm-wayland/src/data_control.rs): bridge, separate PRIMARY, both data-control interfaces | Qualify rich formats, file operations, persistence, manager coexistence, and actual shortcut-triggered copies. |
| Capture | [capture.md](capture.md), [capture_tool](../crates/wm-wayland/src/capture_tool): screen/area/window/recording, worker, PNG clipboard | Separate file and clipboard destinations, add Mac save/review behavior and complete selector semantics. Current screenshots save **and** copy, then open a viewer. |
| Spaces and Overview | [gestures.md](gestures.md), [overview.rs](../crates/chonk-shell/src/overview.rs): live previews, swipe transitions, moves/deletion | Add Mac boundary policy, app-only view, Space creation/reordering, per-display policy, and fullscreen Spaces. |
| Shell actions | [shell.rs](../crates/chonk-shell/src/shell.rs): action dispatch and keyboard-modal menus | Implement Hide, Hide Others, Show Desktop, app Quit/Force Quit UI, launcher/character services, and keyboard focus for Dock/status surfaces. |
| Standalone X11 | [backend.rs](../crates/wm-x11/src/backend.rs): passive key grabs and keyboard grab | Implement and qualify an X11 routing path; Wayland interception code cannot simply be reused. |
| Tests | [chonk-testkit](../crates/chonk-testkit), [e2e.sh](../scripts/e2e.sh) | Extend real input/protocol tests and add app-level conformance. Existing bridge tests already exercise binary data, UTF-8, INCR, backpressure, and XWayland restarts. |

These are inspection findings, not a claim that the existing suites were run for
this planning task.

**Command inventory**

Notation: Cmd = Command/⌘, Opt = Option/⌥, Ctrl = Control/⌃. “App” means
the active application's command layer. “Desktop” means ChonkStep or its shell
service. “Context” means the same chord has different valid meanings in different
apps or controls. Grouped rows expand into individual registry entries and tests.

The registry must eventually contain: stable action ID, chord/sequence, keyboard
layout rule, context, owner, default-enabled state, minimum reference macOS
version, device capability, adapter, repeat behavior, source, and test IDs.
Generate the displayed shortcut card from this registry.

**Everyday application commands**

| Keys | Target behavior | Owner |
| --- | --- | --- |
| Cmd+C / X / V | Copy / cut / paste | App |
| Cmd+Z / Shift+Cmd+Z | Undo / redo | App |
| Cmd+A | Select all | App |
| Opt+Shift+Cmd+V | Paste matching destination style | App |
| Cmd+N / O / S / P | New / open / save / print | App |
| Cmd+W | Close current tab, document, or window according to context | App |
| Cmd+Q | Graceful application quit | App |
| Cmd+T | New tab; context-specific command in some editors | Context |
| Cmd+, | Application settings | App |
| Cmd+F / G / Shift+Cmd+G | Find / next / previous | App |
| Escape / Cmd+. | Cancel current operation where supported | Context |
| Cmd+? | Help/menu help | App |
| Shift+Cmd+S / Opt+Shift+Cmd+S | Duplicate/Save As according to application model | Context |

Reference: [Apple's introductory shortcut table](https://support.apple.com/guide/mac-help/intro-to-mac-keyboard-shortcuts-mchld6b9e240/mac)
and [keyboard design standards](https://developer.apple.com/design/human-interface-guidelines/keyboards/).
For editable content, keyboard, menu, and context-menu copying must invoke the
same application operation. [Apple copy/paste workflow](https://support.apple.com/guide/mac-help/copy-and-paste-on-mac-mchl5252f3de/mac)

Do not bind Cmd+W globally to the existing `close` action: that can close a whole
browser window when the user intended to close one tab. Do not turn Cmd+Q into a
process kill. In an app lacking a graceful Quit interface, document the missing
adapter and implement it before claiming Quit parity for that app.

**Text navigation and editing**

| Keys | Text-control behavior |
| --- | --- |
| Cmd+Left / Right | Line start / end |
| Cmd+Up / Down | Text-area/document start / end |
| Opt+Left / Right | Word movement |
| Opt+Up / Down | Paragraph movement |
| Shift added to movement | Extend selection |
| Delete / Fn+Delete | Backward / forward deletion |
| Opt+Delete / Opt+Fn+Delete | Delete preceding / following word |
| Fn+Up / Down | Page scrolling |
| Fn+Left / Right | Beginning/end scrolling; preserve caret |
| Ctrl+A / E / B / F / P / N | Line and character movement |
| Ctrl+H / D / K / Y / T / O / L | Backspace, forward-delete, kill/yank, transpose, newline, recenter; widget-specific |
| Cmd+B / I / U | Bold / italic / underline |
| Opt+Cmd+C / V | Copy / paste style |
| Cmd+; / Cmd+: | Spelling operations |
| Ctrl+Cmd+D | Dictionary lookup where provided |

Reference: [Pages navigation/editing commands](https://support.apple.com/en-ie/guide/pages/tanc0ffef022/mac)
and [Mac text-editing conventions](https://support.apple.com/en-us/102650).
Also inventory Cmd+Delete (line-prefix deletion), alignment, font-size, font
panel, inspector, completion, and selected-text search commands as contextual
editor capabilities; verify exact behavior against the chosen reference app.

Text conformance must include wrapped lines, bidirectional text, grapheme
clusters, selection anchors, and empty/read-only/password fields. A synthesized
Home key is insufficient evidence of correct visual-line navigation. The
Ctrl+K/Y kill buffer must remain distinct from the system clipboard. Raw Control
sequences in terminals retain their terminal meanings.

**Applications, windows, and Spaces**

| Keys | Behavior | Owner |
| --- | --- | --- |
| Cmd+Tab / Shift+Cmd+Tab | Application MRU forward / backward | Desktop |
| Cmd+grave / Shift+Cmd+grave | Current application's next / previous window | Desktop |
| Cmd+H / Opt+Cmd+H | Hide application / other applications | Desktop |
| Cmd+M / Opt+Cmd+M | Minimize one / all app windows | Desktop |
| Opt+Cmd+W | Close app windows according to app model | App |
| Ctrl+Cmd+F | Toggle fullscreen | Coordinated app/desktop |
| Ctrl+Up / Mission Control key | Overview | Desktop |
| Ctrl+Down | Application windows view | Desktop |
| Ctrl+Left / Right | Previous / next Space | Desktop |
| Cmd+Mission Control / configured Show Desktop key | Reveal desktop, restore on repeat | Desktop |
| Ctrl+number | Direct Space selection, configurable | Desktop |

Reference: [Apple window behavior](https://support.apple.com/en-euro/guide/mac-help/mchlp2469/26/mac/26),
[app/window shortcuts](https://support.apple.com/en-us/102650),
[Mission Control](https://support.apple.com/en-au/guide/mac-help/mchlb7beb9af/mac),
and [Spaces](https://support.apple.com/guide/mac-help/work-in-multiple-spaces-mh14112/mac).
Direct numbered Space bindings require an explicit enabled-state entry; verify
their defaults on the baseline Mac. The grave-key equivalent is layout-dependent.

Application switching requires a new application model. Track desktop-file ID,
Wayland app_id/X11 identity, transient relationships, launched instances, and
the most recently used eligible window. Do not group by window title or assume
one process is one application. Browser apps, separate profiles, sandbox IDs,
late app_id changes, and applications with no visible windows need explicit
identity rules. Use app identity for routing, never as a security credential.

Hold Command to cycle; commit when the final held Command key is released;
Escape restores the prior state. Add the reference switcher's H/Q and arrow
interactions only with explicit state-machine tests. Within-app window cycling
is a separate operation. Hidden, minimized, fullscreen, and windowless
applications need baseline-Mac observations, recorded as fixtures.

Hide must preserve minimized state, geometry, workspace, and stacking. Show
Desktop needs a reversible visibility snapshot. Neither operation should be
implemented by indiscriminately minimizing windows. Window closure or app
creation while hidden must not resurrect stale windows during restore.

For Spaces, extend the current workspace implementation with:

- Explicit creation, deletion, reordering, and stable IDs; deleting a Space
  preserves its windows on a neighboring desktop. Add creation controls to
  Overview and keyboard-accessible equivalents.
- Mac-mode left/right navigation stops at existing boundaries. Disable implicit
  creation by repeated next-Space keys or swipes in this profile. Existing
  ChonkStep growth semantics remain available outside this profile.
- Fullscreen and paired Split View Spaces with remembered return location and
  geometry. A borderless window on an ordinary workspace alone does not satisfy
  this requirement.
- Per-display Spaces with an explicit linked-displays alternative. Define
  active-display selection, dock/menu placement, cross-display moves, and
  unplug/replug/lid-close behavior before changing core membership.
- App assignment to one/all desktops, switching to a Space containing the
  activated app, and an option for recent-use reordering. Record reference
  defaults; exercise both settings states.
- Window dragging through Overview and across desktop edges. Optional keyboard
  “send/carry window” shortcuts are labeled ChonkStep extensions, not invented
  Mac defaults. No Cmd+number or Cmd+Shift+number workspace bindings.
- Migrate session state and all consumers of workspace indexes: Hyprland IPC,
  ext-workspace, X11/EWMH, window rules, shell thumbnails, and restore. Preserve
  stable membership when display order or visible numbering changes.

The Spaces behaviors above use [Apple's Spaces model](https://support.apple.com/guide/mac-help/work-in-multiple-spaces-mh14112/mac)
as the reference; implementation choices for Linux identity and protocol
projection are ChonkStep design work.

**Screenshots and recording**

| Keys / interaction | Mac-mode outcome |
| --- | --- |
| Shift+Cmd+3 | Entire-screen capture to file |
| Shift+Cmd+4 | Immediate region selector |
| Shift+Cmd+4, then Space | Window/menu selector |
| Ctrl added to 3/4 capture | Clipboard destination |
| Shift+Cmd+5 | Screenshot/recording toolbar |
| Escape | Cancel without output |
| Space while dragging region | Move selection |
| Opt while selecting a window | Omit shadow |

Reference: [Apple screenshots](https://support.apple.com/en-us/102646).
Add timed capture, destination choice, pointer inclusion, and optional thumbnail
review to the toolbar. Distinguish destination from selection type in typed
capture requests. File-only capture leaves the clipboard unchanged;
clipboard-only capture creates no screenshot file and launches no viewer.
Mac defaults use the localized Desktop destination. Explicit user destinations
win, and the UI explains any existing destination override.

Measure baseline behavior for multiple outputs, menu capture, Ctrl pressed during
selection, Shift/Option region adjustment, and full-screen capture file splitting.
Test 1x/1.5x/2x, different display scales, negative output coordinates, and color
conversion. Preserve device-pixel resolution and bound capture memory.

Recording uses Ctrl+Cmd+Escape to stop. Add destination, timer, microphone
selection/permission, click indication, and failure recovery. Current silent
recording is a foundation; microphone support is a required service task for
recording parity. Tahoe's selected-window recording needs its own capability
entry and implementation. Keep recording state separate from screenshot
destination controls. [Apple recording reference](https://support.apple.com/en-us/102618)
Inventory Touch Bar capture separately; it is hardware-dependent. HDR capture
is capability-gated and must not relabel an SDR PNG as HDR.

**File manager and file dialogs**

| Keys | File context |
| --- | --- |
| Cmd+C / Cmd+V / Opt+Cmd+V | Copy / paste / move copied items |
| Return | Rename selected item |
| Cmd+Down / Cmd+Up | Open / parent folder |
| Space / Cmd+Y | Quick Look-style preview |
| Shift+Cmd+N | New folder |
| Cmd+Delete / Shift+Cmd+Delete | Trash / empty Trash |
| Cmd+I / D / E / K | Information / duplicate / eject / connect |
| Cmd+[ / ] | Back / forward |
| Shift+Cmd+G | Go to folder |
| Cmd+1 / 2 / 3 / 4 | File view modes |

Reference: [Apple Finder commands](https://support.apple.com/en-us/102650) and
[rename behavior](https://support.apple.com/en-ie/guide/mac-help/mchlp1144/mac).
Also inventory standard folder destinations, hidden files, preview/sidebar/path/
status/toolbar toggles, sorting/grouping, aliases, and Open/Save dialog navigation.

The file-manager adapter needs semantic actions for rename, preview, duplicate,
and “move copied files.” A global Return-to-F2 mapping would damage filename
editing and dialogs. Cmd+X in editable text remains Cut; Finder-style file moves
must not require that chord. Move is a file operation with completion/error
handling, not a clipboard rewrite followed by blind deletion. Test cross-volume
moves, conflicts, Unicode paths, multiple items, inaccessible sources, and
cancelled operations. Reuse file-manager APIs where available; add missing app
integration explicitly. Scope Open/Save dialog mappings by widget context.

**Browsers, editors, terminals, and pointer modifiers**

Browser profile: Cmd+L for address; Cmd+[ / ] for history; Ctrl+Tab and
Ctrl+Shift+Tab for tabs; Shift+Cmd+[ / ] as tab alternatives; Cmd+1–8 and Cmd+9
for tab selection; Shift+Cmd+T to reopen; Cmd+plus/minus to zoom; Cmd-click and
Shift+Cmd-click for opening links in tabs. Reference:
[Safari conventions](https://support.apple.com/guide/safari/keyboard-shortcuts-and-gestures-cpsh003/mac).
Also qualify reload/hard reload, zoom reset, new/private windows, downloads,
bookmarks, developer tools, and focus inside web-based editors against each
browser's macOS version. Safari-only features become explicit capability entries.

Editor profiles must retain multi-key sequences and context predicates. VS Code
needs tests in the editor, search, command palette, notebook, webview, and
integrated terminal. Matching only its top-level app ID cannot distinguish those
contexts; use editor keybinding configuration for those cases.

Terminal profile: Cmd+C/V must call the terminal's own selection-copy and paste
operations. Ctrl+C/Z/D and other raw Control sequences continue to reach the
PTY. Copy without a selection must not interrupt a process. Paste preserves
bracketed-paste handling and multiline policies. Option-as-Meta is an explicit
terminal setting, alongside Option text composition in ordinary applications.
Qualify tabs, font sizing, scrollback, shell word movement, tmux, and SSH.
[Apple Terminal reference](https://support.apple.com/guide/terminal/keyboard-shortcuts-trmlshtcts/mac)

Prefer terminal-native bindings. A validated terminal-specific Ctrl+Shift+C/V
translation is a fallback, never the generic Cmd-to-Ctrl rule. Legacy xterm-like
clients need their own supported translation/configuration.

Pointer behavior is part of this mode: Cmd-click selection, Shift-click ranges,
Ctrl-click context menus, Option-drag copy, and app-specific link modifiers.
Use context-specific adapters; do not transform every Ctrl-click. Preserve
pointer constraints and drag-and-drop action negotiation.
[Apple copy/context menus](https://support.apple.com/guide/mac-help/copy-and-paste-on-mac-mchl5252f3de/mac),
[Option-drag](https://support.apple.com/guide/mac-help/drag-and-drop-items-mh35852/mac).

**Desktop utilities, accessibility, and hardware coverage**

| Family | Registry requirements |
| --- | --- |
| Search | Cmd+Space launcher/search; Opt+Cmd+Space file search. Scope and search providers must match the advertised capability. |
| Characters/input | Ctrl+Cmd+Space character picker; Ctrl+Space / Ctrl+Opt+Space previous/next input source; compose/dead keys and IME selection. Fn/Globe alternatives depend on device support. |
| Session | Ctrl+Cmd+Q lock; Shift+Cmd+Q logout; Force Quit on Opt+Cmd+Escape. Inventory immediate-logout and power/eject variants with their reference confirmation/hold semantics. |
| Media | Brightness, keyboard illumination, volume/mute, playback; Option opens related settings, Option+Shift fine adjustment where supported. |
| Shell focus | Ctrl+F2…F8 family for menu, Dock, windows, toolbar, panels, navigation policy, and status controls; appropriate Fn variants. |
| Accessibility | Cmd+F5 screen reader; Opt+Cmd+F5 accessibility panel; zoom shortcuts, contrast/invert, spoken selection, Full Keyboard Access, Sticky/Slow Keys and Mouse Keys. |
| Modern Fn/Globe | Inventory Dock, desktop, notifications, control center, dictation, character picker, app browser, Quick Note, and window-tiling commands by macOS version. |

References: [Apple system shortcuts](https://support.apple.com/en-us/102650),
[keyboard focus standards](https://developer.apple.com/design/human-interface-guidelines/keyboards/),
[accessibility panel](https://support.apple.com/guide/mac-help/quickly-turn-accessibility-features-on-or-off-mchlp2975/mac),
[screen-reader commands](https://support.apple.com/guide/voiceover/general-commands-cpvokys01/10/mac/26),
and [screen zoom](https://support.apple.com/en-gb/guide/mac-help/mchl779716b8/mac).
Input and launcher references:
[input-source switching](https://support.apple.com/guide/mac-help/write-in-another-language-on-mac-mchlp1406/mac),
[character picker](https://support.apple.com/en-euro/guide/mac-help/mchlp1560/mac),
[Spotlight](https://support.apple.com/guide/mac-help/search-with-spotlight-mchlp1008/mac),
and [keyboard settings](https://support.apple.com/guide/mac-help/keyboard-settings-kbdm162/mac).

For keyboard-only use, implement ordinary Tab/Shift+Tab navigation, arrows,
Space activation, Return/default-button activation, and Escape cancellation
through every owned surface. Add a Full Keyboard Access mode with visible focus,
navigation into and out of controls, and a command reference. ChonkStep's current
pointer-only Dock needs actual focus/navigation support; rebinding a launch menu
does not implement Dock focus. [Apple Full Keyboard Access](https://support.apple.com/en-au/guide/mac-help/mchlc06d1059/mac)

Implement these through capability-bearing Linux services. “Screen reader” needs
working accessible shell controls and an AT-SPI/reader integration; launching a
process alone is insufficient. Preserve its modifier chords during translation.
Services that are unavailable must appear as unavailable in settings, with an
explicit provider option when one exists. Apple-specific Siri, iCloud, AirDrop,
Universal Clipboard, Apple Intelligence, and Touch ID authentication are separate
platform integrations, not outcomes created by assigning a shortcut.

For modern window tiling, include Fn+Ctrl+arrows (halves), Fn+Ctrl+F (fill),
Fn+Ctrl+C (center), Fn+Ctrl+R (restore), Shift variants (paired arrangement), and
Option+Shift variants (one half plus quarters). Keep fill distinct from
fullscreen. Some arrangements have menu actions without default shortcuts.
[Apple tiling table](https://support.apple.com/guide/mac-help/mac-window-tiling-icons-keyboard-shortcuts-mchl9674d0b0/mac)

Hardware qualification covers built-in Apple Silicon and Intel keyboards,
USB/Bluetooth Magic Keyboards, ANSI/ISO/JIS layouts, laptop Delete/navigation
keys, and mixed Apple/PC devices. Add device-specific modifier configuration;
avoid machine-wide swaps. Test US, British, German, French, Japanese, and a
non-Latin input source, including punctuation and shifted-digit shortcuts.

Fn must be a **discovered capability**. Linux's Apple HID driver has its own
Fn translation and modifier-swap options; SPI hardware has a separate path.
Inspect the events reaching libinput/XKB for each model, including kernel swaps
already configured, before designing the compositor representation. Some
combinations may arrive as already-translated keys rather than an observable
Fn modifier. Document the path and any unavailable combinations.
[Linux Apple HID driver](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-apple.c),
[Apple SPI keyboard driver](https://github.com/torvalds/linux/blob/master/drivers/input/keyboard/applespi.c).

Retain the existing interactive swipe implementation and add app-exposé,
Show Desktop, and user-selectable gesture configuration where input supports
them. Check natural scrolling, secondary click, pinch, and page navigation
without stealing application gestures. Missing raw gesture support becomes a
hardware/input milestone. [Apple gesture reference](https://support.apple.com/en-us/102482)

**Architecture: route commands deliberately**

Create a backend-neutral command resolver in or alongside `wm-core`, consumed
by both input backends and shell settings. Its output should distinguish:

| Resolution | Meaning |
| --- | --- |
| Desktop action | Perform one compositor/shell action and consume matching input. |
| Native app command | The app/profile owns this chord and receives the appropriate original input or supported semantic action. |
| Translated app chord | Deliver a validated alternate chord to the same focused client. |
| Raw input | Preserve the input stream for ordinary typing, grabs, or a passthrough profile. |
| Unsupported capability | Report the missing integration through settings/diagnostics; never run an unrelated fallback. |

GTK separates widget key bindings from application accelerators; Qt standard
keys depend on platform, and Electron supports explicit menu roles. Those are
different integration surfaces. Provide tested adapters rather than assuming
that changing one environment variable changes every Linux app.
[GTK input handling](https://docs.gtk.org/gtk4/input-handling.html),
[Qt standard keys](https://doc.qt.io/qt-6/qkeysequence.html),
[Electron menu roles](https://www.electronjs.org/docs/latest/tutorial/menus).

Prefer native application binding/action configuration where supported, including
correct menu labels and tooltip hints. Use an allowlisted chord translator for
known mappings that cannot be supplied natively. Match profiles by desktop ID,
Wayland app_id or X11 class/instance, executable identity where available, and
explicit user rules. Do not infer widget context from window titles.

Keep a conservative generic profile for ordinary GUI editing, with configurable
app exceptions and a visible compatibility report. Known terminals, VMs, remote
desktops, and raw-input applications require dedicated profiles. An unknown app
cannot be certified merely because one text field accepted Ctrl+V. App-internal
context distinctions require native configuration or app/toolkit work.

Preserve ChonkStep's existing input ownership order. Seat/VT handling and session
lock outrank ordinary desktop actions. Shortcut inhibition and XWayland keyboard
grabs select passthrough behavior. Exclusive shell layers, input methods, modal
capture/Overview, and focused app surfaces must each have one current owner.
Adapt shell text fields too, using shell/provider cooperation where necessary.

Translation requires separate physical and client-visible state. Track each
press through repeat and release, its target surface, profile generation,
replacement chord, and modifier ownership. Restore logical modifiers on focus
changes, device removal, lock, VT switch, suspend, reload, and disappearing
clients. Never leave a synthetic Control held, replay a release without its
press, or reroute a held key to a newly focused app. Preserve lock and latched
modifiers and the active layout group. A double-Command/Fn gesture recognizer
must not delay normal typing or trigger while shortcuts are being composed.

Implement translated delivery inside the appropriate backend, without
re-entering global shortcut matching. Do not use per-keystroke subprocesses or
machine-global synthetic input. Standalone X11 needs a separately proven
grab/replay strategy and native app bindings; evaluate XKB/XInput/XTest constraints
in its milestone before promising identical event behavior.

The hot path uses precompiled tables, cached app resolution, bounded state, and
no synchronous IPC or filesystem access. Diagnostics report action IDs, routing,
capabilities, and conflicts; production logs contain no typed text or clipboard
payloads. Display Cmd/Opt symbols and names consistently in owned UI. Menu label
parity in unmodifiable third-party apps is tracked as an app integration gap.

**Clipboard reliability workstream**

Separate three questions in every test: did Copy execute, did the payload survive
and transfer, and did Paste invoke the destination's intended operation?

1. Keep CLIPBOARD authoritative for Cmd+C/V. PRIMARY/middle-click selection stays
   separate; selecting unrelated text must not overwrite an explicit copy.
   Do not enable clipboard/PRIMARY synchronization as part of Mac mode.
2. Preserve offered formats: UTF-8/plain text, HTML, RTF when offered, PNG and
   other supported image formats, URI lists, file operation metadata, and
   application-private formats. Negotiate the richest mutually supported type;
   fallback to plain text only when appropriate. Keep binary bytes intact.
3. Give clipboard persistence one owner per seat. Inventory the current Omarchy
   service first. Reuse it if it passes the required format/lifetime tests;
   otherwise supply a supervised ChonkStep persistence service and coordinate
   ownership so the two cannot continuously replace each other's offers.
4. Capture the current offer eagerly in an asynchronous worker/service. Preserve
   supported representations after source exit without blocking the compositor.
   Use bounded memory and private temporary storage for larger content; define
   size/time limits and show failure rather than truncating silently. History
   across copies/reboots is separate from retaining the current copy.
5. Associate transfers with source generations. Replacing a selection cancels
   stale cache work; a late transfer must never replace a newer copy. Bound
   concurrent MIME requests and slow consumers. Test manager restart, explicit
   clear, empty payloads, source exit mid-transfer, and XWayland restart.
6. Treat file lists as references and preserve supported move/copy intent. Data
   files are transferred by the file manager. Test file-manager-to-app paste,
   attachment fields, and sandbox/document-portal access; a readable URI outside
   a sandbox is not proof that the destination can use it.
7. Preserve password-manager clear requests and sensitive-content hints when
   available. Avoid persisting secrets into history. Clipboard managers retain
   the existing data-control security boundary; ordinary apps use focused
   selection access. Lock/logout behavior must be explicitly tested.
8. Qualify Wayland→Wayland, Wayland→XWayland, XWayland→Wayland, and
   XWayland→XWayland, including isolated/sandboxed apps. Standalone X11 gets the
   same external behavior suite through its selection manager.

A source that disappears before providing its bytes cannot be reconstructed.
The service must distinguish complete retained copies from interrupted offers
and preserve the last completed item according to a documented policy. Make the
ordinary copy-then-quit workflow pass repeatedly, and expose interrupted-copy
failure instead of reporting successful persistence. Native app cooperation may
be needed for stronger completion semantics.

**Implementation milestones and completion gates**

| Milestone | Work and primary code area | Completion evidence |
| --- | --- | --- |
| M0 — Freeze behavior | Expand every inventory family into registry rows. Observe a reference Mac for defaults and unresolved contexts. Capture device event capabilities. Record baseline tests. | No unowned standard; each row is a default, configurable option, extension, or unavailable capability. Specific unsupported features are enumerated. |
| M1 — Profile and resolver | `wm-config`, command resolver, identity plumbing, settings/provenance. Generalize import preservation. Add preview/rollback. | One switch resolves consistently at startup/reload; Omarchy conflicts cannot leak; legacy mode fixtures remain unchanged. |
| M2 — Command delivery | Wayland event state, native app profiles, terminal handling, context-sensitive close/quit, menu labels. | Real Cmd+C/V/Z/W and physical Ctrl+C work in browser/editor/terminal; focus changes and both Command keys cannot stick or double-deliver input. |
| M3 — Clipboard contract | Existing bridge plus qualified persistence service, formats, sandbox/file integrations. | Every required app pair and payload passes; copy-quit-paste, rapid replacement, large/slow transfers, and XWayland loss are accounted for. |
| M4 — App/window model | `wm-core`, shell switcher, Hide/Show Desktop, graceful Quit/Force Quit. | Multi-window/multi-process/hidden/minimized apps switch and restore correctly; closing a tab never closes unrelated tabs. |
| M5 — Spaces and gestures | Workspace model, fullscreen/Split View, per-display behavior, Overview, restore/IPC migrations. | All Space commands and supported gestures pass through display hotplug, fullscreen lifecycle, app activation, and session restore. |
| M6 — Capture and services | Typed capture destination, thumbnail UI, recording controls/audio, search/characters, media, accessibility providers, file-manager actions. | Capture destinations are exact; cancel is side-effect-free; all advertised utilities have working providers and keyboard-accessible UI. |
| M7 — Standalone X11 | Backend delivery, passive grabs, app profiles, persistence and capture provider. | Same applicable conformance suite passes in the standalone X11 session; remaining backend differences are explicit. |
| M8 — Hardware and release | Apple device matrix, full app matrix, international layouts, long-run lifecycle checks, docs/package integration. | Release report lists hardware/app versions and results; no required skipped tests or known data-loss/misrouting failures. |

M0 precedes M1; M1 precedes M2. M2 and M3 jointly deliver the first usable
clipboard milestone. M4 enables M5. M6 shares M1/M3 foundations. M7 follows the
proven resolver contract. M8 integrates the advertised scope. These are
independently reviewable changes; no “complete Mac mode” label at M1 or M2.

The first implementation slice should be **M0/M1 plus a narrowly tested M2/M3
path**: toggle mode, copy browser text with Cmd+C, paste into a terminal with
Cmd+V, interrupt a command with Ctrl+C, copy terminal selection back into the
browser, then repeat with one side on XWayland and after quitting the source.
This proves the hardest cross-layer contract before expanding the inventory.

**Compatibility and verification matrix**

Initial app set, subject to adding the user's daily apps:

| Category | Required coverage |
| --- | --- |
| Browsers | Chromium/Chrome and Firefox; native Wayland and XWayland where available; editable web fields and rich web editors |
| Electron/editor | VS Code editor, integrated terminal, dialogs, command palette, webviews; another Electron app to catch app-specific assumptions |
| Terminals | Alacritty, foot, kitty or Ghostty, and xterm; shell, tmux, SSH, raw/fullscreen programs |
| GTK | GTK3 and GTK4 text controls; Nautilus; a real document/image application |
| Qt | Qt Widgets and Qt Quick controls; representative document/file app |
| Office and images | LibreOffice Writer/Calc and an image editor; formatted text, tables, PNG/alpha, file/attachment workflows |
| Shell/services | Launcher, search, notifications, character picker, capture, file dialogs, lock screen, accessibility tools |
| Isolation/raw input | Flatpak/document portal, password manager, VM/remote desktop and shortcut inhibitors |

For clipboard transport, run all applicable ordered source/destination pairs
within this release set. Use explicit payload capabilities: a terminal is tested
for text and path insertion, not arbitrary rich-image pasting. Each pair includes
Unicode, multiline text, repeated operations, source closure, and ownership
replacement. Add HTML/RTF/images/files to compatible pairs. Use deterministic
payload hashes plus destination-content assertions; a shortcut event in a log
alone does not prove successful copying.

Extend existing `selection_transfer`, `selection_buffer`, `selection_lifecycle`,
`keyboard_focus`, `keyboard_repeat`, `browser_selection`, `xwayland_input`,
`overview`, `desktop_gestures`, `capture_tool`, `session_lock`, and workspace
test targets. Introduce dedicated Mac routing/application conformance tests.
Run through `scripts/e2e.sh --headless --release --test <target>` where applicable;
unit tests alone and virtual-keyboard injection alone cannot qualify real devices.

Mandatory adversarial sequences: release Command before/after the ordinary key;
hold both Commands; switch focus while held; dismiss a menu during a chord;
open/close an IME; unplug a keyboard; suspend/resume; lock/unlock; switch VT;
change layout; reload the mode; kill the destination; inhibit shortcuts; cancel
capture; replace a clipboard offer while a slow consumer is reading. Assert
balanced key states, one action per press, no hidden typing, and bounded memory.

Run real Apple hardware checks on at least an Apple Silicon laptop, an Intel
Mac, and USB/Bluetooth external Apple keyboards. Record kernel, input driver,
layout, desktop provider, app versions, display topology, and scaling. Nested
tests cannot certify Fn/Globe, firmware translations, Bluetooth reconnect,
physical gestures, or display hotplug by themselves.

Measure input dispatch against the existing build: no per-key allocation or
blocking IPC on the added hot path; stable repeat under clipboard/capture load;
no growing retained state after thousands of copy/switch/reload cycles. Keep
latency distributions and resource measurements in the release evidence rather
than inventing an unmeasured performance guarantee.

Mac mode is ready when the required registry rows, app-pair tests, device checks,
and rollback tests pass, documentation matches the active bindings, and every
remaining optional/platform capability has an accurate visible status.
