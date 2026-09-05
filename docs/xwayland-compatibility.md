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

Under the Wayland session, XWayland clients now get real EWMH from
chonkstep (`crates/wm-wayland/src/xewmh.rs`): the compositor opens its
own client connection to the XWayland display and publishes
`_NET_SUPPORTING_WM_CHECK`, `_NET_SUPPORTED`, the client list, active
window, desktops, per-window desktop, workarea (dock reservation
included) and `_NET_FRAME_EXTENTS` — verified live with `xprop`,
including the maximize/fullscreen/hidden state atoms changing as the
window manager acts. Inbound client messages are translated too: a
pager's `_NET_ACTIVE_WINDOW` and `_NET_CURRENT_DESKTOP` messages to
the XWayland root are routed into the same activation and
desktop-switch events every other input path queues (proven by an
x11rb pager round-tripping a workspace switch in the end-to-end
suite), so `wmctrl -l` reads and `wmctrl -a` / `wmctrl -s` command.

XWayland is supervised after it becomes ready. If its live XWM connection
closes, chonkstep removes every X11 window, frame, stacking slot, XSETTINGS
handle and EWMH connection from that generation, withdraws the dead `DISPLAY`
from future launches, and attempts one restart. A replacement that reaches
ready republishes all session settings; a second crash stands down instead of
looping. The nested end-to-end suite SIGKILLs the real Xwayland process,
asserts the old window leaves the compositor ledger, and maps a new window
through the replacement ([#58](https://github.com/iconidentify/chonkstep/issues/58)).

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
| Honours `_MOTIF_WM_HINTS` | **Asserted in CI** | **From the code, verified live once** (`backend_impl.rs` asks smithay's `X11Surface::is_decorated`, which parses the same five words - note its name reads "wants decorations" but it answers "is client-side decorated"; the sign was once inverted here, found and fixed when Spotify arrived frameless with `MWM_DECOR_ALL` set) | n/a |
| Publishes `_NET_FRAME_EXTENTS` | **Asserted in CI** — real geometry when framed, four zeros when not | **From the code, verified live once** — published via `xewmh.rs`, `xprop` shows real chrome values | n/a |
| xdg-decoration | n/a | n/a | Every negotiation concluded server-side (Hyprland's rule); KDE's `org_kde_kwin_server_decoration` also advertised, and a client-side *declaration* there is believed — see below |

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

**Native Wayland clients, the current rule** (`crates/wm-wayland/src/decoration.rs`,
whose module comment carries the measured per-client evidence). The
decision is made from what the client says on one of two protocols. Over
`zxdg_decoration_manager_v1` every negotiation is concluded server-side,
as Hyprland concludes it: a client that asked for client-side hears
`server_side` back and, by the protocol it chose, draws no titlebar of its
own — which is what frames Omarchy's `decorations = "None"` terminals.
Over KDE's `org_kde_kwin_server_decoration` — the only decoration
protocol GTK3 and GTK4 speak — a client-side declaration is believed,
which is what keeps a libadwaita headerbar from wearing a second titlebar;
a GTK4 client that binds the manager and creates nothing for a toplevel is
read the same way, because GTK4 only creates the object when it wants
*our* chrome. A client that binds neither protocol (SDL2, GLFW without
libdecor) is framed, deliberately against the xdg preamble's default,
because those clients are silent for the opposite reason to GTK's.
`[decorations] client_side` and `server_side` in the config correct either
direction. Earlier revisions of this section listed GTK4 double titlebars
as **Known wrong**; that history is under "GTK double titlebars" below.
Applications that route through XWayland are unaffected by any of it,
since they answer in Motif hints.

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

**The environment variables never could have fixed this, and the
launcher no longer sets them under the compositor.** For a Wayland
client they do nothing: verified under `WAYLAND_DEBUG`, a GTK client
launched with `GDK_SCALE=2` against an output advertising scale 1 makes
**no `wl_surface.set_buffer_scale` call at all**. On Wayland, GTK takes
its scale from the protocol. Worse, now that the outputs advertise the
scale, `QT_SCALE_FACTOR=2` would *multiply onto* the platform scale a Qt
client already read and draw it twice as large again — so on the Wayland
stack the launcher withholds every toolkit scale variable
(`crates/chonk-shell/src/shell.rs`, `launch_app`). The variables remain
load-bearing on the X11 session, where there is no output scale to
read.

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

A fractional session scale (1.5) reaches clients two ways. `wl_output.scale`
can only carry an integer, so it advertises the ceiling (2) — rounded
*up* on purpose, because a client on that fallback path then renders more
pixels than the output needs and is downscaled crisp, where the floor
would have it upscaled blurry (`state.rs`, `advertised_output_scale`). A
client that binds `wp_fractional_scale_v1` — implemented, along with
`wp_viewporter` (`state.rs`, `xdg.rs`) — is told the exact fraction
through `preferred_scale` and the ceiling never applies to it.

### XSETTINGS


The session runs an XSETTINGS manager (`crates/chonk-xsettings`), which owns
the `_XSETTINGS_S<screen>` selection and publishes `Xft/DPI`,
`Gdk/WindowScalingFactor`, `Gdk/UnscaledDPI` and `Gtk/CursorThemeSize` to
every X client at once. No theme or font name is published, deliberately:
chonkstep ships no GTK theme and no Xcursor theme, so naming one would make
every GTK client fail to find it and fall back — overriding whatever the user
set in their own `gtk-3.0/settings.ini`. The desktop says true things about
DPI and nothing about taste.

At a fractional scale the integer GDK window factor and unscaled text DPI
are a pair: at 1.5x the factor is 2 and the pre-scale DPI is 72, whose
product is the requested 144 DPI. GTK substitutes `Gdk/UnscaledDPI` for
its text DPI before applying the window factor, so leaving it fixed at 96
would silently turn 1.5x into 2x. Cursor size follows a different GTK path:
`Gtk/CursorThemeSize` is handed directly to Xcursor without the window
factor, and therefore remains the pre-multiplied physical-pixel size.

**This now works on both sessions, and the Wayland half required a
policy call worth recording.** XWayland claims `_XSETTINGS_S0` the
moment it starts and publishes an *empty* settings block — a
placeholder, not a manager, and its emptiness meant X11 toolkits under
the compositor got no DPI at all. The manager now classifies the
current owner before deciding: an owner whose property is absent or a
valid zero-settings block is a placeholder and is taken over
(ICCCM-correct, `SelectionClear` to the old owner); an owner publishing
even one real setting — a user's own `xsettingsd` — is refused exactly
as before. Both paths are pinned by live tests against a real X
server, and the takeover was verified against a live XWayland: the
session log shows the selection acquired and scale 2.0 published.

Where it does apply, it reaches clients this session did not launch — which
the per-child environment variables below can never do — and it is
republished when the UI scale changes, so a **running** GTK or Qt application
follows a live rescale instead of waiting to be restarted.

The compositor also merges `Xft.dpi` and `Xcursor.size` into the X root
`RESOURCE_MANAGER` as soon as XWayland is ready, and republishes them after a
live scale change. It replaces only those two resource names, preserving any
unrelated values a user installed with `xrdb -merge`. This complements
XSETTINGS for Java, Electron's X11 backend and direct Xcursor consumers; some
of those clients latch resources when they open the display and therefore need
to be restarted after a live scale change.

Applications the desktop launches itself are additionally told the scale
through environment variables, set on each child as the launcher spawns it
(`crates/chonk-shell/src/shell.rs`, `launch_app` and `launch_env`) — on the
X11 session only, for the reason given under "Native Wayland clients scale
from the outputs". `QT_SCALE_FACTOR` plus `QT_AUTO_SCREEN_SCALE_FACTOR=0`
for Qt, and Chromium-family binaries additionally get
`--force-device-scale-factor`, which is the switch Chromium actually
honours. Nothing sets `GDK_SCALE` or `GDK_DPI_SCALE` any more: GTK's scale
comes from XSETTINGS, and setting the variable as well would fix the window
scale and disable the `Gdk/UnscaledDPI` override, double-scaling the client
(`spawn.rs`, `gtk_qt_scale_env` and its test). `XCURSOR_SIZE` rides both
stacks, with a per-stack value: pre-multiplied by the scale for an X11
client, which has nothing to multiply by itself, and the unscaled base for
a Wayland client, which treats it as a logical size
(`startup.rs`, `xcursor_size_env`).

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

Under the Wayland session the compositor advertises the session scale on
its outputs and implements `wp_fractional_scale_v1` and `wp_viewporter`
(see "Native Wayland clients scale from the outputs"), so a native client
is scaled by the protocol and not by the variables above — which is why
the launcher withholds them there. Only the XWayland clients in that
session take the X11 path: XSETTINGS for the toolkits that read it, and
the environment for the rest.

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
| LibreOffice | XWayland or native GTK, its choice | Single titlebar — the Motif hint on XWayland, a KDE-protocol `Server` request natively | XSETTINGS (X11); the output scale (Wayland) | From the code; one titlebar verified live on Wayland |
| Microsoft Edge, Chrome, Chromium, Brave | **Native Wayland** under the compositor; X11 under the X11 session | Single titlebar | `--force-device-scale-factor` | The launcher pins the ozone platform; a browser started outside the launcher may pick the wrong one |
| Electron apps (Slack, VS Code, Discord) | Usually XWayland | Single titlebar if the app sets the Motif hint, which most Chromium-derived apps do | `Xft.dpi` from `RESOURCE_MANAGER` | The launcher's Chromium fixups match only known browser names, so the display-global resource is the fallback for other Electron binaries |
| JetBrains IDEs (IntelliJ, CLion) | XWayland | Framed by chonkstep — JBR does not set the Motif hint | `Xft.dpi` from `RESOURCE_MANAGER` | Java reads the resource when opening the display; restart after a live scale change. Unverified with a real IDE |
| GIMP | XWayland or native GTK | Framed; GIMP asks for server-side chrome | XSETTINGS (X11); the output scale (Wayland) | Multi-window mode leans on `_NET_WM_WINDOW_TYPE_UTILITY` and `_DIALOG`, both handled |
| Qt applications (VLC, Krita, qBittorrent) | Native Wayland or XWayland | Framed, single titlebar either way | `QT_SCALE_FACTOR` at launch (X11); the output scale (Wayland) | Qt defers to server-side decorations when offered, and the compositor always offers |
| Steam | XWayland | Mixed — Steam's own windows ask for various chrome | 1× unless Steam is told otherwise | Its Chromium-embedded UI, overlay and Big Picture mode are all unverified here |
| Wine / Proton applications | XWayland | Depends on winecfg's "allow the window manager to decorate the windows"; Wine sets the Motif hint when it decorates itself | Wine's own DPI setting | Fullscreen games depend on `_NET_WM_STATE_FULLSCREEN`, which works in the X11 session; see the state caveat below for the Wayland one |

Every row is **From the code** or weaker. None has been run.

## Known not to work

**Drag-and-drop across the X11/Wayland boundary.** Not implemented, deferred
deliberately. See above.

**Live UI scale changes.** Fixed in stages, and kept here because earlier
revisions listed it as not implemented. A config reload's new scale reaches
native Wayland clients through the re-advertised outputs and per-surface
preferences, and X11/XWayland GTK and Qt clients through XSETTINGS (both
above). What still does not follow is anything read once at launch: a
client's own pointer size (`XCURSOR_SIZE`), Chromium's
`--force-device-scale-factor` on the X11 session, and Java.

**EWMH control messages inside the Wayland session.** Fixed: a
pager's `_NET_CURRENT_DESKTOP` or `_NET_ACTIVE_WINDOW` client message
to the XWayland root is translated into the window manager's own
activation and desktop-switch events, covered by the end-to-end
suite. `wmctrl -l` lists, `wmctrl -a` activates, `wmctrl -s` switches
desks. This entry is kept because earlier revisions listed it as not
implemented.

**`_NET_WM_STATE` feedback.** Fixed, on both client kinds:
`publish_net_state` pushes maximize/fullscreen/hidden back to X11
surfaces through smithay's setters, and sets the matching
`xdg_toplevel` states for native Wayland clients — which also never
used to hear they were maximized. Shading remains unpublished on the
Wayland session (no protocol vocabulary for it) and is omitted from
`_NET_SUPPORTED` there accordingly.

**Client-initiated minimize.** Fixed on both client kinds. An X11
client's `WM_CHANGE_STATE` request now enters the same miniaturize and
restore paths as the compositor's titlebar and shell icon tile. The
round trip publishes `_NET_WM_STATE_HIDDEN` plus ICCCM `WM_STATE`
(`IconicState` while hidden, `NormalState` after restore), so toolkits
that draw their own minimize button see the state they requested.

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

**GTK double titlebars.** Fixed, and not the way this document
previously said; this entry is history, kept so the reasoning is not
re-derived. The claim here used to be that a toplevel which never
negotiates xdg-decoration is client-decorated by the protocol's default
rule and so goes unframed — which is what the specification says, and
which is the wrong reading for GTK, because GTK never binds
xdg-decoration *at all*. It implements only KDE's older
`org_kde_kwin_server_decoration`, so a compositor advertising just the
standard interface hears silence from every GTK application on the
system and cannot tell a libadwaita headerbar from an SDL2 window that
draws nothing.

The desktop now advertises that second protocol with `default_mode =
Server`, as KWin, Sway, labwc and Hyprland all do. GTK reads it through
`gdk_wayland_display_prefers_ssd()`, and the split falls where it
should: a GTK application with no header bar of its own (LibreOffice)
stops drawing a titlebar and takes ours, while one whose header bar is
part of its interface (Nautilus, anything libadwaita) keeps it and goes
unframed. See `crates/wm-wayland/src/decoration.rs`.

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
