"""Capture the running reference.c fixture through its real OS input paths.

Call capture_matrix(page, output, depth) with a Playwright page running the
fixture at 1024x768, browser zoom 100%, DPR 1. Set Monitors/Color first and
save their settings separately. This does not read emulator memory or fonts.
Set WindowShade to two clicks without modifier keys before running the matrix.
"""
import json
from pathlib import Path


def capture_matrix(page, output, depth):
    output = Path(output) / depth
    output.mkdir(parents=True, exist_ok=True)
    if (output / "requests.json").exists():
        raise FileExistsError(f"Capture matrix already exists: {output}")
    canvas = page.locator("canvas")
    geometry = canvas.evaluate("e=>({w:e.width,h:e.height,r:e.getBoundingClientRect().toJSON(),dpr:devicePixelRatio,zoom:visualViewport.scale})")
    assert (geometry["w"], geometry["h"]) == (1024, 768), geometry
    assert (geometry["r"]["width"], geometry["r"]["height"]) == (1024, 768), geometry
    assert geometry["dpr"] == geometry["zoom"] == 1, geometry

    def move(x, y):
        bounds = canvas.bounding_box()
        page.mouse.move(bounds["x"] + x, bounds["y"] + y)
        # The emulated 60 Hz event queue must receive motion before the
        # subsequent button transition. These are capture-driver delays,
        # not compositor performance measurements or test success criteria.
        page.wait_for_timeout(150)

    def click(x, y):
        move(x, y)
        page.mouse.down()
        page.wait_for_timeout(100)
        page.mouse.up()
        page.wait_for_timeout(200)

    def key(value):
        page.keyboard.press(value, delay=120)
        page.wait_for_timeout(250)

    records = []

    def save(name, title, zoom, focused=True, pressed=None, content=(120, 140, 400, 240), shaded=False):
        filename = name + ".png"
        if (output / filename).exists():
            raise FileExistsError(output / filename)
        canvas.screenshot(path=str(output / filename))
        records.append(dict(file=filename, title=title, focused=focused,
                            resizable=zoom, proc_id=8 if zoom else 0,
                            pressed=pressed, shaded=shaded, content=list(content)))

    def held(name, title, zoom, box, x, shaded=False):
        move(x, 131)
        page.mouse.down()
        page.wait_for_timeout(250)
        try:
            save(name, title, zoom, pressed=box, shaded=shaded)
        finally:
            move(580, 450)
            page.mouse.up()
            page.wait_for_timeout(200)

    titles = [
        ("short", "Terminal"),
        ("long", "A deliberately long document title that reaches beyond the available title bar width"),
        ("empty", ""),
        ("latin", "Café - naïve - Ångström"),
    ]
    # The fixture's hide-cursor command preserves the held-button pixels.
    key("h")
    for variant, zoom in [("doc", False), ("zoom", True)]:
        key("z" if zoom else "d")
        for index, (label, title) in enumerate(titles, start=1):
            key(str(index))
            save(f"{variant}-{label}-active", title, zoom)
            click(700, 230)
            save(f"{variant}-{label}-inactive", title, zoom, focused=False)
            click(250, 220)
        key("1")
        held(f"{variant}-close-pressed", "Terminal", zoom, "Close", 134)
        if zoom:
            held("zoom-zoom-pressed", "Terminal", True, "Maximize", 506)
        for index, (label, title) in enumerate(titles, start=1):
            key("z" if zoom else "d")
            key(str(index))
            move(320, 131)
            for _ in range(2):
                page.mouse.down()
                page.wait_for_timeout(90)
                page.mouse.up()
                page.wait_for_timeout(90)
            page.wait_for_timeout(400)
            save(f"{variant}-{label}-shaded-active", title, zoom, shaded=True)
            if index == 1:
                held(f"{variant}-close-shaded-pressed", title, zoom, "Close", 134, shaded=True)
                if zoom:
                    held("zoom-zoom-shaded-pressed", title, True, "Maximize", 506, shaded=True)
            click(700, 230)
            save(f"{variant}-{label}-shaded-inactive", title, zoom, focused=False, shaded=True)
    key("z")
    key("1")
    key("e")
    save("zoom-screen-edge", "Terminal", True, content=(622, 525, 400, 240))
    key("r")
    save("glyph-ascii", "Terminal", True)
    for label in ["macroman-128-223", "macroman-224-255"]:
        key("g")
        save("glyph-" + label, "Terminal", True)
    key("g")
    (output / "requests.json").write_text(json.dumps(records, ensure_ascii=False, indent=2) + "\n")
    print(f"Captured {len(records)} genuine {depth} states; review every image before deriving goldens.")
