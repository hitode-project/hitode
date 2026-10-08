"""Unit tests for the authored RTL caller-address fixture's scalar contracts."""
import unittest

from rtl_raise_address import evaluate


class Contracts(unittest.TestCase):
    def setUp(self):
        self.values = [1] * 20
        self.values[17] = 3

    def test_complete(self):
        self.assertEqual(evaluate(self.values), [])

    def test_missing(self):
        self.assertEqual(evaluate([]), ["missing_results"])

    def test_hook_return_is_required(self):
        self.values[16] = 0
        self.assertEqual(evaluate(self.values), ["hook_returned"])

    def test_record_must_be_overwritten(self):
        self.values[3] = self.values[7] = 0
        self.assertEqual(evaluate(self.values), ["first_record", "second_record"])


if __name__ == "__main__":
    unittest.main()
