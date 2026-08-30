# X11 and XWayland application compatibility

What to expect when you run the applications you actually use — LibreOffice,
a browser, an IDE, a game — on this desktop. It is written for someone
deciding whether chonkstep is usable for their work, which means its only
value is that it can be trusted. A matrix of green ticks nobody checked is
worse than no matrix, so every claim below carries a status, and "I have not
run this" is one of the statuses.

## How to read this

Four status words are used, and they mean exactly this:

**Asserted in CI** — a machine checks it on every push, from outside the
window manager, against a real X server. If it regresses, the build goes red.
This is the only status that is evidence rather than argument.

**From the code** — the implementation says it behaves this way, and the
citation is given so you can check the reasoning yourself. Nobody has
launched the named application against a chonkstep session and looked.
Treat it as a prediction with a source, not a test result.

**Not implemented** — there is no code path. Not "partly works", not "works
by accident": the protocol handler does not exist, and the behaviour it would
produce is simply absent.

**Known wrong** — code exists, runs, and produces the wrong result. These are
written down deliberately rather than omitted, because the ones a project
hides are the ones that waste an evaluator's afternoon.

Nothing in this document was verified by launching the named applications.
The claims come from reading the two backends and the shell's launcher, plus
one mechanical check in CI that covers the decoration policy and nothing
else. If you run one of these and it behaves differently, this file is the
bug — fix it here first.

## Which stack an application ends up on

This is the first thing to understand, because almost every difference below
follows from it. chonkstep ships as two binaries and the same application can
take two different paths through them.

**The X11 session** (`scripts/xsession.sh`) runs `chonkstep` as an ordinary
X11 window manager. Every application is an X11 client, there is no XWayland
involved at all, and the WM implements a real EWMH surface — `_NET_SUPPORTED`,
`_NET_CLIENT_LIST`, `_NET_ACTIVE_WINDOW`, the `_NET_WM_STATE` family, the
`_NET_WM_WINDOW_TYPE` family, `_NET_WORKAREA`, `_NET_FRAME_EXTENTS`
(`crates/wm-x11/src/backend.rs`). `wmctrl`, `xdotool`, pagers and taskbars
work here because there is something for them to talk to.

**The Wayland session** (`scripts/wayland-session.sh`) runs
`chonkstep-wayland` as a compositor and starts XWayland rootlessly for X11
clients (`crates/wm-wayland/src/state.rs`, `crates/wm-wayland/src/xwayland.rs`).
An application here is either a native Wayland client or an XWayland client,
and it chooses, not you — except where the desktop's launcher chooses for it.
Chromium-family binaries are pinned to the ozone platform of the session they
were launched from, so under the compositor Edge, Chrome, Chromium and Brave
run *natively on Wayland*, not through XWayland
(`crates/chonk-shell/src/spawn.rs`, `chromium_platform_args`). Everything
else takes its toolkit's default: GTK and Qt applications prefer Wayland when
`WAYLAND_DISPLAY` is set, while Wine, Steam, JetBrains' JBR and most Electron
builds land on XWayland.

The consequence worth stating plainly: **under the Wayland session, XWayland
clients get no EWMH root properties from chonkstep.** The `publish_*` family
is left at its no-op defaults there, deliberately —
`crates/wm-wayland/src/backend_impl.rs` says so directly — so an XWayland
client sees only the minimal `_NET_SUPPORTED`/`_NET_SUPPORTING_WM_CHECK` that
smithay's own `X11Wm` publishes. X11 automation tools do not work inside that
session, and neither does `_NET_FRAME_EXTENTS`.

## Decorations

The rule is one sentence: a client that says it draws its own titlebar is
fully managed but never framed. It is focused, moved, stacked, put on a
workspace and closed exactly like anything else; it just does not get
chonkstep's chrome drawn around chrome it already drew. An X11 client says so
through `_MOTIF_WM_HINTS` — the decorations bit set in `flags`, the
`decorations` field zero — which is the dialect LibreOffice, Microsoft Edge,
GTK, Qt, Chromium and Electron all speak.

Silence means "decorate me". Every way of failing to get an answer — property
absent, unreadable, wrong format, truncated, flag not set — converges on
framing the window, because an ordinary X11 client has relied on being
decorated for forty years and "we could not tell" must not un-frame it
(`crates/wm-x11/src/backend.rs`, `client_draws_own_chrome`).

