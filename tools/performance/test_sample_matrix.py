import copy
import unittest

from sample_matrix import milestones


class MilestoneTests(unittest.TestCase):
    def setUp(self):
        self.report = {"api": {"mode": "strict", "acceptance_eligible": True,
                               "stub_calls": 0, "signature_conflicts": []},
                       "startup": {"loaded": True, "guest_windows": 1}, "frames": 120}

    def test_frames_do_not_certify_title_or_sound(self):
        self.assertEqual(milestones(self.report)["accepted_stage"], "S1")

    def test_discovery_cannot_pass_even_without_called_stubs(self):
        self.report["api"]["mode"] = "discovery"
        result = milestones(self.report)
        self.assertEqual(result["stage_reached"], "S1")
        self.assertIsNone(result["accepted_stage"])

    def test_failure_stub_or_conflict_disqualifies(self):
        for path, value in (("stub_calls", 1), ("signature_conflicts", ["bad stack"]),
                            ("acceptance_eligible", False)):
            report = copy.deepcopy(self.report)
            report["api"][path] = value
            self.assertIsNone(milestones(report)["accepted_stage"])
        self.report["failure"] = "fetch at zero"
        self.assertIsNone(milestones(self.report)["accepted_stage"])

    def test_review_is_bound_to_report(self):
        evidence = {"stage": "S4", "report_sha256": "digest", "reviewer": "tester",
                    "note": "Completed the chapter with correct image and sound."}
        self.assertEqual(milestones(self.report, evidence, "digest")["accepted_stage"], "S4")
        with self.assertRaises(ValueError):
            milestones(self.report, evidence, "another build")
        self.report["api"]["mode"] = "discovery"
        self.assertIsNone(milestones(self.report, evidence, "digest")["accepted_stage"])

    def test_old_reports_and_missing_windows_are_not_passes(self):
        self.assertIsNone(milestones({"frames": 120})["accepted_stage"])
        self.report["startup"]["guest_windows"] = 0
        self.assertIsNone(milestones(self.report)["stage_reached"])


if __name__ == "__main__":
    unittest.main()
