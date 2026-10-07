"""Synthetic report tests: python -m unittest discover -s tools/survey -p test_cliffs.py.

Input fixtures are generated in memory or under CARGO_TARGET_DIR (default target).
No real reports are used. Output is unittest's assertion summary.
"""
from contextlib import redirect_stderr, redirect_stdout
import copy
import io
import json
import os
from pathlib import Path
import tempfile
import unittest

from cliffs import aggregate, empty_census, main, markdown, read_reports


def fixture():
    return {
        "cpu_cliffs": {
            "x87_helper_calls": 20,
            "x87_fast": 5,
            "x87_exact": {"Fadd/Control": 8, "Fadd/Operand": 3, "Fsin/NoFastForm": 4},
            "fallbacks": {"Movsb": 7, "Cpuid": 2},
            "interpreter": {"Fallback": {"entries": 9, "instructions": 30},
                            "Pending": {"entries": 3, "instructions": 90}},
        },
        "tier_verification": {"verified": 11, "inconclusive": 2, "mismatches": 1,
                              "mxcsr_flags": 3, "reports": ["private-detail-marker"]},
        "exe": "private-executable-marker",
        "elapsed_seconds": 123.5,
        "loaded_modules": [{"base": "private-address-marker"}],
    }


def empty_fixture():
    return {
        "cpu_cliffs": {"x87_helper_calls": 0, "x87_fast": 0, "x87_exact": {},
                       "fallbacks": {}, "interpreter": {}},
        "tier_verification": dict.fromkeys(("verified", "inconclusive", "mismatches", "mxcsr_flags"), 0),
    }


