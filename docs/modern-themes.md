# Modern themes

For period Macintosh window chrome and repeating desktop patterns, see
[System 7 themes](system7-themes.md).

Obsidian, Washi and Relay share one modern rendering recipe. Their colors,
typography and shell metrics live in the Theme payload; the compositor does not
branch on theme IDs. The original interactive reference is preserved separately
from the native implementation.

```toml
theme = "obsidian"             # also "washi" or "relay"
decoration_style = "auto"
omarchy_shell = true
omarchy_bar = true
interaction_mode = "desktop"
```

Omit `appearance` to use the selected theme's native appearance: Obsidian and
Relay are dark, Washi is light. Each also supplies a companion appearance.
Changing the configured theme also changes its default artwork while preserving
a separately chosen wallpaper.
Explicit `windowmaker`, `system7` and `modern` frame selections still work;
the Omarchy bar remains the desktop's navigation surface under each override.

The Omarchy installer registers the built-in themes in the user's theme picker.
ChonkStep also registers missing themes before starting Omarchy's shell on each
login or hot restart, so package upgrades add new themes automatically. Existing
theme directories and user customizations are preserved. To refresh an existing
export explicitly, run `omarchy-export-themes`; `--missing` only adds new themes.

Select the exported theme from Omarchy's existing picker. Its `chonkstep.toml`
carries the complete public `Theme` at 1x, so following
Omarchy retains the modern design under the stable `omarchy` identity. The
descriptor owns Chonkstep's colors, appearance, fonts and geometry; the matching
`colors.toml` and `shell.toml` own the exported application and Omarchy shell
palette. Regenerate all three together after an authored change. Palette-only
Omarchy themes retain the existing classic mapping.

Obsidian, Washi, and Relay have no Chonkstep dock or bottom workspace switcher.
Their workspace navigation and system apps live in Omarchy's menu bar. The
[Agent Sessions plugin](../omarchy/plugins/chonkstep.agents/README.md) provides
agent status, search, and terminal/session navigation there. All themes use the same compositor core.
Overview presents a grid of workspace cards
with live windows, actual window counts, and empty desks you can select to
create. The ordinary window switcher and classic Overview remain available
under their existing theme recipes.

Omarchy's existing shell owns the top bar, its menus and system indicators.
Chonkstep does not provide a replacement bar or reserve space for a modern dock.
Themes do not replace application
content: terminal and toolkit applications continue to receive their palette
through the existing theme export and appearance integration.

## Extending the theme API

`Theme.chrome` is an optional serialized semantic token table. Its absence selects
the classic decoration recipe. Colors are `background`, `panel`, `surface`, `raised`,
`text`, `muted`, `line`, `accent`, `accent_text`, `selection`, `success`, and
`danger`. Frame and Overview metrics are separate from color. The
frame's `round_focus_mark` chooses a circular title marker independently of its
corner radius. The optional `chrome.instrument` table supplies `background`
and `border` colors for Omarchy popover exports; omitted colors use the shared
surface and line colors. These tokens remain available to external shell
integrations.
`chrome.shadow` supplies an offset, blur and color independently from frame
input geometry. Future palettes can reuse these recipes by supplying tokens
rather than duplicating painters.

Metrics are normalized before allocation and scaled once into device pixels.
The exported theme descriptor includes custom tokens and alternate appearances.
ChonkStep normalizes it at load and applies the output scale once.

Modern decoration surfaces use sparse raster bands for glyphs and corners and
retained solid rectangles for flat rows and borders. Unchanged pieces retain
their GPU identity and server pixels. Client interiors are not rasterized by the
theme. Native Overview keeps live client textures. Font files are bundled under
their adjacent SIL Open Font Licenses and loaded into the session's resident
font database lazily when the modern recipe is first selected. Classic-only
sessions do not parse or copy those font files.

Wayland draws shadows around the frame perimeter. Client interiors keep the
ordinary texture path; only their corner squares need a mask shader. A scoped
texture read lock protects both draws together. Retained opacity regions still
let the compositor skip covered content.
The resize background is omitted when a committed opaque client completely
covers it. Emitted curved borders own their corner pixels, avoiding overlapping
straight and curved outlines; shaded frames retain their existing border path.
Rounded visual corners and input agree; the invisible resize margin remains
available. Shadow blur follows the reference CSS values with a bounded Gaussian
approximation, so it is not a browser rasterization pixel oracle. Native X11
uses Shape for rounded chrome; outer shadows require a separate X compositor
and are not supplied by this renderer.

Blurred shadows share premultiplied color atlases by radius, blur and color. The
cache retains at most eight textures, totaling 2 MiB, and discards temporary CPU pixels after
upload. Upload completion is paid once on a cache miss; the private textures
remain immutable afterward. Existing scenes can retain evicted textures until
released. Tiny frames, large offsets, nonuniform presentation, and authored
blurs too thin for the capped atlas use a bounded analytic fallback.
During ordinary window movement, one retained shadow element binds its atlas
once and batches its perimeter tiles without allocations or per-tile texture
synchronization. Atlas coordinates are interpolated from tile vertices. Only
the tiny inner corners need a silhouette shader;
exterior shadow pixels use ordinary texture sampling. Fragmented damage uses
bounded stack storage. Client-buffer ownership and synchronization guarantees
are preserved.

Window-only exports retain their existing visual-frame bounds and crop outer
shadows. Desktop and region screenshots include shadows within the selection.

See [Chonk Agents](../examples/chonk-agents/README.md) for the optional local
agent instrument, normal CLI session adapters, T3 connection setup, and event
contract.

Validation includes legacy pixel oracles, allocation/storage ceilings, cold
font I/O checks, native input/scale/theme transitions, and paired release
measurements. Native nested rendering verifies shell behavior; it does not
replace frame pacing measurements on a hardware KMS session.

## Isolated native preview

```sh
cargo build --release -p chonkstep-wayland -p chonk-shell --bin chonkstep-wayland --bin omarchy-export-themes
scripts/preview-modern.sh obsidian
```

Run from a Wayland session; substitute `washi` or `relay` for the other palettes.
The preview opens the actual compositor with its own temporary configuration,
state, runtime sockets and agent broker. It starts the installed Omarchy shell
and shows its existing bar, applying exported tokens through Omarchy's own
theme API to this isolated instance. Close the outer desktop window to
stop it. It does not change your current desktop or invoke any model. Logs are
kept in the printed temporary directory. The optional agent tile requires
Pillow; opening its review app also requires GTK 4 and PyGObject.

The installed Omarchy shell still reads its usual user configuration and idle
state from HOME. Its menus, notifications and idle behavior remain Omarchy's;
the preview's private XDG directories do not make it an operating-system sandbox.

Direct Wayland nesting stalled with both NVIDIA and Mesa on the development
host; the cause is undetermined. If the host's nested Wayland connection stalls, the outer preview window can
use the host X server instead. The desktop inside remains a Wayland compositor
with the actual Omarchy shell:

```sh
CHONKSTEP_PREVIEW_HOST_BACKEND=x11 scripts/preview-modern.sh obsidian
```

On the development host, this alternate connection also requires Mesa software
rendering because NVIDIA cannot create the requested X11 EGL context. With the
Mesa GLVND vendor file installed at the Arch location, the working invocation is:

```sh
CHONKSTEP_PREVIEW_HOST_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
  __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json \
  scripts/preview-modern.sh obsidian
```

These overrides apply only to the preview process tree. Software rendering has
additional blurred-shadow costs and a first-use shader stall; use the preview
for visual and interaction review, not as evidence of hardware performance.
