# Reviewed ChonkStep output

These are **implementation previews**, distinct from the unmodified historical
images in [reference](../reference/README.md). They are not acceptance oracles.

The `desktop-*`, `menu-*`, `switcher-*`, `overview-*`, and `minimized-*` images
come from the real frame/shell renderers through this display-free example:

```sh
cargo run --locked -p wm-theme --example beos_preview -- /tmp/beos-new-review
```

The desktop composition uses illustrative client content and empty preview
tiles. Its chrome is the implementation used by the window manager. Review
at native image size: [1×](desktop-1x.png), [1.25×](desktop-1.25x.png),
[1.5×](desktop-1.5x.png), [2×](desktop-2x.png).

The `live-*` images are unmodified screenshots from the nested compositor's
BeOS acceptance test, using actual Wayland clients and the installed Omarchy
desktop menu. Test clients deliberately display no private shell content:

```sh
scripts/e2e.sh --headless --test beos
```

| Surface | 1× | 2× |
| --- | --- | --- |
| Live frames and tab cutout | [Screenshot](live-frames-1x.png) | [Screenshot](live-frames-2x.png) |
| Live desktop menu | [Screenshot](live-menu-1x.png) | [Screenshot](live-menu-2x.png) |

For measured source pixels, typography limitations, and modern adaptations,
see the [specification](../../beos.md).
