#!/usr/bin/env python3
"""Reproduce the compatible BeOS text atlases, never from OS font resources.

Requires Pillow and the hash-pinned Liberation Sans 2.1.5 regular/bold faces.
Use --check to verify committed output, or --output with a NEW directory.
"""
import argparse
import hashlib
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
ATLAS = ROOT / 'crates/wm-theme/src/styles/beos'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fonts', type=Path, default=Path('/usr/share/fonts/liberation'))
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--output', type=Path)
    args = parser.parse_args()
    sources = json.loads((ATLAS / 'font-sources.json').read_text())
    if args.output:
        args.output.mkdir()  # Accepted atlases are never silently refreshed.
    for weight in ['Regular', 'Bold']:
        file = args.fonts / f'LiberationSans-{weight}.ttf'
        assert hashlib.sha256(file.read_bytes()).hexdigest() == sources[file.name], file
        font = ImageFont.truetype(str(file), 12)
        data = bytearray()
        for code in [*range(32, 127), *range(160, 256)]:
            char = chr(code)
            image = Image.new('L', (16, 15))
            ImageDraw.Draw(image).text((0, 12), char, font=font, fill=255, anchor='ls')
            data.append(round(font.getlength(char)))
            data.extend(image.tobytes())
        name = f'{weight.lower()}.atlas'
        if args.check:
            assert data == (ATLAS / name).read_bytes(), f'{name}: rasterizer/source changed'
        else:
            (args.output / name).write_bytes(data)
        print(f'{name}: {len(data)} bytes, sha256 {hashlib.sha256(data).hexdigest()}')


if __name__ == '__main__':
    main()
