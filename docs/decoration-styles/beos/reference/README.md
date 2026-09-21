# Original BeOS reference images

These are OS screenshots, not modern recreations, CSS themes, Haiku captures,
or output from ChonkStep. `sources.json` records the original URLs and SHA-256
of every unmodified download.

* `r5-desktop.png`: **BeOS 5.0.3**, captured by **M0J01812**, 26 February 2023.
  [Original file and attribution](https://commons.wikimedia.org/wiki/File:Screenshot_from_2023-02-26_15-38-37.png).
  Licensed [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
  The unmodified 1280×1024 screenshot is the independent color/control oracle.
  Control drawings in `styles/beos/buttons.rs` reproduce its four 14×14 control
  crops as indexed colors; the crop coordinates are documented in the spec.
  Those adaptations retain this attribution.
* `beFresh.gif`, `beMenu1.gif`, `desktopContext.gif`, `3PartDeskbar.gif`:
  **Be, Inc.'s own BeOS User's Guide**, preserved by the Asleson mirror.
  [The Desktop](https://asleson.org/public/mirrors/www.be.com/documentation/User%27s%20Guide/01_basics/Basics02_Desktop.html),
  [The Deskbar](https://asleson.org/public/mirrors/www.be.com/documentation/User%27s%20Guide/01_basics/Basics03_Deskbar.html).
* `Window1.gif`, `Window2.gif`, `activeWindow.gif`, `resizeControls.gif`:
  [Be's window reference](https://asleson.org/public/mirrors/www.be.com/documentation/User%27s%20Guide/06_appendices/Appendices02_Windows.html).
* `menu1.gif`, `contextMenu.gif`:
  [Be's menu reference](https://asleson.org/public/mirrors/www.be.com/documentation/User%27s%20Guide/06_appendices/Appendices03_Menus.html).

The Be guide illustrations remain copyright their original owners, retained
here as historical design/reference evidence. They are not embedded in the
shipping renderer. Their GIF palette quantizes some highlights to 248;
the R5 PNG records 252. Do not mix these two color-depth renditions and call
the result a captured oracle. The guide's highlighted **Add-Ons** row is gray,
with black text, and its cascade arrows are hollow beveled triangles.

To check provenance without regenerating any image:

```sh
python3 - <<'PY'
import hashlib, json
from pathlib import Path
p = Path('docs/decoration-styles/beos/reference')
for source in json.loads((p / 'sources.json').read_text()):
    assert hashlib.sha256((p / source['file']).read_bytes()).hexdigest() == source['sha256']
print('All BeOS reference hashes match')
PY
```
