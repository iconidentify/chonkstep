#!/usr/bin/env python3
"""Derive sparse frame oracles from genuine OS captures, never our renderer.

Generation requires --write NEW_DIRECTORY. --check EXISTING_DIRECTORY verifies
decoded pixels and all metadata without overwriting anything.
"""
import argparse
import hashlib
import json
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
REFERENCE = ROOT / "docs/decoration-styles/system7/reference"


def cases():
    for depth in ("1bit", "8bit"):
        requests = json.loads((REFERENCE / depth / "requests.json").read_text())
        for request in requests:
            source = REFERENCE / depth / request["file"]
            with Image.open(source) as opened:
                image = opened.convert("RGBA")
            assert image.size == (1024, 768), source
            x, y, width, height = request["content"]
            left, top = x - 1, y - 19
            # Measured in reference/1bit/zoom-short-active.png and the
            # unobscured screen-edge shadow: 1 outline + 1 right/bottom shadow.
            frame_width = width + 3
            frame_height = 20 if request["shaded"] else height + 21
            bands = [("top", (0, 0, frame_width, 19))]
            if request["shaded"]:
                bands.append(("bottom", (0, 19, frame_width, 20)))
            else:
                bands.extend([
                    ("bottom", (0, height + 19, frame_width, height + 21)),
                    ("left", (0, 19, 1, height + 19)),
                    ("right", (width + 1, 19, width + 3, height + 19)),
                ])
            record = dict(request, source=f"{depth}/{request['file']}",
                          source_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
                          depth=depth, frame_size=[frame_width, frame_height],
                          client_offset=[1, 19], titlebar_height=18, parts=[],
                          style="system7", scale=1, palette="classic",
                          request=dict(
                              content_size=[width, 0 if request["shaded"] else height],
                              title=request["title"], focused=request["focused"],
                              resizable=request["resizable"],
                              buttons=[dict(kind=kind, hovered=request["pressed"] == kind,
                                            pressed=request["pressed"] == kind)
                                       for kind in (["Close", "Maximize"] if request["resizable"] else ["Close"])]))
            crops = []
            for name, box in bands:
                absolute = (left + box[0], top + box[1], left + box[2], top + box[3])
                assert 0 <= absolute[0] < absolute[2] <= image.width, absolute
                assert 0 <= absolute[1] < absolute[3] <= image.height, absolute
                crop = image.crop(absolute)
                # These two coordinates are outside the OS-painted shape.
                # Clear ONLY those background samples; every opaque pixel is
                # an unchanged capture pixel. The source PNG stays untouched.
                transparent = []
                if name == "top":
                    transparent.append((crop.width - 1, 0))
                if name == "bottom":
                    transparent.append((0, crop.height - 1))
                for point in transparent:
                    crop.putpixel(point, (0, 0, 0, 0))
                filename = f"{depth}/{Path(request['file']).stem}-{name}.png"
                record["parts"].append(dict(file=filename, offset=list(box[:2]),
                                           source_crop=list(absolute), size=list(crop.size),
                                           transparent=[list(point) for point in transparent]))
                crops.append((filename, crop))
            yield record, crops


def write(output):
    output.mkdir(parents=True, exist_ok=False)
    records = []
    for record, crops in cases():
        records.append(record)
        for filename, crop in crops:
            target = output / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            crop.save(target)
    (output / "manifest.json").write_text(json.dumps(records, ensure_ascii=False, indent=2) + "\n")
    print(f"Wrote {len(records)} capture-derived cases to {output}")


def check(output):
    expected = []
    filenames = set()
    for record, crops in cases():
        expected.append(record)
        for filename, crop in crops:
            filenames.add(filename)
            with Image.open(output / filename) as existing:
                assert existing.size == crop.size, filename
                assert existing.convert("RGBA").tobytes() == crop.tobytes(), filename
    assert {str(path.relative_to(output)) for path in output.rglob("*.png")} == filenames
    assert json.loads((output / "manifest.json").read_text()) == expected
    print(f"Verified {len(expected)} capture-derived cases, every opaque pixel unchanged")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--write", type=Path, metavar="NEW_DIRECTORY")
    action.add_argument("--check", type=Path, metavar="EXISTING_DIRECTORY")
    arguments = parser.parse_args()
    if arguments.write:
        write(arguments.write)
    else:
        check(arguments.check)
