"""Artifact-only checks; no Wayland fixture, scope, or memory stress is launched."""
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[1] / 'mixed-capture-fixture.py'
SPEC = importlib.util.spec_from_file_location('mixed_fixture_tested', SOURCE)
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


class MixedFixtureTests(unittest.TestCase):
    def test_worker_refuses_unconstrained_allocation(self):
        with mock.patch.object(M, 'scope_limits', side_effect=RuntimeError('unrelated scope')), \
                mock.patch.object(M, 'touched', side_effect=AssertionError('must not allocate')):
            with self.assertRaisesRegex(RuntimeError, 'unrelated scope'):
                M.memory_worker(SimpleNamespace())

    def test_worker_peak_retains_headroom_before_allocating(self):
        args = SimpleNamespace(payload_mib=970, churn_mib=32)
        with mock.patch.object(M, 'scope_limits', return_value={'memory_max_bytes': 1024**3}), \
                mock.patch.object(M, 'touched', side_effect=AssertionError('must not allocate')):
            with self.assertRaisesRegex(ValueError, '96 MiB'):
                M.memory_worker(args)

    def test_incomplete_final_worker_record_is_not_fabricated(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'events.jsonl'
            path.write_text('{"generation": 1}\n{"generation":')
            self.assertEqual(M.records(path), [{'generation': 1}])
            path.write_text('{"generation": 1}\n{"generation": 2}\n')
            self.assertEqual(M.records(path)[-1], {'generation': 2})

    def test_clean_client_exit_is_still_workload_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = object.__new__(M.Fixture)
            fixture.artifact = Path(temporary)
            fixture.children = {'foot': mock.Mock(poll=lambda: 0)}
            with self.assertRaisesRegex(RuntimeError, 'unexpectedly'):
                fixture.alive()
            data = json.loads((fixture.artifact / 'mixed-failure.json').read_text())
            self.assertEqual(data['unexpected_client_exits'], {'foot': 0})

    def test_late_capture_is_failure_and_preserves_phase_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = object.__new__(M.Fixture)
            fixture.artifact = Path(temporary)
            fixture.args = SimpleNamespace(capture_deadline_ms=500)
            fixture.phase_samples = []
            fixture.snapshot = mock.Mock(side_effect=[{'frame_callbacks': 3}, {'frame_callbacks': 8}])
            with self.assertRaisesRegex(TimeoutError, 'exceeded'):
                fixture.phase('warm-open', lambda: {'result': [20, 501]})
            data = json.loads((fixture.artifact / 'mixed-phases.json').read_text())
            self.assertEqual(data[0]['frame_callbacks'], 5)


if __name__ == '__main__':
    unittest.main()