Clients are allowed to change their mind. Edge in particular rewrites
`_MOTIF_WM_HINTS` mid-session, so both backends watch for the property change
and re-decide rather than reading once at map time.

| | X11 session | Wayland session, XWayland client | Wayland session, native client |
|---|---|---|---|
| Honours `_MOTIF_WM_HINTS` | **Asserted in CI** | **From the code** (`backend_impl.rs` asks smithay's `X11Surface::is_decorated`, which parses the same five words) | n/a |
| Publishes `_NET_FRAME_EXTENTS` | **Asserted in CI** — real geometry when framed, four zeros when not | **Not implemented** — no EWMH is published to XWayland | n/a |
| xdg-decoration | n/a | n/a | Server-side forced on every toplevel |

The CI assertion lives in the `test` job of `.github/workflows/ci.yml`: it
boots the release binary as the window manager of an Xvfb display, launches an
ordinary `xterm` and a purpose-built client that sets the Motif hint before its
first map, and checks from outside with `xprop` that the first has a non-zero
top frame edge, that the second is in `_NET_CLIENT_LIST` with
`_NET_FRAME_EXTENTS` of `0, 0, 0, 0`, and that `_NET_FRAME_EXTENTS` is
advertised in `_NET_SUPPORTED`. Being in the client list is half the point:
"do not decorate this" must never be read as "ignore this".

Be precise about what that job proves for XWayland. The decision itself lives
in `wm-core`, above both backends, and an XWayland client reaches the same
shared policy — so the job genuinely covers the *policy*. It does not cover
the XWayland *plumbing*: the compositor's surface handling, frame geometry and
property routing are not exercised, and cannot be, because a headless CI
runner cannot boot a compositor at all (the nested backend needs a host
display and the session backend needs a DRM device and a seat).

**Known wrong, native Wayland only:** a toolkit that draws its own titlebar
and never binds `zxdg_decoration_manager_v1` — GTK4/libadwaita above all —
gets framed anyway and wears two titlebars. This is a deliberate trade rather
than an oversight, and the long argument is at `client_draws_own_chrome` in
`crates/wm-wayland/src/backend_impl.rs`: treating a client's *silence* as
"I decorate myself" would also un-frame every SDL2 or GLFW window built
without libdecor, leaving a rectangle that cannot be moved, resized or closed
with the pointer. Applications that route through XWayland are unaffected,
since they answer in Motif hints instead of by staying quiet.

## UI scale and HiDPI

### Native Wayland clients scale from the outputs

The outputs advertise the session's UI scale, rounded to a whole number
(`wl_output.scale` carries only integers — `state.rs`,
`advertised_output_scale`), every surface is told it entered them and,
for clients binding `wl_surface` v6, told the number outright through
`preferred_buffer_scale` (`xdg.rs`, the commit handler). A GTK client
answers with `set_buffer_scale(2)` and renders itself at 2x; verified
under `WAYLAND_DEBUG` with the whole loop closed — `wl_output.scale(2)`
→ `wl_surface.enter` → `preferred_buffer_scale(2)` →
`set_buffer_scale(2)` — and visually, a zenity dialog at exactly twice
its 1x size next to pixel-identical chrome. A live scale change
(config reload) re-advertises the outputs and re-sends the per-surface
preference, and an already-mapped GTK dialog follows it both ways.

**The environment variables never could have fixed this.** The launcher
puts `GDK_SCALE` and `QT_SCALE_FACTOR` in every child's environment, and
for a Wayland client they do nothing: verified under `WAYLAND_DEBUG`, a
GTK client launched with `GDK_SCALE=2` against an output advertising
scale 1 makes **no `wl_surface.set_buffer_scale` call at all**. On
Wayland, GTK takes its scale from the protocol. The variables remain
load-bearing for X11 and XWayland clients, which have no output scale
to read.

Advertising the scale was tried once before and broke the desktop badly
— the dock vanished off the right edge, the wallpaper was clipped to a
quarter. The conclusion drawn at the time, that a physical-pixel
compositor needs a full logical/physical split first, was **wrong**.
The mechanism was narrower: smithay's damage trackers were reading
their render scale from the outputs, and smithay sizes an element at
its *logical* extent times that scale while leaving its physical
position alone — so chrome the theme had already rasterized in device
pixels was multiplied a second time. Element positions were never the
problem.

The shipped design splits exactly along that line
(`state.rs::physical_damage_tracker` carries the full account):

- the advertisement is protocol metadata only — every damage tracker
  and `DrmCompositor` in the crate is pinned to scale 1, so the
  compositor composes in physical pixels end to end and chrome needs no
  conversion at all (and no `size / scale` integer division that would
  shave a pixel off odd-sized chrome);
- each client surface tree is wrapped in a `RescaleRenderElement` by
  its own committed buffer scale (`renderer.rs::push_surface_tree`),
  putting its buffer back at 1 buffer pixel : 1 screen pixel — the size
  the ledger recorded for it. A surface that ignores the advertisement
  (Xwayland's always do) commits at scale 1 and the wrap is the
  identity;
- values crossing the ledger boundary convert by each surface's own
  committed factor, never a session-wide one — the measure
  (`xdg.rs::committed_content_size`), the configure
  (`backend_impl.rs::resize_client`, where a session-wide factor makes
  the round trip feed itself and a scaled window grow without bound),
  and the input hit walk (`input.rs`);
- screencopy converts the protocol's "output logical coordinates" back
  into scene pixels (`protocols.rs::output_geometry`) — dividing the
  mode by the advertised scale there is how the first working build
  handed `grim` a quarter-size capture.

A fractional session scale (1.5) advertises the nearest whole step (2):
clients render crisp and somewhat larger than the chrome, and the
ledger reflows their frames around what they actually commit. The exact
fraction has no channel short of implementing `fractional-scale-v1`.

### XSETTINGS


The session runs an XSETTINGS manager (`crates/chonk-xsettings`), which owns
the `_XSETTINGS_S<screen>` selection and publishes `Xft/DPI`,
`Gdk/WindowScalingFactor`, `Gdk/UnscaledDPI` and `Gtk/CursorThemeSize` to
every X client at once. No theme or font name is published, deliberately:
chonkstep ships no GTK theme and no Xcursor theme, so naming one would make
every GTK client fail to find it and fall back — overriding whatever the user
set in their own `gtk-3.0/settings.ini`. The desktop says true things about
DPI and nothing about taste.

**This works on the X11 session and is currently inert under the Wayland
compositor. Verified, both halves.** On a plain X server chonkstep acquires
the selection and the published bytes decode to the right values for the
session's scale. Under the compositor it does not: XWayland claims
`_XSETTINGS_S0` for itself the moment it starts, and publishes an *empty*
settings block (a bare 12-byte header, zero settings). Standing down is the
correct response to an existing owner — two managers fighting is worse than
either winning, and ICCCM says so — but the practical result is that X11
applications under the Wayland session still get no DPI from this mechanism,
and fall back to the per-child environment variables described below. Taking
the selection away from XWayland by force is possible and is not currently
done; it needs deciding, not just coding.

Where it does apply, it reaches clients this session did not launch — which
the per-child environment variables below can never do — and it is
republished when the UI scale changes, so a **running** GTK or Qt application
follows a live rescale instead of waiting to be restarted. Nothing writes
`Xft.dpi` into the X `RESOURCE_MANAGER`; there is no `xrdb` call anywhere in
the tree, and XSETTINGS is the mechanism toolkits prefer anyway.

Applications the desktop launches itself are additionally told the scale
through environment variables, set on each child as the launcher spawns it
(`crates/chonk-shell/src/shell.rs`, `launch_app`).
`GDK_SCALE` and `GDK_DPI_SCALE` carry the integer and fractional halves for
GTK, `QT_SCALE_FACTOR` plus `QT_AUTO_SCREEN_SCALE_FACTOR=0` for Qt,
`XCURSOR_SIZE` (24 × scale) for pointers, and Chromium-family binaries
additionally get `--force-device-scale-factor`, which is the switch Chromium
actually honours.

One limit follows from the environment-variable half, and one from the
toolkits themselves. Both are real, and both are narrower than they were
before XSETTINGS existed.

**Toolkit scale is live; pointer size is not.** A running GTK or Qt
application follows a mid-session scale change, because XSETTINGS bumps its
serial and rewrites the property and the toolkits watch it. What does not
follow is the *pointer*: a client that draws its own cursor read
`XCURSOR_SIZE` once, when it started, and there is no protocol for telling it
otherwise — `crates/chonk-shell/src/startup.rs` says as much. `Gtk/CursorThemeSize`
is published live for the toolkits that consult it, but an application using
Xcursor directly keeps the pointer size it launched with. Anything launched
*after* the change gets the new size, since the value is put in each child's
own environment rather than the session's.

**A toolkit that ignores XSETTINGS is only reached at launch.** Chromium is
the case that matters: it does not read XSETTINGS for scale, which is why the
launcher passes `--force-device-scale-factor` explicitly. A Chromium-family
browser therefore picks the scale up when it starts and not afterwards. The
same applies to anything else that consults neither XSETTINGS nor the
environment.

Under the Wayland session the compositor advertises its outputs at scale 1
and implements neither `wp_fractional_scale_v1` nor `wp_viewporter`, so no
client's buffers are scaled by the compositor. Every scaling decision is the
application's, made from the variables above.

## Clipboard

**Landing in this same change** — this is new work, not a long-standing
guarantee, and it deserves your scepticism until you have used it for a week.

Copy and paste bridges both directions between X11 and Wayland clients, for
both the CLIPBOARD selection and the X11 PRIMARY selection (middle-click
paste). The compositor mirrors a Wayland client's selection onto XWayland's
proxy window and serves the bytes back when an X11 client pastes, and answers
the mirror image when the owner is an X11 client
(`crates/wm-wayland/src/xdg.rs` and `crates/wm-wayland/src/xwayland.rs`).
Clipboard access follows keyboard focus, which is what stops a paste
targeting whichever client happened to focus first.

Under the X11 session the question does not arise: every client is an X11
client talking to the same X server, and selections are between them.

Status: **From the code**, both sessions. Large transfers, unusual MIME types
and image paste between an X11 and a Wayland application are exactly the cases
a first implementation gets wrong, and none of them have been exercised.

## Drag and drop

**X11 ↔ Wayland drag-and-drop is not implemented at all.** This is a
deliberate deferral, not a gap someone missed.

Concretely: there is no Xdnd implementation anywhere in the tree — no
`XdndAware`, no Xdnd client messages, nothing that could act as the X11 half
of a bridge — so dragging a file from an X11 file manager into a Wayland
application, or the reverse, does nothing. Dragging between two X11
applications works, because that is the X server's own business and chonkstep
is not in the path. Dragging between two native Wayland clients works through
smithay's grab machinery, with one visible defect: **the drag icon is not
drawn**, so the pointer carries nothing visible while the drag is in flight
(`crates/wm-wayland/src/xdg.rs` — the icon surface arrives and is currently
unused).

## Cursors

Under the X11 session the window manager rasterises its own pointer set —
arrow and the four resize edges — at the session scale and installs them on
the root window and on each frame, because the X core cursor font does not
scale and looked visibly wrong next to properly themed application pointers.
Applications keep their own cursors; the WM only owns the root and the frames.

Under the Wayland session the compositor draws the pointer itself. An X11
client's cursor arrives through XWayland as an ordinary `wl_pointer.set_cursor`
surface and is composited, so X11 applications do get their own shapes. Their
*size* comes from `XCURSOR_SIZE` in the client's environment, with the launch-
time-only caveat from the scaling section. There is no cursor *theme* loading
at all: the compositor's own arrow is hand-authored rather than read from an
Xcursor theme, so it will not match a theme you have configured elsewhere.

## Per application

Transport is what the application uses under the **Wayland** session; under
the X11 session everything is an X11 client.

| Application | Transport | Decorations | Scale | Notes |
|---|---|---|---|---|
| LibreOffice | XWayland or native GTK, its choice | Single titlebar — it sets the Motif hint | `GDK_SCALE`/`GDK_DPI_SCALE` at launch | From the code |
| Microsoft Edge, Chrome, Chromium, Brave | **Native Wayland** under the compositor; X11 under the X11 session | Single titlebar | `--force-device-scale-factor` | The launcher pins the ozone platform; a browser started outside the launcher may pick the wrong one |
| Electron apps (Slack, VS Code, Discord) | Usually XWayland | Single titlebar if the app sets the Motif hint, which most Chromium-derived apps do | **Likely 1×** | The launcher's Chromium fixups match on the binary name (`chrom*`, `microsoft-edge`, `brave*`), so an Electron app gets neither the ozone pin nor the scale flag, and Chromium does not read `GDK_SCALE` reliably |
| JetBrains IDEs (IntelliJ, CLion) | XWayland | Framed by chonkstep — JBR does not set the Motif hint | **Likely 1×** | Java's HiDPI detection wants `Xft.dpi` in `RESOURCE_MANAGER`, which nothing here writes. Expect to set `-Dsun.java2d.uiScale` yourself. Unverified |
| GIMP | XWayland or native GTK | Framed; GIMP asks for server-side chrome | `GDK_SCALE` at launch | Multi-window mode leans on `_NET_WM_WINDOW_TYPE_UTILITY` and `_DIALOG`, both handled |
| Qt applications (VLC, Krita, qBittorrent) | Native Wayland or XWayland | Framed, single titlebar either way | `QT_SCALE_FACTOR` at launch | Qt defers to server-side decorations when offered, and the compositor always offers |
| Steam | XWayland | Mixed — Steam's own windows ask for various chrome | 1× unless Steam is told otherwise | Its Chromium-embedded UI, overlay and Big Picture mode are all unverified here |
| Wine / Proton applications | XWayland | Depends on winecfg's "allow the window manager to decorate the windows"; Wine sets the Motif hint when it decorates itself | Wine's own DPI setting | Fullscreen games depend on `_NET_WM_STATE_FULLSCREEN`, which works in the X11 session; see the state caveat below for the Wayland one |

Every row is **From the code** or weaker. None has been run.

## Known not to work

**Drag-and-drop across the X11/Wayland boundary.** Not implemented, deferred
deliberately. See above.

**Live UI scale changes.** Applications read the scale once, at launch. Not
implemented, deferred deliberately.

**EWMH inside the Wayland session.** `wmctrl`, `xdotool`, X11 pagers and
taskbars have nothing to talk to, because chonkstep publishes no root
properties to XWayland. Native Wayland tooling uses
`wlr-foreign-toplevel-management` instead, which X11 clients cannot see.

**`_NET_WM_STATE` feedback to XWayland clients.** The compositor maximises and
fullscreens an X11 window but never tells it: `X11Surface::set_maximized`,
`set_fullscreen` and `set_minimized` are never called, so the client's
`_NET_WM_STATE` goes stale. An application that draws differently when it
believes itself maximised will draw the wrong thing.

**Client-initiated resize.** `_NET_WM_MOVERESIZE`'s eight *resize* directions
are dropped: this window manager's resize machinery is driven by its own
resizebar geometry and has no edge to take from a client that has none. An
application's own "drag this border to resize" affordance therefore does
nothing, and such a window is resized by its edges under chonkstep's chrome —
or, if it draws its own chrome, not by the pointer at all.

Client-initiated *moves* are implemented on both backends, and have to be: a
window whose client draws its own titlebar has no chrome of ours to drag, so
its own titlebar is the only handle it has. Both stacks translate the request
into the same interactive move the window manager runs for its own titlebar
drags, edge snapping included.

**GTK4/libadwaita double titlebars, native Wayland only.** Known wrong, with
the reasoning above.

**Drag icons.** Not drawn during a native Wayland drag.

**Input methods.** No `text-input`/`input-method` protocols and nothing sets
`XMODIFIERS`, so ibus and fcitx do not work for any client. CJK and compose-
key input beyond what xkb itself provides is unavailable.

**Window icons.** `_NET_WM_ICON` and `WM_HINTS` icon pixmaps are not read;
the miniaturise and switcher previews use live window captures instead.

**Client struts.** `_NET_WM_STRUT_PARTIAL` is not read, so a third-party panel
or dock will be overlapped rather than reserved around. The workarea comes
from chonkstep's own dock geometry.

**X11 screen capture under the Wayland session.** XWayland is rootless and
nothing composites the scene into its root window, so `scrot` and
`import -window root` capture nothing. Use the compositor's own
`wlr-screencopy` path.

**Startup notification.** `DESKTOP_STARTUP_ID` is neither set nor consumed, so
there is no launch feedback and no focus-stealing arbitration based on it.

## Verifying this yourself

The decoration policy is the one thing with a mechanical check. Run the
`test` job's smoke test locally, or reproduce its core by hand under any X
session chonkstep is managing:

```sh
# Which clients are managed, and how much chrome each one has.
xprop -root _NET_CLIENT_LIST
xprop -id <client-id> _MOTIF_WM_HINTS _NET_FRAME_EXTENTS
```

A framed window reports a non-zero third value (the top edge); a client-
decorated one reports `0, 0, 0, 0` and is still in the client list.
`scripts/verify-ewmh.sh` covers the rest of the X11 session's EWMH surface.

For everything above marked **From the code**: the useful contribution is not
another paragraph of reasoning, it is launching the application and changing
the status.
