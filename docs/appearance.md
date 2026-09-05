# Light and dark: the appearance axis

ChonkStep has two independent axes of taste. The **theme** decides
which desktop you have -- Amber Phosphor, Teal Blueprint, Ivory
Halftone -- and the **appearance** decides which of that theme's two
renditions you are looking at: `light` or `dark`. Every built-in theme
ships both, designed as a pair rather than derived from each other:
the chrome geometry, the theme's identity and its wallpaper
composition are shared; the fills, the chiseled bevel ramps, the menu
palette, the full terminal color scheme and the wallpaper artwork's
mood are each drawn twice. Dark is not an inversion of light -- the
focused titlebar stays ink on both sides (that is what keeps keyboard
focus legible on a pale desk), selection highlights use each theme's
accent instead of inverting to white, and the ANSI palettes are
re-derived per side so terminal output holds contrast on its own
ground.

Switching appearance re-resolves the current theme in its other
rendition through the same live-apply path a theme pick takes: no
restart, nothing closed, one repaint.

## Switching

- **At runtime** (the normal way): write `light`, `dark`, or `toggle`
  to the request file described below. Anything can write it -- a
  dockapp, a keybinding script, `echo toggle > .../appearance-request`.
- **In the config**: `appearance = "dark"` (or `"light"`) in
  `~/.config/chonkstep/config.toml` sets the mode for a session that
  has never chosen one. Because the running desktop persists its mode
  (see the state file below), the config line only decides the very
  first session of a state directory; after that, the request file is
  the way to move.
- **By default**: with nothing said anywhere, the selected theme's own
  native mood applies -- dark for seven of the eight built-ins, light
  for Ivory Halftone -- so upgrading into this feature changes nothing
  on screen.

A theme pick keeps the current appearance; an appearance switch keeps
the current theme. The two axes never reach across each other -- with
one exception, below.

## Following Omarchy

