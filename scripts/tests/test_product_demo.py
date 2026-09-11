"""A later clean PNG must not hide a damaged frame in the share video."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from PIL import Image

spec = importlib.util.spec_from_file_location('product_demo', Path(__file__).parents[1] / 'product-demo.py')
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


class RecordedPixels(unittest.TestCase):
    timeline = [{'seconds': 6, 'action': 'Select a region'}]
    clean = bytes([207, 203, 201])*680*2

    def verify(self, frames):
        with patch.object(demo.subprocess, 'check_output', return_value=frames):
            return demo.verify_capture_video(Path('sample.mp4'), self.timeline)

    def test_clean_decoded_frames_are_counted(self):
        result = self.verify(self.clean*45)
        self.assertEqual(result['frames'], 45)
        self.assertEqual(result['max_header_spread'], 0)

    def test_one_stale_strip_between_clean_frames_rejects_the_video(self):
        bad = bytearray(self.clean)
        bad[300:600] = bytes([126,124,123])*100
        with self.assertRaisesRegex(RuntimeError, 'recorded frame 15'):
            self.verify(self.clean*15 + bad + self.clean*29)

    def test_blank_short_and_truncated_videos_cannot_pass(self):
        for pixels in (b'', self.clean*2, self.clean*45+b'x', bytes(len(self.clean)*45)):
            with self.subTest(length=len(pixels)), self.assertRaises(RuntimeError):
                self.verify(pixels)

    def test_decoder_failures_are_not_treated_as_empty_success(self):
        with patch.object(demo.subprocess, 'check_output', side_effect=subprocess.CalledProcessError(1, 'ffmpeg')):
            with self.assertRaises(subprocess.CalledProcessError):
                demo.verify_capture_video(Path('sample.mp4'), self.timeline)

    def test_video_colors_match_the_independent_screenshot(self):
        with tempfile.TemporaryDirectory() as temporary:
            reference = Path(temporary)/'reference.png'
            Image.new('RGB', (1920,1080), (19,23,35)).save(reference)
            with patch.object(demo.subprocess, 'check_output', return_value=bytes([18,22,35])*16*8*45):
                result = demo.verify_capture_colors(Path('sample.mp4'), self.timeline, reference)
            self.assertEqual(result['max_channel_difference'], 1)
            self.assertEqual(result['frames'], 45)

    def test_uniformly_washed_out_video_cannot_pass_the_color_check(self):
        with tempfile.TemporaryDirectory() as temporary:
            reference = Path(temporary)/'reference.png'
            Image.new('RGB', (1920,1080), (19,23,35)).save(reference)
            # Limited-range samples mislabeled full-range have no spatial
            # variation, but lift dark RGB levels by about twelve steps.
            with patch.object(demo.subprocess, 'check_output', return_value=bytes([31,35,46])*16*8*45):
                with self.assertRaisesRegex(RuntimeError, 'differ from the screenshot'):
                    demo.verify_capture_colors(Path('sample.mp4'), self.timeline, reference)

    def test_dynamic_fixture_dot_does_not_hide_a_stale_selection_edge(self):
        with tempfile.TemporaryDirectory() as temporary:
            reference = Path(temporary)/'reference.png'
            captured = Path(temporary)/'captured.png'
            image = Image.new('RGB', (1920,1080), (19,23,35))
            image.save(reference)
            image.putpixel((1734,295),(255,255,255))
            image.save(captured)
            self.assertEqual(demo.verify_capture_still(captured,reference)['max_channel_difference'],0)
            image.putpixel((1568,210),(255,255,255))
            image.save(captured)
            with self.assertRaisesRegex(RuntimeError, 'differs from a full repaint'):
                demo.verify_capture_still(captured,reference)
