# System 7 themes

Choose **System 7 Classic**, **System 7 Light Gray**, or **System 7 Dark Gray**
from Omarchy's Theme picker. Each choice includes its window and menu recipe,
application palette, desktop pattern, and picker preview. New installs and
session starts register them automatically.

Keep `decoration_style = "auto"` (the default). The selected theme carries
`preferred_decoration_style = "system7"` in its public `chonkstep.toml` descriptor.
Selecting Obsidian, Washi, Relay, or another theme also selects that theme's
native frame recipe. Keyboard behavior remains a separate preference.

| Theme | Desktop pattern |
| --- | --- |
| System 7 Classic | QuickDraw `gray`: alternating black and white pixels |
| System 7 Light Gray | QuickDraw `ltGray`: 25% black, 75% white |
| System 7 Dark Gray | QuickDraw `dkGray`: 75% black, 25% white |

The patterns are rasterized from the period's 8×8 bit patterns. Apple's
[Inside Macintosh: Imaging With QuickDraw](https://dev.os9.ca/techpubs/mac/QuickDraw/QuickDraw-59.html)
documents the standard Macintosh gray desktop and the predefined gray patterns.
The [System 7.5.3 reference captures](decoration-styles/system7/reference/README.md)
also show the classic desktop and provide the measured window chrome.

These are monochrome System 7 designs. Their backgrounds remain unchanged when
the application appearance is overridden. Exported `*-pattern.png` backgrounds
tile at native pixel size in ChonkStep, including when selected through Omarchy;
they are never stretched or smoothed. The Omarchy bar and application services
continue to provide desktop navigation in the exported black-and-white palette.

Minimized windows have clickable desktop preview tiles by default. Drag a tile
to reposition it, click to restore, or select its window in Alt-Tab.