`theme = "omarchy"` (or the `Omarchy (...)` row in the Theme
submenu) is not a ninth theme but an instruction: wear whatever
[Omarchy](https://omarchy.org) is wearing. The desktop reads the palette
`omarchy-theme-set` leaves at
`~/.local/state/omarchy/current/theme/colors.toml` (`$XDG_STATE_HOME`
honoured), maps its named colours onto chonkstep's chrome, and keeps
chonkstep's geometry: the 23 px titlebar, the bevels, the dock tile,
the fonts. For a dark palette the focused titlebar is its
`darker_background` with `bright_foreground` as ink, the unfocused bar
its `muted`, menus on `lighter_background`, the highlight in its
`accent`. A light palette swaps roles the way the built-in light theme
does, so focus stays legible on a pale desk: the focused titlebar
becomes the palette's `foreground` (ink) with `background` as its
text, the unfocused bar its `selection`, and the borders `foreground`.
In both moods every ink is chosen from two palette candidates by
contrast against the fill it lands on, the highlight text included,
and the terminal palette is laid out slot for slot as Omarchy's own
alacritty template lays it (`crates/wm-theme/src/omarchy.rs` carries
the full table). The theme is named after Omarchy's:
`Omarchy (Tokyo Night)`.

The session then watches that file about once a second and re-dresses
when it changes, so `omarchy-theme-set catppuccin-latte` -- or a pick
from Omarchy's own theme menu -- restyles this desk too, live, along
with every dockapp. Applications launched under the follow (a foot, a
`chonk_ui` app reading `CHONKSTEP_THEME=omarchy`) read the same file
and wear the same palette; as with any theme pick, a terminal already
open keeps the palette it was launched with.

**Here the appearance axis defers.** An Omarchy palette has one mood,
its `mode` (`light` or `dark`, inferred from the background when the
file omits it), and that mood *is* the session's appearance: it is
published to the `appearance` file like any other, so GTK and portal
applications follow a light Omarchy theme as a light desk. While
following, `appearance = ...` in the config and any
`appearance-request` are consumed and declined, with a line in the
session log saying why -- the way to change mood is to change the
Omarchy theme. Picking a built-in from the Theme submenu ends the
follow and hands the axis back.

With Omarchy not installed, or its palette missing or unparsable, the
flagship stands in and the log says so once; the choice to follow
stands, and the watch picks the palette up the moment one appears.
The Theme submenu only offers the Omarchy row when there is a
palette to follow.

**The wallpaper is Omarchy's own.** An Omarchy theme is a palette and a
set of backgrounds, and `omarchy-theme-set` -- and `omarchy-theme-bg-next`,
the background-cycling key -- leave `current/background` pointing at the
one in use. The follow theme names that link as its wallpaper: picked
(or booted into with nothing else persisted) the desk shows the picture
behind it, in whatever format Omarchy ships it (webp, jpg, png, gif,
bmp), and the same one-hertz watch that notices a palette swap notices
the link moving, so cycling backgrounds in Omarchy cycles them here. A
follow with no background set wears Graphite Fold, the neutral artwork.
The row also stands on its own: the Wallpaper submenu offers `Omarchy's
Background` whenever Omarchy has a current theme to read (a readable
`current/theme/colors.toml` -- the same test that admits the Omarchy
row to the Theme submenu), so a built-in theme can wear Omarchy's
picture too.

The bridge runs the other way too, and on a machine with Omarchy it is
the *only* theme list there is: chonkstep offers no theme menu of its
own when Omarchy's is readable, because two lists that disagree about
which themes exist is the split system this desktop is trying not to
have. `omarchy-export-themes` writes each built-in theme as an Omarchy
theme -- `colors.toml` derived from the theme itself, its wallpaper
under `backgrounds/`, and a `preview.png` for Omarchy's picker -- into
`~/.config/omarchy/themes/` (or a directory you name), after which
`omarchy-theme-set amber-phosphor` dresses the rest of the machine to
match. `scripts/install.sh` runs it once so the themes are in the
picker from the first login.

The preview is *rendered*, not photographed: the theme's background
with two of its own window frames on it, drawn through the same
`ThemeEngine` the compositor decorates live windows with. A screenshot
would be truer, and needs a running compositor on a real display --
which an exporter that has to work over ssh, in a package build and in
CI does not have. It is built with the shell (`cargo build -p chonk-shell`) and
lives at `target/release/omarchy-export-themes` in a checkout, at
`~/.local/bin/omarchy-export-themes` after `scripts/install.sh`, and at
`/usr/bin/omarchy-export-themes` from the Arch package; the whole
invocation is

```sh
omarchy-export-themes                 # into ~/.config/omarchy/themes/
omarchy-export-themes /some/dir       # or wherever you say
```

The export is generated, never hand-edited: change the built-in and
run it again.

## The file contract (public -- dockapps build on this)

Both files live in the session state directory:
`$XDG_STATE_HOME/chonkstep/` (`~/.local/state/chonkstep/` when the
variable is unset).

| File | Direction | Content |
| --- | --- | --- |
| `appearance` | published by the shell | the literal string `light` or `dark` (no newline requirement) |
| `appearance-request` | written by anyone, consumed by the shell | `light`, `dark`, or `toggle` |

`appearance` is rewritten atomically (write-to-temp, rename), so a
reader polling it can never observe a torn or empty value; it is
written at session startup and again on every switch. It doubles as
the persisted choice the next session starts from.

`appearance-request` is consumed the way the `reload`/`restart`
markers are: the shell polls once per housekeeping tick (~16 ms),
reads the file, deletes it, then acts -- so a request is honored
exactly once. Values are trimmed and case-insensitive; an unparsable
request is consumed and warned about in the session log, never
guessed at. A request naming the mode the session is already in is
consumed and does nothing.

## How applications follow

On every switch the shell fans the new mode out through every channel
it owns, in this order (all within one tick; none of the later steps
gate the desktop's own repaint):

1. **The desktop itself** -- window chrome, menus, dock, Clip,
   launcher, icon tiles and the wallpaper (each artwork has a
   rendition per mood) re-resolve through the live theme-apply path.
2. **Dockapps** -- the `ThemeChanged` broadcast pushes the full
   resolved palette (now carrying an `appearance` tag in its
   `theme_toml`) down every dockapp socket; freshly launched dockapps
   and SDK apps additionally get `CHONKSTEP_APPEARANCE` beside
   `CHONKSTEP_THEME` in their environment.
3. **Terminals** -- see the next section.
4. **GSettings / the portal** -- the shell runs
   `gsettings set org.gnome.desktop.interface color-scheme
   prefer-dark|prefer-light`. xdg-desktop-portal-gtk republishes that
   as the `org.freedesktop.appearance color-scheme` setting
   (0 default / 1 dark / 2 light) on `org.freedesktop.portal.Settings`,
   which is what GTK4/libadwaita applications, Electron apps and most
   modern toolkits watch. If the `gsettings` binary or the schema is
   missing, this degrades with one log line and everything else still
   switches. Additionally, when the current `gtk-theme` value is a
   member of an installed light/dark pair this desktop knows
   (Adwaita/Adwaita-dark, or adw-gtk3/adw-gtk3-dark), it is flipped to
   the matching member so GTK3 applications follow too; a hand-picked
   third-party theme is never overwritten.
5. **XSETTINGS (X11/XWayland clients)** -- on the X11 session the
   binary republishes `Net/ThemeName`/`Gtk/ThemeName` with the
   matching member of that same installed pair. If no known pair is
   fully installed (both members present with real `gtk-3.0`/`gtk-4.0`
   payloads), no name is published at all -- naming a missing theme
   would make GTK clients fall back to their default while overriding
   the user's own settings, which is worse than saying nothing.

### What follows live, honestly

| Surface | On a switch |
| --- | --- |
| Window chrome, menus, dock, wallpaper, Overview | live, same tick |
| Dockapps (SDK or protocol) | live, next servicing pass (&le;16 ms) |
| Terminals the desktop spawned | live (foot color-theme signal, see below) |
| Terminals opened by other means | **not live** -- they keep their colors |
| GTK4 / libadwaita / Electron (portal watchers) | live, via GSettings &rarr; portal |
| GTK3 apps (Wayland) | live only if `gtk-theme` is a managed pair member |
| GTK2/3 X11/XWayland apps | live via XSETTINGS on the X11 session; on the Wayland session this requires the compositor's XSETTINGS publisher to carry the theme name (not yet wired) |
| Qt applications | **next launch at best**: Qt reads `QT_QPA_PLATFORMTHEME`/`QT_STYLE_OVERRIDE` and its platform theme at startup; this desktop does not force them (see below) |
| Apps that only read colors at startup | next launch |

Qt is documented rather than forced on purpose: `QT_QPA_PLATFORMTHEME`
is a launch-time environment variable, so no running Qt app can be
retinted by the desktop, and pinning the variable for future launches
would override whatever the user's own environment (qt5ct/qt6ct,
Plasma leftovers) already configured. A user who wants Qt to track the
portal can set `QT_QPA_PLATFORMTHEME=xdgdesktopportal` in their own
environment; Qt apps launched after that follow the same portal
setting as GTK.

## Terminals

The terminal palette (foreground, background, cursor, the full
16-slot ANSI set, and the glass opacity) is part of each theme
*rendition* -- every theme carries a dark scheme and a light scheme
designed together with its chrome.

The desktop spawns `foot` with **both** palettes: this rendition's in
its matching `[colors-dark]`/`[colors-light]` section and the
counterpart rendition's in the other, plus `initial-color-theme` set
to the session's current mode. foot switches between those two
sections at runtime on `SIGUSR1` (dark) / `SIGUSR2` (light), which the
shell sends to every terminal it launched (and can still prove is
alive) on each appearance switch -- so running terminals follow live,
scrollback included, and a terminal spawned in one mood then switched
is pixel-identical to one spawned in the other.

Two honest limits: a terminal you started some other way (from another
terminal, a `foot --server` you run yourself) is not on the shell's
list and keeps its colors until you restart it or signal it yourself;
and a *theme* pick (as opposed to an appearance switch) still only
affects new terminals -- the two sections a running foot holds belong
to the theme it was launched under.

## For dockapp and SDK authors

- Poll (or read on demand) `$XDG_STATE_HOME/chonkstep/appearance` for
  the current mode; write `light`/`dark`/`toggle` to
  `appearance-request` to ask for a change. That is the whole
  protocol.
- A connected dockapp does not need the files for *rendering*: the
  `ThemeChanged` push carries the resolved palette, and its
  `theme_toml` now includes `appearance = "light"|"dark"` (absent in
  streams from older desktops -- treat absent as dark).
- `chonk_ui::active_theme()` resolves `CHONKSTEP_THEME` +
  `CHONKSTEP_APPEARANCE` to the exact rendition the desktop is
  wearing at launch time.
