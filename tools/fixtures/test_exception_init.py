"""Validate DLL-init exception observations without launching a process."""
import unittest

import exception_init as fixture


class ExceptionInitTests(unittest.TestCase):
    def test_all_cases_include_successful_load_and_resume(self):
        for case in fixture.RECOVERABLE_CASES:
            values = fixture.expected(case)
            self.assertEqual(fixture.evaluate(case, values), [])
            self.assertEqual(values[-5:], [0x2000, 1, 0, 1, 0x3000])

    def test_missing_rf_and_live_df_are_independent_failures(self):
        values = fixture.expected(0)
        values[10] &= ~0x10000
        values[11] = 1
        self.assertEqual(fixture.evaluate(0, values), ["scalar_10", "scalar_11"])

    def test_breakpoint_must_not_gain_fault_resume_flag(self):
        values = fixture.expected(4)
        values[10] |= 0x10000
        self.assertEqual(fixture.evaluate(4, values), ["scalar_10"])

    def test_truncated_or_failed_load_is_rejected(self):
        self.assertEqual(fixture.evaluate(1, []), ["missing_results"])
        values = fixture.expected(1)
        values[-4] = 0
        self.assertTrue(fixture.evaluate(1, values))

    def test_unhandled_initializer_requires_failed_load_and_caller_continuation(self):
        values = fixture.expected(5)
        self.assertEqual(values, [0x1005, 0x4005, 0x2000, 0, 1114, 0x3000])
        self.assertEqual(fixture.evaluate(5, values), [])
        self.assertEqual(fixture.evaluate(5, values[:2]), ["missing_results"])


if __name__ == "__main__":
    unittest.main()
