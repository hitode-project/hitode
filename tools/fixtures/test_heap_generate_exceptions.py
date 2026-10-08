"""Tests for heap exception payload, preservation and continuation evaluation."""
import unittest

from heap_generate_exceptions import evaluate, expected


class Contracts(unittest.TestCase):
    def test_all_four_fronts_have_success_null_raise_and_continue_controls(self):
        for case in range(20):
            with self.subTest(case=case):
                self.assertEqual(evaluate(case, expected(case)), [])

    def test_missing_is_not_an_empty_success(self):
        self.assertEqual(evaluate(0, None), ["missing_results"])
        self.assertIn("exception_code", evaluate(2, {}))

    def test_failure_is_continuable_and_size_is_payload(self):
        row = expected(2)
        row["exception_flags"] = 1
        row["argument0"] = 0
        self.assertEqual(evaluate(2, row), ["exception_flags", "argument0"])

    def test_continued_oom_cannot_be_success(self):
        row = expected(19)
        row["result"] = 1
        self.assertEqual(evaluate(19, row), ["result"])

    def test_realloc_original_and_zero_fill_are_required(self):
        row = expected(12)
        row["original_ok"] = row["zero_ok"] = 0
        self.assertEqual(evaluate(12, row), ["original_ok", "zero_ok"])

    def test_last_error_is_observed_but_not_part_of_the_heap_contract(self):
        row = expected(2)
        row["last_error"] = 123
        self.assertEqual(evaluate(2, row), [])

    def test_case_range_is_bounded(self):
        for case in (-1, 20):
            with self.assertRaises(ValueError):
                expected(case)


if __name__ == "__main__":
    unittest.main()
