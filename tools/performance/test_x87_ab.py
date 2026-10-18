import csv
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import x87_ab as ab


def document(mode="helper", change=None):
    rows = []
    for name, operations in ab.WORKLOADS.items():
        rows.append(dict(zip(ab.FIELDS, [mode, name, 31, 20000, operations,
                                        20 if mode == "helper" else 10,
                                        22, 30, 4, 5000, "0123456789abcdef"])))
    if change:
        change(rows)
    out = io.StringIO()
    writer = csv.DictWriter(out, fieldnames=ab.FIELDS)
    writer.writeheader()
    writer.writerows(rows)
    return out.getvalue()


class X87ComparisonTests(unittest.TestCase):
    def test_complete_pair_and_explicit_aggregation(self):
        helper, native = ab.parse(document(), "helper"), ab.parse(document("native"), "native")
        summary = ab.summarize([(helper, native), (helper, native)])
        self.assertEqual(summary["acceptance"], "synthetic-only")
        self.assertEqual(len(summary["workloads"]), 7)
        self.assertTrue(all(row["speedup_median"] == 2 for row in summary["workloads"]))

    def test_rejects_wrong_mode_missing_duplicate_and_unknown_workload(self):
        edits = [lambda r: r.pop(), lambda r: r.append(r[0]),
                 lambda r: r[0].update(workload="unknown"),
                 lambda r: r[0].update(mode="native")]
        for edit in edits:
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                ab.parse(document(change=edit), "helper")
        for text in ("", "a,b\n1,2\n", document() + "extra,row,too,short\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                ab.parse(text, "helper")

    def test_rejects_nonfinite_nonpositive_and_unordered_measurements(self):
        for key in ab.TIMINGS:
            for value in ("NaN", "inf", "-inf", "0", "-1", "invalid"):
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    ab.parse(document(change=lambda r: r[0].update({key: value})), "helper")
        for key in ab.INTEGERS:
            for value in (0, -1, "NaN", "1.5"):
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    ab.parse(document(change=lambda r: r[0].update({key: value})), "helper")
        for edit in ({"p95_ns_per_op": 19}, {"max_ns_per_op": 21}, {"x87_ops": 99},
                     {"checksum": "oops"}, {"checksum": "0123456789abcdef0"}):
            with self.subTest(edit=edit), self.assertRaises(ValueError):
                ab.parse(document(change=lambda r: r[0].update(edit)), "helper")

    def test_rejects_mismatched_work_counts_checksums_and_rounds(self):
        helper = ab.parse(document(), "helper")
        for field, value in [("samples", 32), ("iterations", 1),
                             ("checksum", "fedcba9876543210")]:
            changed = ab.parse(document("native", lambda r: r[0].update({field: value})), "native")
            with self.subTest(field=field), self.assertRaises(ValueError):
                ab.compare(helper, changed)
        changed_h = ab.parse(document(change=lambda r: r[0].update(checksum="fedcba9876543210")), "helper")
        changed_n = ab.parse(document("native", lambda r: r[0].update(checksum="fedcba9876543210")), "native")
        native = ab.parse(document("native"), "native")
        with self.assertRaises(ValueError):
            ab.summarize([(helper, native), (changed_h, changed_n)])
        with self.assertRaises(ValueError):
            ab.summarize([])

    def test_runner_alternates_modes_and_overrides_inherited_environment(self):
        modes = []
        def invoke(command, **kwargs):
            mode = "helper" if kwargs["env"].get("ADV_JIT_X87_HELPER_ONLY") == "1" else "native"
            modes.append(mode)
            self.assertEqual(command[-1], "31")
            return subprocess.CompletedProcess(command, 0, document(mode), "")
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root) / "bench"
            binary.touch()
            output = Path(root) / "results"
            with patch.dict("os.environ", {"ADV_JIT_X87_HELPER_ONLY": "1"}), patch.object(ab.subprocess, "run", side_effect=invoke):
                ab.run(binary, output, 3, 31)
            self.assertEqual(modes, ["helper", "native", "native", "helper", "helper", "native"])
            self.assertEqual(len(list(output.glob("*.csv"))), 6)
            self.assertEqual(json.loads((output / "summary.json").read_text())["workloads"][0]["rounds"], 3)

    def test_failed_process_cannot_leave_a_stale_success_summary(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            binary = directory / "bench"
            binary.touch()
            summary = directory / "summary.json"
            summary.write_text("old success")
            result = subprocess.CompletedProcess([], 1, "partial", "failure")
            with patch.object(ab.subprocess, "run", return_value=result), self.assertRaises(RuntimeError):
                ab.run(binary, directory, 2)
            self.assertFalse(summary.exists())
            self.assertEqual((directory / "round-01-helper.stderr.txt").read_text(), "failure")


if __name__ == "__main__":
    unittest.main()
