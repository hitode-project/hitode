"""Native ABI coverage for the standalone version-condition acceptance tool."""
import ctypes
import importlib.util
import os
from pathlib import Path
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "version_condition_probe.py"
SPEC = importlib.util.spec_from_file_location("version_condition_probe", SCRIPT)
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class NativeVersionConditions(unittest.TestCase):
    def test_fixed_windows_layout_is_independent_of_host_wchar_size(self):
        self.assertEqual(ctypes.sizeof(PROBE.VersionInfo), 284)
        self.assertEqual(PROBE.VersionInfo.sp_major.offset, 276)
        self.assertEqual(PROBE.VersionInfo.product.offset, 282)

    @unittest.skipUnless(os.name == "nt", "requires native Windows version APIs")
    def test_every_mixed_operator_case_matches_native_result_and_last_error(self):
        report = PROBE.probe(max_examples=0)
        self.assertEqual(report["tested"], 50_625)
        self.assertEqual(report["mismatch_count"], 0, report)
        self.assertEqual(report["first_mismatches"], [])

    @unittest.skipUnless(os.name == "nt", "requires native Windows version APIs")
    def test_bad_model_still_fails_when_example_capture_is_disabled(self):
        # Negative control: failure accounting must not depend on storing a
        # mismatch example. An all-success model contradicts native comparisons.
        with mock.patch.object(PROBE, "hierarchy_matches", return_value=True):
            report = PROBE.probe(max_examples=0)
        self.assertEqual(report["tested"], 50_625)
        self.assertGreater(report["mismatch_count"], 0)
        self.assertEqual(report["first_mismatches"], [])


if __name__ == "__main__":
    unittest.main()
