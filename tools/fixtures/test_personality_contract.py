"""Unit tests for scalar personality-contract validation; no native execution."""
import unittest
from personality_contract import evaluate


class Contracts(unittest.TestCase):
    def test_thread_history_limit_is_a_failure(self):
        self.assertEqual(evaluate(1, [63, 63, 8]), ["threads_200", "threads_1000"])
        self.assertEqual(evaluate(1, [200, 1000, 0]), [])

    def test_export_counts_are_observations_but_identity_is_required(self):
        self.assertEqual(evaluate(4, [56, 0xffffffff, 0xffffff, 0, 3000, 1800, 1, 1, 1]), [])
        self.assertEqual(evaluate(4, [56, 0xffffffff, 0xffffff, 0, 3000, 1800, 0, 1, 0]), ["heap_forwarder"])

    def test_no_result_and_unobserved_hook_are_not_passes(self):
        self.assertEqual(evaluate(7, []), ["missing_results"])
        self.assertEqual(evaluate(7, [1, 1, 0, 1, 0]), ["load_library", "load_library_ex"])
        self.assertEqual(evaluate(7, [1, 1, 1, 1, 5]), [])

    def test_ansi_path_requires_crt(self):
        self.assertEqual(evaluate(8, [936, 1, 1, 0]), ["ansi_filename"])
        self.assertEqual(evaluate(8, [936, 1, 1, 1]), [])


if __name__ == "__main__":
    unittest.main()
