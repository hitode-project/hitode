"""Synthetic reconciliation, correlation and privacy tests for compile_ir.py."""
import copy
import json
import unittest

from compile_ir import PREFIX, aggregate, validated


def request(ir=2, service=10, strategy="fast"):
    return dict(version=1, strategy=strategy, guest=1, ir=ir, blocks=1, functions=1,
                native_bytes=ir*8, service_us=service,
                rows=[["Mov", "int.mem", "check", ir, 1]], decoded=[["Mov", "int.mem", 1]])


class CompileIrTests(unittest.TestCase):
    def test_strategy_histograms_and_linear_correlation(self):
        rows = [request(2, 10), request(4, 20), request(8, 40), request(strategy="speed")]
        result = aggregate([["ignored line\n"] + [PREFIX+json.dumps(row) for row in rows]])
        self.assertEqual(result["fast"]["ir"]["sum"], 14)
        self.assertEqual(result["fast"]["ir_service_pearson_r"], 1)
        self.assertIsNone(result["speed"]["ir_service_pearson_r"])
        self.assertEqual(result["fast"]["service_by_ir"][3]["sum"], 20)
        self.assertEqual(result["fast"]["rows"]["Mov/int.mem/check"]["blocks"], 3)

    def test_invalid_counters_and_totals(self):
        for key, value in [("ir", 0), ("guest", 2), ("blocks", 3), ("service_us", -1),
                           ("functions", True), ("native_bytes", 1 << 64)]:
            raw = request()
            raw[key] = value
            with self.assertRaises(ValueError):
                validated(raw)

    def test_private_labels_and_unknown_fields_are_rejected(self):
        for label in ["some/path", "0x12345678", "12345678"]:
            raw = request()
            raw["rows"][0][0] = label
            with self.assertRaises(ValueError):
                validated(raw)
        raw = request()
        raw["address"] = 10
        with self.assertRaises(ValueError):
            validated(raw)

    def test_duplicate_rows_and_bad_shapes(self):
        for key in ["rows", "decoded"]:
            raw = request()
            raw[key].append(copy.deepcopy(raw[key][0]))
            with self.assertRaises(ValueError):
                validated(raw)
        self.assertEqual(aggregate([["unrelated"]]), {})


if __name__ == "__main__":
    unittest.main()
