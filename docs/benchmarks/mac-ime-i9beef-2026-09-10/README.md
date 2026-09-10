# i9beef Mac-mode UAT and input-method regression

Enabling Mac mode on the qualified NVIDIA build exposed a real shortcut failure:
with Fcitx's Wayland keyboard grab active, GTK received Super-A/C/V instead of
Control-A/C/V. Command-Tab worked, and bypassing the input method made editing
work, isolating the problem to delivery through the input-method grab.

The compositor previously projected modifiers only at `KeyboardFocus`, which
an input-method grab consumes events before reaching. The patch forwards the
projection through that grab without changing physical XKB state. The input
method receives the projected mask before the translated key, and the physical
mask before the next untranslated key. Fcitx remains enabled.

The real GTK/Fcitx fixture checks exact Unicode clipboard contents across two
apps and after the source exits, Command-Tab/Command-Q, physical Control-A,
Command released before its letter, ordinary shifted typing, and an untranslated
Command-/ immediately after translated Command-A while Command stays held.
It requires a real protocol keyboard grab; testing only direct key delivery
would miss the original failure. A short delay lets asynchronous text-input
focus/grab replacement settle before each chord; this test does not establish
arbitrary-speed input delivery during grab replacement.

The installed baseline fails the same fixture at Command-A. All 288 compositor
unit tests pass (three separate ignored cases), and strict Clippy passes for
`wm-wayland` and `chonk-testkit`, including test targets. The first debug Mac
suite passed 11 cases and timed out in the existing Nautilus file-copy case;
that case then passed in isolation on both the baseline and patched binaries.
The clean release then passed all 12 Mac-mode cases together and all seven
keyboard-focus cases. After installation and restart, eight native uinput smoke
checks passed on i9beef with Fcitx enabled and no GTK input-method override.
The running binary matches the tested release SHA-256. The monitor reports
3840×2160 at 144 Hz, normal scanout is allowed, experimental overlay/primary-any
options are off, and the sampled pipeline reports zero render/queue failures.
Terminal and Steam were reopened. See `validation.json` for exact evidence.

The shared release cache produced an LTO bitcode parsing error. The deployment
build uses a fresh `target/mac-uat-clean` directory and the ordinary release
profile, including thin LTO. No optimization setting was weakened.

User configuration enables `interaction_mode = "mac"`, separate display Spaces,
and clipboard persistence, and removes the starter Alt shortcut overrides that
conflicted with Option editing. The Apple keyboard uses an explicit US layout;
this also avoids unresolved Lua variable names in the Omarchy compatibility
parser. Prior configuration and the prior installed binary are retained locally.
The existing unsupported Hyprland compatibility diagnostics are unchanged.

This is the implemented Mac profile described in [mac-mode.md](../../mac-mode.md),
not a claim of complete macOS parity. Native multi-monitor hotplug and other
machines were not retested for this keyboard-only deployment.
