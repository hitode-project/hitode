"""Test scalar activation-query decoding/contracts without Windows execution."""
import struct
import unittest

from query_actctx import ERROR, FIELDS, HEADER, SENTINEL, decode, evaluate, expected_default


def row(cls=1, **values):
    result = dict(zip(FIELDS, [0] * 25))
    result.update({"class": cls, "buffer_bytes": 256, "has_buffer": 1,
                   "has_required": 1, **values})
    return result


class QueryContracts(unittest.TestCase):
    def test_sizes_and_detailed_probe_quirk(self):
        for cls, size in ((1, 8), (2, 48), (5, 12), (6, 0)):
            self.assertEqual(expected_default(row(cls)), (1, ERROR, size))
        self.assertEqual(expected_default(row(2, buffer_bytes=0, has_buffer=0)), (0, 122, 0))
        self.assertEqual(expected_default(row(1, buffer_bytes=0, has_buffer=0)), (0, 122, 8))
        self.assertEqual(expected_default(row(6, buffer_bytes=0, has_buffer=0)), (1, ERROR, 0))

    def test_flags_optional_required_and_subinstance(self):
        self.assertEqual(expected_default(row(2, flags=0x80000000)), (0, 87, 0))
        self.assertEqual(expected_default(row(5, flags=0x80000000)), (1, ERROR, 12))
        self.assertEqual(expected_default(row(1, flags=4, handle_kind=2)), (0, 87, 0))
        self.assertEqual(expected_default(row(1, flags=8)), (0, 87, 0))
        self.assertEqual(expected_default(row(1, flags=8, handle_kind=2)), (1, ERROR, 8))
        self.assertEqual(expected_default(row(1, has_required=0)), (1, ERROR, SENTINEL))
        self.assertEqual(expected_default(row(5, subkind=1)), (0, 87, 0))
        self.assertEqual(expected_default(row(1, subkind=1)), (1, ERROR, 8))

    def test_decoder_rejects_partial_extra_and_reordered_output(self):
        header = [0] * 16
        header[0], header[15] = 1, 118
        words = header + sum(([index] + [0] * 24 for index in range(118)), [])
        raw = struct.pack(f"<{len(words)}I", *words)
        self.assertEqual(len(decode(raw)["rows"]), 118)
        for malformed in (b"", raw[:-4], raw + b"\0", raw + b"\0" * 4):
            with self.assertRaises(ValueError):
                decode(malformed)
        words[16] = 1
        with self.assertRaises(ValueError):
            decode(struct.pack(f"<{len(words)}I", *words))

    def test_manifest_control_requires_nonempty_information_despite_null_current(self):
        header = dict(zip(HEADER, [1, 10, 0, 26300, 1, 1, 0, ERROR, 8, 48, 12, 1, 1, 1, 1, 118]))
        rows = [row() for _ in range(118)]
        rows[2] = row(2, required=436, success=0, error=122, unchanged=1)
        rows[6] = row(6, success=1, required=8, unchanged=0, tail_unchanged=1)
        rows[-1] = row(2, required=436)
        record = {"header": header, "rows": rows}
        self.assertEqual(evaluate(record, True), [])
        rows[2]["required"] = 48
        self.assertEqual(evaluate(record, True), ["manifest_detailed_scope"])
        rows[2]["required"] = 436
        rows[6]["required"] = 0
        self.assertEqual(evaluate(record, True), ["manifest_compatibility_scope"])
        rows[6]["required"] = 8
        rows[2].update(required=200, success=1, error=ERROR, unchanged=0)
        self.assertEqual(evaluate(record, True), [])


if __name__ == "__main__":
    unittest.main()
