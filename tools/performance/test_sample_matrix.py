import copy
import argparse
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from sample_matrix import milestones, run


class CampaignTests(unittest.TestCase):
    def test_multiple_samples_keep_review_evidence_separate_from_failure_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game = root / "game" / "fixture.exe"
            game.parent.mkdir()
            game.write_bytes(b"Not executed: subprocess is mocked")
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [
                {"id": name, "exe": str(game)} for name in ("one", "two")
            ]}), encoding="utf-8")

            def launch(command, **kwargs):
                report = Path(command[command.index("--report") + 1])
                report.write_text(json.dumps({"frames": 1, "startup": {"loaded": True, "guest_windows": 1},
                                               "api": {"mode": "discovery"}}), encoding="utf-8")
                return SimpleNamespace(wait=lambda timeout: 0)

            args = argparse.Namespace(manifest=manifest, only=None, evidence=None, binary=game,
                                      output=root / "output", seconds=1, timeout_grace=1,
                                      mode="hidden", api_mode="discovery", codec="native", trace=False)
            with patch("sample_matrix.subprocess.Popen", side_effect=launch):
                run(args)
            result = json.loads((args.output / "results.json").read_text(encoding="utf-8"))
            self.assertEqual(len(result["samples"]), 2)
            self.assertTrue(all(row["milestones"]["stage_reached"] == "S1" for row in result["samples"]))
            self.assertTrue(all(row["milestones"]["accepted_stage"] is None for row in result["samples"]))


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
