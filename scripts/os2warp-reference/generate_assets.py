#!/usr/bin/env python3
"""Reproduce the pinned IBM bitmap subsets. No network or live desktop changes.

--font-dir contains the two YAFF files listed in reference/sources.json.
--warpd names WARPD.BMP extracted from the pinned installation media.
--check alone verifies every committed asset and unmodified capture hash.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import struct

ROOT = Path(__file__).resolve().parents[2]
REFERENCE = ROOT / 'docs/decoration-styles/os2warp/reference'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def glyphs(path):
    result = {}
    for block in re.split(r'\n(?=0x[0-9a-f]+:)', path.read_text()):
        rows = re.findall(r'^    ([.@]+)$', block, re.M)
        if not rows:
            continue
        unicode = re.search(r'^u\+([0-9a-f]+):', block, re.M)
        literal = re.search(r"^'(.*)':", block, re.M)
        char = chr(int(unicode[1], 16)) if unicode else literal[1] if literal else ''
        if len(char) == 1:
            result[char] = rows
    return result


def atlas(path):
    source = glyphs(path)
    data = bytearray()
    for code in list(range(32, 127)) + list(range(160, 256)):
        rows = source.get(chr(code), source['?'])
        width = len(rows[0])
        assert width <= 16 and len(rows) <= 15
        data.append(width)
        for row in rows:
            data.extend([255 if bit == '@' else 0 for bit in row] + [0] * (16 - width))
        data.extend([0] * (16 * (15 - len(rows))))
    return data


def wallpapers(data):
    from PIL import Image
    offset = 0
    while True:
        assert data[offset:offset + 2] == b'BA'
        next_offset = struct.unpack_from('<I', data, offset + 6)[0]
        base = offset + 14
        width, height, planes, depth = struct.unpack_from('<HHHH', data, base + 18)
        assert planes == 1 and depth in (4, 8)
        start = struct.unpack_from('<I', data, base + 10)[0]
        palette = data[base + 26:base + 26 + 3 * (1 << depth)]
        pixels = data[start:start + ((width * depth + 31) // 32) * 4 * height]
        header = bytearray(data[base:base + 26])
        struct.pack_into('<I', header, 2, 26 + len(palette) + len(pixels))
        struct.pack_into('<I', header, 10, 26 + len(palette))
        if depth == 8:
            image = Image.open(io.BytesIO(header + palette + pixels)).convert('RGB')
            # Palette conversion is lossless: these sources have <=256 colors.
            indexed = image.convert('P', palette=Image.Palette.ADAPTIVE, colors=256)
            assert indexed.convert('RGB').tobytes() == image.tobytes()
            out = io.BytesIO()
            indexed.save(out, format='PNG', optimize=True)
            yield width, out.getvalue()
        if not next_offset:
            break
        offset = next_offset


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--font-dir', type=Path)
    parser.add_argument('--warpd', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    manifest = json.loads((REFERENCE / 'sources.json').read_text())
    outputs = {}
    if args.font_dir:
        for name, font in zip(('regular', 'bold'), manifest['fonts']):
            path = args.font_dir / font['file']
            assert digest(path.read_bytes()) == font['sha256'], f'wrong font: {path}'
            outputs[ROOT / f'crates/wm-theme/src/styles/os2warp/{name}.atlas'] = atlas(path)
    if args.warpd:
        data = args.warpd.read_bytes()
        assert digest(data) == manifest['wallpaper']['bitmap_sha256'], 'wrong WARPD.BMP'
        for width, png in wallpapers(data):
            outputs[ROOT / f'crates/chonk-shell/assets/wallpapers/os2-warp-4-{width}.png'] = png
    for path, data in outputs.items():
        if args.check:
            assert path.read_bytes() == data, f'asset differs: {path}'
        else:
            path.write_bytes(data)
    for capture in manifest['captures']:
        assert digest((REFERENCE / capture['file']).read_bytes()) == capture['sha256']
    for asset in manifest['assets']:
        assert digest((ROOT / asset['file']).read_bytes()) == asset['sha256']
    print('OS/2 reference and asset hashes match.')


if __name__ == '__main__':
    main()
