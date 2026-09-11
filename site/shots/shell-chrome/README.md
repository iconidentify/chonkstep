# Window-derived shell chrome

Unmodified 1280×800 captures from private nested Wayland sessions with two real Foot clients.
The same desktop is switched between WindowMaker and System 7. Terminal text is fixture content.
Scale 2 uses twice the physical pixels per logical unit; the output size stays 1280×800.

Reproduce the scenes and interaction/pixel assertions:

```sh
scripts/e2e.sh --headless --release --test shell_chrome
```

The test also verifies menu action hit targets, minimize/restore, Alt-Tab dismissal, live
Overview textures and cached captions, and repeated reload while a menu owns input.
Native X11 shape/input behavior is separately exercised by the private Xvfb tests.

| Surface / scale | WindowMaker | System 7 |
| --- | --- | --- |
| Root menu / 1× | [![windowmaker Root menu 1×](windowmaker-root-menu-1x.png)](windowmaker-root-menu-1x.png) | [![system7 Root menu 1×](system7-root-menu-1x.png)](system7-root-menu-1x.png) |
| Window menu / 1× | [![windowmaker Window menu 1×](windowmaker-window-menu-1x.png)](windowmaker-window-menu-1x.png) | [![system7 Window menu 1×](system7-window-menu-1x.png)](system7-window-menu-1x.png) |
| Minimized icon / 1× | [![windowmaker Minimized icon 1×](windowmaker-minimized-icon-1x.png)](windowmaker-minimized-icon-1x.png) | [![system7 Minimized icon 1×](system7-minimized-icon-1x.png)](system7-minimized-icon-1x.png) |
| Switcher / 1× | [![windowmaker Switcher 1×](windowmaker-switcher-1x.png)](windowmaker-switcher-1x.png) | [![system7 Switcher 1×](system7-switcher-1x.png)](system7-switcher-1x.png) |
| Overview / 1× | [![windowmaker Overview 1×](windowmaker-overview-1x.png)](windowmaker-overview-1x.png) | [![system7 Overview 1×](system7-overview-1x.png)](system7-overview-1x.png) |
| Root menu / 2× | [![windowmaker Root menu 2×](windowmaker-root-menu-2x.png)](windowmaker-root-menu-2x.png) | [![system7 Root menu 2×](system7-root-menu-2x.png)](system7-root-menu-2x.png) |
| Window menu / 2× | [![windowmaker Window menu 2×](windowmaker-window-menu-2x.png)](windowmaker-window-menu-2x.png) | [![system7 Window menu 2×](system7-window-menu-2x.png)](system7-window-menu-2x.png) |
| Minimized icon / 2× | [![windowmaker Minimized icon 2×](windowmaker-minimized-icon-2x.png)](windowmaker-minimized-icon-2x.png) | [![system7 Minimized icon 2×](system7-minimized-icon-2x.png)](system7-minimized-icon-2x.png) |
| Switcher / 2× | [![windowmaker Switcher 2×](windowmaker-switcher-2x.png)](windowmaker-switcher-2x.png) | [![system7 Switcher 2×](system7-switcher-2x.png)](system7-switcher-2x.png) |
| Overview / 2× | [![windowmaker Overview 2×](windowmaker-overview-2x.png)](windowmaker-overview-2x.png) | [![system7 Overview 2×](system7-overview-2x.png)](system7-overview-2x.png) |
