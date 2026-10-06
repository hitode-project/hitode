import json
from pathlib import Path
import tempfile
import unittest

from compare_loop_profiles import compare, load, markdown


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def source(self, name, report=None, samples=100, parts=None, instructions=None):
        directory = self.root / name
        directory.mkdir()
        if report is not None:
            (directory / 'report.json').write_text(json.dumps(report), encoding='utf-8')
        attribution = {'instruction_map': True, 'windows': {'all': {
            'samples': samples, 'jit_parts_pct': parts or {},
            'guest_instructions': instructions or []}}}
        (directory / 'pc-attribution.json').write_text(json.dumps(attribution), encoding='utf-8')
        return directory

    def test_share_delta_is_percentage_points_not_speedup(self):
        a = load(self.source('a', parts={'check': 10}), 'all')
        b = load(self.source('b', samples=200, parts={'check': 5}), 'all')
        result = compare(a, b)
        self.assertEqual(result['sections']['jit_parts_pct'][0]['delta'], -5)
        self.assertIn('percentage points', markdown(result))
        self.assertNotIn('speedup', result)

    def test_empty_window_and_truncated_top_list_do_not_invent_zero(self):
        instruction = {'pc': '0x1000', 'class': 'int', 'part': 'body', 'pct': 20}
        a = load(self.source('a', parts={'check': 10}, instructions=[instruction]), 'all')
        b = load(self.source('b', samples=0), 'all')
        result = compare(a, b)
        self.assertIsNone(result['sections']['jit_parts_pct'][0]['candidate'])
        self.assertIsNone(result['sections']['guest_top_pct'][0]['delta'])
        b['window']['samples'] = 100
        result = compare(a, b)
        self.assertEqual(result['sections']['jit_parts_pct'][0]['candidate'], 0)
        self.assertIsNone(result['sections']['guest_top_pct'][0]['candidate'])

    def test_frame_and_presentation_intervals_stay_separate(self):
        report = {'frames': 50, 'performance': {
            'warm_wall_time_between_frame_observations': {'samples': 49, 'p95_ms': 90},
            'warm_host_present_intervals': {'samples': 9, 'p95_ms': 120},
            'warm_host_present_cost': {'samples': 0, 'p95_ms': 0}}}
        source = load(self.source('a', report=report), 'all')
        self.assertEqual(source['metrics']['frame_wall.p95_ms'], 90)
        self.assertEqual(source['metrics']['present_interval.p95_ms'], 120)
        self.assertIsNone(source['metrics']['present_cost.p95_ms'])

    def test_mismatched_or_failed_work_is_marked(self):
        a = load(self.source('a', report={'frames': 50, 'frame_fnv1a64': 'a'}), 'all')
        b = load(self.source('b', report={'frames': 60, 'frame_fnv1a64': 'b',
                                        'failure': 'fault'}), 'all')
        warnings = '\n'.join(compare(a, b)['warnings'])
        self.assertIn('Frame counts differ', warnings)
        self.assertIn('checksums differ', warnings)
        self.assertIn('guest failure', warnings)

    def test_missing_requested_window_and_invalid_numbers_fail(self):
        directory = self.source('a', report={'frames': float('nan')})
        with self.assertRaisesRegex(ValueError, 'absent'):
            load(directory, 'execution')
        with self.assertRaisesRegex(ValueError, 'finite'):
            load(directory, 'all')


if __name__ == '__main__':
    unittest.main()
