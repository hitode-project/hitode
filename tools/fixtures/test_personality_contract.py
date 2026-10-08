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

    def test_memory_fronts_require_both_results_and_native_entry_observations(self):
        self.assertEqual(evaluate(9, [1] + [1, 1] * 10), [])
        values = [1] + [1, 1] * 10
        values[4] = 0
        self.assertEqual(evaluate(9, values), ["alloc_ex"])

    def test_exception_frame_and_continuation_are_both_required(self):
        values = [1, 1, 1, 0, 1, 0, 2, 2, 1, 2, 1]
        self.assertEqual(evaluate(10, values), [])
        values[4] = 0
        values[7] = 0
        self.assertEqual(evaluate(10, values), ["raw_dispatch_frame", "native_continue"])

    def test_unload_requires_a_successful_observed_native_call(self):
        self.assertEqual(evaluate(11, [1, 1, 1]), [])
        self.assertEqual(evaluate(11, [1, 1, 0]), ["native_unload"])

    def test_loader_query_paths_require_observation_and_output_contracts(self):
        values = [1]*38
        values[29] = 0xc0000135
        values[32] = 0xc000000d
        values[34] = 0
        self.assertEqual(evaluate(12, values), [])
        values[2] = 0
        values[17] = 0
        values[30] = 0
        self.assertEqual(evaluate(12, values), ["altered_native_load", "address_unchanged", "native_missing"])

    def test_owned_sync_requires_recursion_counts_and_failure_output_contracts(self):
        values = [0, 0, 0, 0xffffffff, 0, 0, 0xc0000046, 0x12345678, 0, 288,
                  0, 0xc000000d, 0x12345678, 0xc0000047, 0x12345678, 0, 1,
                  0, 87, 0x12345678, 0xc000000d, 0, 0xc000000d, 0,
                  0xc0000005, 0, 0, 258, 0, 6]
        self.assertEqual(evaluate(13, values), [])
        self.assertEqual(evaluate(13, values[:-1]), ["missing_results"])
        values[3] = 0
        values[14] = 0
        values[21] = 0x12345678
        values[24] = 0x80000002
        self.assertEqual(evaluate(13, values), ["mutant_recursion", "semaphore_limit",
                                               "semaphore_invalid_initial", "semaphore_output_fault"])


if __name__ == "__main__":
    unittest.main()
