"""Tests for acceptance.py: python -m unittest discover -s tools/cpu -p test_acceptance.py"""

import tempfile
import unittest
from pathlib import Path

import acceptance

PASSING = """\
running 3 tests
test a ... ok
test b ... ok
test c ... ignored
test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
"""

STRICT = """\
test calls_returns_and_indirect_targets_stay_native ... FAILED
test brand_new_regression ... FAILED
test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
"""

CRASHED = """\
error: test failed, to rerun pass `-p adv-cpu --lib`

Caused by:
  process didn't exit successfully: `/w/target/release/deps/adv_cpu-5fc6a7cc11a9b1d9` (signal: 11, SIGSEGV: invalid memory reference)
"""

BASELINE = {"calls_returns_and_indirect_targets_stay_native"}


class AcceptanceTest(unittest.TestCase):
    def judge(self, texts, strict):
        return acceptance.judge(acceptance.summarize(texts), strict, BASELINE)

    def test_clean_run_is_accepted(self):
        result = self.judge([PASSING], strict=False)
        self.assertEqual((result["passed"], result["ignored"]), (2, 1))
        self.assertTrue(result["accepted"])

    def test_strict_separates_baseline_from_new_failures(self):
        result = self.judge([PASSING, STRICT], strict=True)
        self.assertEqual(result["new_failures"], ["brand_new_regression"])
        self.assertFalse(result["accepted"])
        only_known = STRICT.replace("test brand_new_regression ... FAILED\n", "")
        self.assertTrue(self.judge([only_known], strict=True)["accepted"])

    def test_baseline_failures_are_new_without_strict(self):
        result = self.judge([STRICT], strict=False)
        self.assertEqual(len(result["new_failures"]), 2)

    def test_crashed_process_rejects_even_with_zero_failures(self):
        result = self.judge([PASSING, CRASHED], strict=False)
        self.assertEqual(result["crashed"], ["adv_cpu"])
        self.assertEqual(result["failed"], 0)
        self.assertFalse(result["accepted"])

    def test_device_signal_exit_counts_as_crash(self):
        text = "Device test failed (139): adv_cpu; evidence: out\n"
        self.assertEqual(self.judge([PASSING, text], False)["crashed"], ["adv_cpu"])
        text = "Device test failed (101): adv_cpu; evidence: out\n"
        self.assertEqual(self.judge([PASSING, text], False)["crashed"], [])

    def test_tier_verification_lines_reject(self):
        text = PASSING + "tier verification: mismatch\n"
        self.assertFalse(self.judge([text], strict=True)["accepted"])

    def test_missing_results_are_not_accepted(self):
        self.assertFalse(self.judge(["no tests here"], strict=False)["accepted"])

    def test_directory_input_and_baseline_file(self):
        with tempfile.TemporaryDirectory() as root:
            Path(root, "one.log").write_text(PASSING, encoding="utf-8")
            Path(root, "nested").mkdir()
            Path(root, "nested", "two.txt").write_text(STRICT, encoding="utf-8")
            baseline = Path(root, "baseline")
            baseline.write_text(
                "# comment\ncalls_returns_and_indirect_targets_stay_native\n",
                encoding="utf-8",
            )
            args = ["--strict", "--baseline", str(baseline), root]
            self.assertEqual(acceptance.main(args), 1)

    def test_shipped_baseline_parses(self):
        names = acceptance.load_baseline(acceptance.DEFAULT_BASELINE)
        self.assertGreater(len(names), 20)
        self.assertFalse(any(name.startswith("#") for name in names))


if __name__ == "__main__":
    unittest.main()
