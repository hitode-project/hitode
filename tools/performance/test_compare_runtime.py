import copy
import unittest

from compare_runtime import signature, summarize


def report():
    return dict(frames=120, width=640, height=480, frame_fnv1a64='0123456789abcdef',
                elapsed_seconds=10.0, first_frame_seconds=2.0, failure=None,
                stop_reason='frame-limit', backend='jit', compile_strategy='fast',
                region_linking=True, codepage=932, guest_exit_code=None, missing_apis=[])


def rows():
    candidate = report()
    candidate['elapsed_seconds'] = 2.0
    candidate['first_frame_seconds'] = 1.0
    return [dict(variant='baseline', process_seconds=12.0, report=report()),
            dict(variant='candidate', process_seconds=3.0, report=candidate)]


class RuntimeComparisonTests(unittest.TestCase):
    def test_elapsed_process_and_first_frame_are_distinct(self):
        result = summarize(rows(), 120)
        self.assertEqual(result['elapsed_seconds']['speedup'], 5.0)
        self.assertEqual(result['process_seconds']['speedup'], 4.0)
        self.assertEqual(result['first_frame_seconds']['speedup'], 2.0)

    def test_incomplete_failed_and_nonnumeric_reports_are_rejected(self):
        for key, value in [('frames', 119), ('frames', True), ('failure', 'fault'),
                           ('elapsed_seconds', 0), ('elapsed_seconds', float('nan')),
                           ('elapsed_seconds', True), ('frame_fnv1a64', ''),
                           ('width', 0), ('stop_reason', None), ('missing_apis', None)]:
            with self.subTest(key=key, value=value):
                r = report()
                r[key] = value
                with self.assertRaises(ValueError):
                    signature(r, 120)

    def test_observable_mismatch_rejects_speedup(self):
        for key, value in [('frame_fnv1a64', 'ffffffffffffffff'), ('width', 800),
                           ('backend', 'interpreter'), ('region_linking', False),
                           ('missing_apis', ['missing!api']), ('guest_exit_code', 1)]:
            with self.subTest(key=key):
                data = rows()
                data[1]['report'][key] = value
                with self.assertRaises(ValueError):
                    summarize(data, 120)

    def test_unbalanced_variants_and_nonpositive_process_time_are_rejected(self):
        for data in [[], rows()[:1], rows() + rows()[:1]]:
            with self.assertRaises(ValueError):
                summarize(data, 120)
        data = rows()
        data[0]['process_seconds'] = 0
        with self.assertRaises(ValueError):
            summarize(data, 120)

    def test_nondeterministic_repetition_is_not_hidden_by_medians(self):
        data = rows() + copy.deepcopy(rows())
        data[-1]['report']['frame_fnv1a64'] = '1111111111111111'
        with self.assertRaises(ValueError):
            summarize(data, 120)

    def test_missing_first_frame_time_omits_only_that_metric(self):
        data = rows()
        data[1]['report']['first_frame_seconds'] = None
        result = summarize(data, 120)
        self.assertNotIn('first_frame_seconds', result)
        self.assertIn('elapsed_seconds', result)


if __name__ == '__main__':
    unittest.main()