class AggregateTests(unittest.TestCase):
    def census_fixture(self):
        report = empty_fixture()
        c = report["cpu_cliffs"]
        c["interpreter"] = {"Uncovered": {"entries": 2, "instructions": 20}}
        c["tier1_census"] = empty_census()
        row = c["tier1_census"]["PendingCompilation"]
        row.update(entries=2, instructions=4, supported_units=18, unsupported_units=2,
                   prefix_units=17, unsupported={"Fld1": 2})
        row["blocked_prefixes"][1] = 1
        row["censored_prefixes"][1] = 1
        return report

    def test_census_merges_rep_units_prefixes_and_unknown_reports(self):
        result = aggregate([self.census_fixture(), empty_fixture(), self.census_fixture()])
        cliffs = result["cpu_cliffs"]
        self.assertEqual(cliffs["tier1_census_reports"], 2)
        row = cliffs["tier1_census"]["PendingCompilation"]
        self.assertEqual(row["supported_units"], 36)
        self.assertEqual(row["unsupported"], {"Fld1": 4})
        self.assertEqual(row["blocked_prefixes"][1], 2)
        self.assertNotIn("private", markdown(result))

    def test_census_and_pc64_distributions_coexist(self):
        report = fixture()
        report["cpu_cliffs"].update({
            key: value for key, value in self.census_fixture()["cpu_cliffs"].items()
            if key in ("interpreter", "tier1_census")})
        report["cpu_cliffs"].update(
            x87_control={"PC=64/RC=nearest/masks=3f": 8},
            x87_control_rejections={"PC": 8, "RC": 0, "Masks": 0},
            x87_pc64={"Fadd/Exact": 3, "Fadd/Inexact": 5})
        counters = aggregate([report])["cpu_cliffs"]
        self.assertEqual(counters["x87_pc64"], {"Fadd/Inexact": 5, "Fadd/Exact": 3})
        self.assertEqual(counters["tier1_census"]["PendingCompilation"]["supported_units"], 18)

    def test_census_truncation_and_bucket_reconciliation(self):
        report = self.census_fixture()
        row = report["cpu_cliffs"]["tier1_census"]["PendingCompilation"]
        row.update(unsupported={f"Form{n}_rm32": 1 for n in range(24)},
                   unsupported_units=27, omitted_rows=2, omitted_units=3)
        report["cpu_cliffs"]["interpreter"]["Uncovered"]["instructions"] = 45
        self.assertEqual(aggregate([report])["cpu_cliffs"]["tier1_census"]["PendingCompilation"]["omitted_units"], 3)
        row["omitted_units"] = 2
        with self.assertRaisesRegex(ValueError, "reconcile"):
            aggregate([report])

    def test_census_rejects_private_labels_bad_histograms_and_partial_fields(self):
        for key, value in (("unsupported", {"Mov_0x1234": 2}),
                           ("unsupported", {"private/path": 2}),
                           ("blocked_prefixes", [True] * 8),
                           ("censored_prefixes", [0]),
                           ("entries", 3), ("instructions", 100),
                           ("prefix_units", 19), ("omitted_rows", 1)):
            report = self.census_fixture()
            report["cpu_cliffs"]["tier1_census"]["PendingCompilation"][key] = value
            with self.assertRaises(ValueError) as caught:
                aggregate([report])
            self.assertNotIn("private", str(caught.exception))
        report = self.census_fixture()
        del report["cpu_cliffs"]["tier1_census"]["Rejected"]
        with self.assertRaises(ValueError):
            aggregate([report])

    def detailed_fixture(self):
        report = empty_fixture()
        report["cpu_cliffs"].update(
            interpreter={"Rejected": {"entries": 2, "instructions": 11}},
            interpreter_details={"Rejected/Unsupported/Cbw/Cbw": {"entries": 2, "instructions": 11}},
            interpreter_details_omitted={reason: {"entries": 0, "instructions": 0}
                                         for reason in ("Uncovered", "Rejected")},
            interpreter_detail_rows_omitted=0)
        return report

    def test_details_merge_and_old_reports_remain_explicitly_unknown(self):
        report = self.detailed_fixture()
        old = empty_fixture()
        old["cpu_cliffs"]["interpreter"] = {"Uncovered": {"entries": 3, "instructions": 20}}
        result = aggregate([report, report, old])
        self.assertEqual(result["cpu_cliffs"]["interpreter_details"]["Rejected/Unsupported/Cbw/Cbw"],
                         {"entries": 4, "instructions": 22})
        self.assertEqual(result["cpu_cliffs"]["interpreter_details_unknown"]["Uncovered"],
                         {"entries": 3, "instructions": 20})
        self.assertEqual(result["report_limits"]["interpreter_detail_reports"], 2)

    def test_iced_forms_can_suffix_the_mnemonic_with_operand_size(self):
        for label in ("Uncovered/Unsupported/Popa/Popaw", "Rejected/Unsupported/Ret/Retnd"):
            report = self.detailed_fixture()
            bucket = label.split("/")[0]
            report["cpu_cliffs"]["interpreter"] = {bucket: {"entries": 2, "instructions": 11}}
            report["cpu_cliffs"]["interpreter_details"] = {label: {"entries": 2, "instructions": 11}}
            self.assertIn(label, aggregate([report])["cpu_cliffs"]["interpreter_details"])

    def test_detail_truncation_reconciles_both_counters(self):
        report = self.detailed_fixture()
        cliffs = report["cpu_cliffs"]
        cliffs["interpreter_details"] = {
            f"Rejected/Unsupported/Op{i}/Op{i}_rm32": {"entries": 1, "instructions": 3}
            for i in range(24)}
        cliffs["interpreter_details_omitted"]["Rejected"] = {"entries": 5, "instructions": 7}
        cliffs["interpreter_detail_rows_omitted"] = 2
        cliffs["interpreter"]["Rejected"] = {"entries": 29, "instructions": 79}
        result = aggregate([report])
        self.assertEqual(result["cpu_cliffs"]["interpreter_details_omitted"]["Rejected"]["instructions"], 7)
        self.assertIn("源报告截断行数：2", markdown(result))
        cliffs["interpreter"]["Rejected"]["entries"] += 1
        with self.assertRaisesRegex(ValueError, "reconcile"):
            aggregate([report])

    def test_detail_schema_rejects_partial_or_private_labels(self):
        for label in ("Rejected/UnknownReason", "Uncovered/private/path", "Rejected/Unsupported/Mov/private",
                      "Rejected/Unsupported/Mov/Mov_0x1234-operand", "Rejected/Unsupported/Mov/Mov_0x1234",
                      "Rejected/Unsupported/Mov/Mov_12345678", "Forced/BelowThreshold"):
            report = self.detailed_fixture()
            report["cpu_cliffs"]["interpreter_details"] = {label: {"entries": 2, "instructions": 11}}
            with self.assertRaises(ValueError) as caught:
                aggregate([report])
            self.assertNotIn("private", str(caught.exception))
        for key in ("interpreter_details", "interpreter_details_omitted", "interpreter_detail_rows_omitted"):
            report = self.detailed_fixture()
            del report["cpu_cliffs"][key]
            with self.assertRaises(ValueError):
                aggregate([report])

    def test_detail_limits_and_counter_types_are_checked(self):
        for key, bad in (("interpreter_detail_rows_omitted", True),
                         ("interpreter_detail_rows_omitted", 1),
                         ("interpreter_details", []),
                         ("interpreter_details_omitted", {})):
            report = self.detailed_fixture()
            report["cpu_cliffs"][key] = bad
            with self.assertRaises(ValueError):
                aggregate([report])

    def test_pc64_categories_are_complete_and_optional(self):
        report = fixture()
        report["cpu_cliffs"].update({
            "x87_control": {"PC=64/RC=nearest/masks=3f": 8},
            "x87_control_rejections": {"PC": 8, "RC": 0, "Masks": 0},
            "x87_pc64": {"Fadd/Exact": 2, "Fadd/Inexact": 6},
        })
        summary = aggregate([report, report, fixture()])
        self.assertEqual(summary["cpu_cliffs"]["x87_pc64_reports"], 2)
        self.assertEqual(summary["cpu_cliffs"]["x87_pc64"], {"Fadd/Inexact": 12, "Fadd/Exact": 4})
        self.assertIn("PC=64 候选细分覆盖 2", markdown(summary))
        for invalid in ({"Fadd/Exact": 7}, {"Fadd/Exact": 9}, {"Fadd/Exact": True},
                        {"private/path/Exact": 8}, {"Fadd/private-marker": 8}, {42: 8}):
            report["cpu_cliffs"]["x87_pc64"] = invalid
            with self.assertRaises(ValueError) as caught:
                aggregate([report])
            self.assertNotIn("private", str(caught.exception))

    def test_control_distribution_is_complete_and_guards_overlap(self):
        report = fixture()
        report["cpu_cliffs"]["x87_control"] = {
            "PC=64/RC=nearest/masks=3f": 5,
            "PC=64/RC=zero/masks=32": 3,
        }
        report["cpu_cliffs"]["x87_control_rejections"] = {"PC": 8, "RC": 3, "Masks": 3}
        summary = aggregate([report, report, fixture()])
        counters = summary["cpu_cliffs"]
        self.assertEqual(counters["x87_control_reports"], 2)
        self.assertEqual(counters["x87_control"]["PC=64/RC=nearest/masks=3f"], 10)
        self.assertEqual(counters["x87_control_rejections"], {"PC": 16, "RC": 6, "Masks": 6})
        self.assertIn("Control 细分覆盖 2", markdown(summary))

    def test_control_and_entry_details_reconcile_in_the_same_report(self):
        report = fixture()
        report["cpu_cliffs"].update({key: value for key, value in self.detailed_fixture()["cpu_cliffs"].items()
                                     if key.startswith("interpreter")})
        report["cpu_cliffs"]["x87_control"] = {"PC=64/RC=nearest/masks=3f": 8}
        report["cpu_cliffs"]["x87_control_rejections"] = {"PC": 8, "RC": 0, "Masks": 0}
        report["cpu_cliffs"]["x87_pc64"] = {"Fadd/Exact": 3, "Fadd/Inexact": 5}
        summary = aggregate([report])
        self.assertEqual(summary["cpu_cliffs"]["x87_pc64"], {"Fadd/Inexact": 5, "Fadd/Exact": 3})
        self.assertEqual(summary["cpu_cliffs"]["x87_control_rejections"]["PC"], 8)
        self.assertEqual(summary["cpu_cliffs"]["interpreter_details"]["Rejected/Unsupported/Cbw/Cbw"],
                         {"entries": 2, "instructions": 11})
        self.assertIn("Control 细分覆盖 1", markdown(summary))
        self.assertIn("Rejected/Unsupported/Cbw/Cbw", markdown(summary))

    def test_control_distribution_rejects_private_labels_and_inconsistent_counts(self):
        for label, value, guards in [
            (42, 8, {"PC": 8, "RC": 0, "Masks": 0}),
            ("private-detail-marker", 8, {"PC": 8, "RC": 0, "Masks": 0}),
            ("PC=64/RC=nearest/masks=3f", 8, {"PC": 7, "RC": 0, "Masks": 0}),
            ("PC=64/RC=nearest/masks=3f", 16, {"PC": 16, "RC": 0, "Masks": 0}),
            ("PC=64/RC=nearest/masks=3f", 7, {"PC": 7, "RC": 0, "Masks": 0}),
        ]:
            report = fixture()
            report["cpu_cliffs"]["x87_control"] = {label: value}
            report["cpu_cliffs"]["x87_control_rejections"] = guards
            with self.assertRaises(ValueError) as caught:
                aggregate([report])
            self.assertNotIn("private-detail-marker", str(caught.exception))

    def test_merges_mnemonics_reasons_and_verification(self):
        first = fixture()
        second = fixture()
        second["cpu_cliffs"]["fallbacks"] = {"Cpuid": 12, "Int": 1}
        second["cpu_cliffs"]["x87_exact"] = {"Fadd/Pending": 6, "Fmul/Operand": 9}
        second["tier_verification"]["verified"] = 20
        before = copy.deepcopy([first, second])
        summary = aggregate([first, second])
        self.assertEqual([first, second], before)
        self.assertEqual(summary["report_count"], 2)
        cliffs = summary["cpu_cliffs"]
        self.assertEqual(list(cliffs["fallbacks"].items()), [("Cpuid", 14), ("Movsb", 7), ("Int", 1)])
        self.assertEqual(cliffs["fallbacks_retained_total"], 22)
        self.assertEqual(cliffs["x87_exact_by_mnemonic"], {"Fadd": 17, "Fmul": 9, "Fsin": 4})
        self.assertEqual(cliffs["x87_exact_by_reason"], {"Control": 8, "Operand": 12,
                                                       "Pending": 6, "NoFastForm": 4})
        self.assertEqual(cliffs["x87_helper_calls"], 40)
        self.assertEqual(cliffs["x87_fast"], 10)
        self.assertEqual(cliffs["x87_exact_total"], 30)
        self.assertEqual(cliffs["x87_exact_unattributed"], 0)
        self.assertEqual(cliffs["interpreter_total"], {"entries": 24, "instructions": 240})
        self.assertEqual(list(cliffs["interpreter"])[0], "Pending")
        self.assertEqual(cliffs["interpreter"]["Fallback"], {"entries": 18, "instructions": 60})
        self.assertEqual(summary["tier_verification"], {"verified": 31, "inconclusive": 4,
                                                       "mismatches": 2, "mxcsr_flags": 6})
        self.assertEqual(summary["per_report"][1], {"report": 2, "tier_verification": {
            "verified": 20, "inconclusive": 2, "mismatches": 1, "mxcsr_flags": 3}})

    def test_ties_are_stable_and_json_keeps_more_than_top20(self):
        report = empty_fixture()
        report["cpu_cliffs"]["fallbacks"] = {f"Op{i:02}": 1 for i in reversed(range(25))}
        summary = aggregate([report])
        self.assertEqual(list(summary["cpu_cliffs"]["fallbacks"]), [f"Op{i:02}" for i in range(25)])
        rendered = markdown(summary)
        self.assertIn("| Op19 |", rendered)
        self.assertNotIn("| Op20 |", rendered)
        self.assertEqual(summary["report_limits"]["fallback_reports_at_limit"], 1)

    def test_truncation_retains_unattributed_exact_counts(self):
        report = empty_fixture()
        report["cpu_cliffs"].update(x87_helper_calls=100, x87_fast=20,
                                     x87_exact={f"Op{i}/Control": 2 for i in range(24)})
        summary = aggregate([report])
        self.assertEqual(summary["cpu_cliffs"]["x87_exact_total"], 80)
        self.assertEqual(summary["cpu_cliffs"]["x87_exact_unattributed"], 32)
        self.assertEqual(summary["cpu_cliffs"]["x87_exact_by_reason"]["Control"], 48)
        self.assertEqual(summary["report_limits"]["x87_exact_reports_at_limit"], 1)

    def test_zero_counters_are_valid_but_missing_fields_are_not_zero(self):
        summary = aggregate([empty_fixture()])
        self.assertEqual(summary["cpu_cliffs"]["x87_exact_total"], 0)
        self.assertEqual(len(summary["cpu_cliffs"]["x87_exact_by_reason"]), 4)
        self.assertEqual(len(summary["cpu_cliffs"]["interpreter"]), 6)
        for section, key in ((None, "cpu_cliffs"), (None, "tier_verification"),
                             ("cpu_cliffs", "x87_fast"), ("cpu_cliffs", "fallbacks"),
                             ("tier_verification", "mxcsr_flags")):
            report = fixture()
            del (report if section is None else report[section])[key]
            with self.subTest(section=section, key=key), self.assertRaises(ValueError):
                aggregate([report])

    def test_rejects_invalid_counter_types(self):
        for bad in (-1, True, 1.5, "2", None, {}, []):
            for section, key in (("cpu_cliffs", "x87_fast"), ("tier_verification", "verified")):
                report = fixture()
                report[section][key] = bad
                with self.subTest(bad=bad, key=key), self.assertRaises(ValueError):
                    aggregate([report])

    def test_rejects_invalid_nested_counts_and_shapes(self):
        for key, value in (("fallbacks", {"Movsb": True}), ("x87_exact", {"Fld/Control": -1}),
                           ("interpreter", {"Forced": {"entries": 1}}),
                           ("interpreter", {"Forced": {"entries": 1, "instructions": 0.5}}),
                           ("fallbacks", []), ("x87_exact", None), ("interpreter", 0)):
            report = fixture()
            report["cpu_cliffs"][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                aggregate([report])
        for report in (None, [], "private-marker"):
            with self.assertRaises(ValueError):
                aggregate([report])

    def test_rejects_inconsistent_x87_totals(self):
        for helpers in (4, 19):
            report = fixture()
            report["cpu_cliffs"]["x87_helper_calls"] = helpers
            with self.assertRaisesRegex(ValueError, "input 2: x87 counters"):
                aggregate([fixture(), report])

    def test_private_fields_do_not_reach_json_or_markdown(self):
        summary = aggregate([fixture()])
        for output in (json.dumps(summary), markdown(summary)):
            for marker in ("private-", "elapsed", "exe", "loaded_modules", "123.5"):
                self.assertNotIn(marker, output)

    def test_invalid_labels_are_not_echoed(self):
        for key, value in (("fallbacks", {"private/path": 1}),
                           ("fallbacks", {"a" * 8: 1}),
                           ("x87_exact", {"Fld/private-reason": 1}),
                           ("x87_exact", {"private/path/Control": 1}),
                           ("interpreter", {"private-reason": {"entries": 1, "instructions": 1}})):
            report = fixture()
            report["cpu_cliffs"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError) as caught:
                aggregate([report])
            self.assertNotIn("private", str(caught.exception))

    def test_large_integer_counters_are_exact(self):
        report = empty_fixture()
        report["cpu_cliffs"]["fallbacks"] = {"Movsb": 2**63}
        self.assertEqual(aggregate([report, report])["cpu_cliffs"]["fallbacks"]["Movsb"], 2**64)

    def test_empty_input_is_not_an_observed_zero(self):
        with self.assertRaisesRegex(ValueError, "at least one report"):
            aggregate([])


class CliTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(os.environ.get("CARGO_TARGET_DIR", Path(__file__).resolve().parents[2] / "target")) / "scratch" / "cliff-survey"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_utf8_bom_and_cli_json(self):
        path = self.root / "private-filename.json"
        path.write_text(json.dumps(fixture()), encoding="utf-8-sig")
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            result = main([str(path), "--json"])
        self.assertEqual(result, 0)
        self.assertEqual(stderr.getvalue(), "")
        self.assertEqual(json.loads(stdout.getvalue()), aggregate([fixture()]))
        self.assertNotIn("private", stdout.getvalue())

    def test_cli_default_tables(self):
        path = self.root / "input.json"
        path.write_text(json.dumps(fixture()), encoding="utf-8")
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            self.assertEqual(main([str(path)]), 0)
        self.assertIn("Native 回退 Top 20", stdout.getvalue())
        self.assertIn("| 1 | 11 | 2 | 1 | 3 |", stdout.getvalue())

    def test_read_failures_hide_paths_and_content(self):
        path = self.root / "private-filename.json"
        for contents in (None, b"private-content", b"\xff", b'{"private-key":1,"private-key":2}'):
            if contents is not None:
                path.write_bytes(contents)
            stdout, stderr = io.StringIO(), io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                result = main([str(path), "--json"])
            self.assertEqual(result, 2)
            self.assertEqual(stdout.getvalue(), "")
            self.assertIn("input 1", stderr.getvalue())
            self.assertNotIn("private", stderr.getvalue())

    def test_late_failure_emits_no_partial_result(self):
        path = self.root / "valid.json"
        path.write_text(json.dumps(fixture()), encoding="utf-8")
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            result = main([str(path), str(self.root / "missing.json")])
        self.assertEqual(result, 2)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("input 2", stderr.getvalue())

    def test_duplicate_counter_keys_are_rejected(self):
        path = self.root / "duplicate.json"
        path.write_text('{"cpu_cliffs":{"fallbacks":{"Movsb":1,"Movsb":2}}}', encoding="utf-8")
        with self.assertRaises(ValueError):
            list(read_reports([path]))


if __name__ == "__main__":
    unittest.main()
