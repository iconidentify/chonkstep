"""Independent pixel/coverage checks for the genuine OS acceptance oracles.

These tests read screenshots and atlas drawings; they never invoke a renderer
or regenerate an expected image from implementation output.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
REFERENCE = ROOT / 'docs/decoration-styles/system7/reference'
GOLDENS = ROOT / 'crates/wm-theme/tests/fixtures/system7'
ATLAS = ROOT / 'crates/wm-theme/src/styles/system7/title.atlas'
SPEC = importlib.util.spec_from_file_location(
    'system7_crop', ROOT / 'scripts/system7-reference/crop_goldens.py')
CROP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CROP)
BLACK = (0, 0, 0)
WHITE = (255, 255, 255)


def rgb(path):
    with Image.open(path) as opened:
        return opened.convert('RGB')


def glyphs():
    result = {}
    for line in ATLAS.read_text().splitlines():
        if not line or line.startswith('#'):
            continue
        fields = line.split()
        code, advance, top = int(fields[0], 16), int(fields[1]), int(fields[2])
        rows = [int(word, 16) for word in fields[3:]]
        if code in result or not (1 <= advance <= 24) or not (0 <= top < 15):
            raise AssertionError(f'invalid/duplicate glyph: {line}')
        if not rows or top + len(rows) > 15 or any(not 0 <= row < 1 << advance for row in rows):
            raise AssertionError(f'glyph exceeds its cell: {line}')
        result[code] = advance, [0] * top + rows + [0] * (15 - top - len(rows))
    return result


class System7ReferenceTests(unittest.TestCase):
    def test_full_state_matrix_and_depth_are_real_pixels(self):
        for depth in ('1bit', '8bit'):
            requests = json.loads((REFERENCE / depth / 'requests.json').read_text())
            self.assertEqual(len(requests), 42)
            self.assertEqual(len({r['file'] for r in requests}), 42)
            for zoom in (False, True):
                for title in ('Terminal', '', 'Café - naïve - Ångström',
                              'A deliberately long document title that reaches beyond the available title bar width'):
                    for focus in (False, True):
                        for shade in (False, True):
                            states = [r for r in requests if r['resizable'] == zoom
                                      and r['title'] == title and r['focused'] == focus
                                      and r['shaded'] == shade and r['pressed'] is None
                                      and not r['file'].startswith('glyph-')
                                      and 'edge' not in r['file']]
                            self.assertEqual(len(states), 1, (depth, zoom, title, focus, shade))
                for shade in (False, True):
                    for box in (('Close', 'Maximize') if zoom else ('Close',)):
                        self.assertEqual(sum(r['resizable'] == zoom and r['shaded'] == shade
                                             and r['pressed'] == box for r in requests), 1)
            for request in requests:
                with self.subTest(depth=depth, file=request['file']):
                    image = rgb(REFERENCE / depth / request['file'])
                    self.assertEqual(image.size, (1024, 768))
                    colors = {color for _, color in image.getcolors(1024 * 768)}
                    if depth == '1bit':
                        self.assertEqual(colors, {BLACK, WHITE})
                    else:
                        self.assertGreater(len(colors), 2)
                    self.assertEqual(request['proc_id'], 8 if request['resizable'] else 0)

    def test_outline_shadow_and_unpainted_corners(self):
        im = rgb(REFERENCE / '1bit/zoom-short-active.png')
        for x in range(119, 521):
            self.assertEqual(im.getpixel((x, 121)), BLACK)
            self.assertEqual(im.getpixel((x, 380)), BLACK)
        for y in range(140, 380):
            self.assertEqual(im.getpixel((119, y)), BLACK)
            self.assertEqual(im.getpixel((520, y)), BLACK)
            self.assertEqual(im.getpixel((521, y)), BLACK)
        for x in range(120, 522):
            self.assertEqual(im.getpixel((x, 381)), BLACK)
        edge = rgb(REFERENCE / '1bit/zoom-screen-edge.png')
        for x in range(622, 1024):
            self.assertEqual(edge.getpixel((x, 766)), BLACK)
        # This specific background phase proves the bottom-left isn't painted.
        self.assertEqual(edge.getpixel((621, 766)), WHITE)

    def test_stripes_and_pressed_boxes(self):
        im = rgb(REFERENCE / '1bit/zoom-empty-active.png')
        self.assertEqual([y for y in range(122, 139) if im.getpixel((160, y)) == BLACK],
                         [125, 127, 129, 131, 133, 135])
        for x in (127, 139, 500, 512):
            for y in range(125, 136):
                self.assertEqual(im.getpixel((x, y)), WHITE)
        expected = [
            '###########', '#....#....#', '#.#..#..#.#', '#..#.#.#..#',
            '#.........#', '####...####', '#.........#', '#..#.#.#..#',
            '#.#..#..#.#', '#....#....#', '###########',
        ]
        for name, x in [('zoom-close-pressed', 128), ('zoom-zoom-pressed', 501),
                        ('doc-close-pressed', 128), ('zoom-close-shaded-pressed', 128),
                        ('zoom-zoom-shaded-pressed', 501), ('doc-close-shaded-pressed', 128)]:
            im = rgb(REFERENCE / '1bit' / (name + '.png'))
            actual = [''.join('#' if im.getpixel((x + dx, 125 + dy)) == BLACK else '.'
                              for dx in range(11)) for dy in range(11)]
            self.assertEqual(actual, expected, name)

    def test_classic_focus_does_not_change_other_frame_bands(self):
        records = json.loads((GOLDENS / 'manifest.json').read_text())
        by_source = {record['source']: record for record in records}
        for source, record in by_source.items():
            if not source.startswith('1bit/') or not source.endswith('-active.png'):
                continue
            inactive = by_source[source.replace('-active.png', '-inactive.png')]
            for part, other in zip(record['parts'], inactive['parts']):
                if part['file'].endswith('-top.png'):
                    continue
                self.assertEqual(rgb(GOLDENS / part['file']).tobytes(),
                                 rgb(GOLDENS / other['file']).tobytes(), source)
        for variant in ('doc', 'zoom'):
            im = rgb(REFERENCE / '1bit' / f'{variant}-short-inactive.png')
            self.assertEqual(im.getpixel((292, 126)), BLACK)
            self.assertEqual(im.getpixel((160, 125)), WHITE)
            self.assertEqual(im.getpixel((128, 125)), WHITE)

    def test_all_ascii_and_non_ascii_title_glyphs_match_capture(self):
        atlas = glyphs()
        self.assertEqual(set(atlas), set(range(32, 127)) | set(range(160, 256)))
        images = {name: rgb(REFERENCE / '1bit' / f'glyph-{name}.png')
                  for name in ('ascii', 'macroman-128-223', 'macroman-224-255')}
        for code in list(range(32, 127)) + list(map(ord, 'éïÅö')):
            with self.subTest(character=chr(code)):
                byte = chr(code).encode('mac_roman')[0]
                if byte < 128:
                    name, index = 'ascii', byte - 32
                elif byte < 224:
                    name, index = 'macroman-128-223', byte - 128
                else:
                    name, index = 'macroman-224-255', byte - 224
                im = images[name]
                x, baseline = 132 + index % 16 * 24, 164 + index // 16 * 28
                advance, rows = atlas[code]
                ruler = [im.getpixel((x + dx, baseline + 5)) for dx in range(24)]
                self.assertEqual(ruler, [BLACK] * advance + [WHITE] * (24 - advance))
                # Compare the entire24-pixel chart cell: an incorrect bearing,
                # advance or clipped overhang cannot hide outside the atlas width.
                for dy, row in enumerate(rows):
                    actual = [im.getpixel((x + dx, baseline - 12 + dy)) for dx in range(24)]
                    expected = [BLACK if row & (1 << (advance - 1 - dx)) else WHITE
                                for dx in range(advance)] + [WHITE] * (24 - advance)
                    self.assertEqual(actual, expected, f'{chr(code)} row{dy}')

    def test_complete_title_ink_centering_clipping_and_accents(self):
        atlas = glyphs()
        requests = json.loads((REFERENCE / '1bit/requests.json').read_text())
        for request in requests:
            if request['focused']:
                continue
            with self.subTest(file=request['file']):
                widths = [atlas[ord(character)][0] for character in request['title']]
                visible_width = min(sum(widths), 336)
                expected = Image.new('RGB', (400, 17), WHITE)
                start = (400 - visible_width) // 2
                cursor = start
                for character in request['title']:
                    width, rows = atlas[ord(character)]
                    for dy, row in enumerate(rows):
                        for dx in range(width):
                            if cursor + dx < start + visible_width and row & (1 << (width - 1 - dx)):
                                expected.putpixel((cursor + dx, dy + 1), BLACK)
                    cursor += width
                actual = rgb(REFERENCE / '1bit' / request['file']).crop((120, 122, 520, 139))
                self.assertEqual(actual.tobytes(), expected.tobytes())

    def test_capture_crops_hashes_and_metadata_are_unchanged(self):
        CROP.check(GOLDENS)
        records = json.loads((GOLDENS / 'manifest.json').read_text())
        self.assertEqual(len(records), 84)
        for record in records:
            with self.subTest(source=record['source']):
                self.assertEqual(record['source_sha256'], hashlib.sha256(
                    (REFERENCE / record['source']).read_bytes()).hexdigest())
                self.assertEqual(record['frame_size'], [403, 20 if record['shaded'] else 261])
                self.assertEqual(record['client_offset'], [1, 19])
                for part in record['parts']:
                    with Image.open(GOLDENS / part['file']) as im:
                        alpha = im.convert('RGBA').getchannel('A')
                        counts = dict((value, count) for count, value in alpha.getcolors())
                        self.assertEqual(counts.get(0, 0), len(part['transparent']))
                        self.assertEqual(set(counts) - {0, 255}, set())
        provenance = json.loads((REFERENCE / 'provenance.json').read_text())
        self.assertEqual(provenance['fixture_source_sha256'], hashlib.sha256(
            (ROOT / provenance['fixture_source']).read_bytes()).hexdigest())

    def test_golden_writer_refuses_overwriting_accepted_oracle(self):
        with tempfile.TemporaryDirectory() as directory:
            sentinel = Path(directory) / 'sentinel'
            sentinel.write_text('keep')
            with self.assertRaises(FileExistsError):
                CROP.write(Path(directory))
            self.assertEqual(sentinel.read_text(), 'keep')


if __name__ == '__main__':
    unittest.main()
