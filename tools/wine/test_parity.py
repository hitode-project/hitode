"""Exercise the stack gate with an authored runner, without guest programs."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import parity


FAKE = r'''
import json
from pathlib import Path
import os
import sys
import time

root = Path(__file__).parent
stack = "wine" if "--wine" in sys.argv else "old"
config = json.loads((root / "config.json").read_text()).get(stack, {})
with (root / "calls.jsonl").open("a") as calls:
    calls.write(json.dumps({"stack": stack, "args": sys.argv[1:],
                            "mute": os.environ.get("ADV_AUDIO_MUTE_DEVICE")}) + "\n")
started = time.process_time()
while time.process_time() - started < config.get("cpu", 0.02):
    pass
if not config.get("missing"):
    frames = config.get("frames", 100)
    code = config.get("guest_code", 0)
    if stack == "old" and not config.get("exit_line"):
        print(f"time_limit: exit={code if code else 'none'} frames={frames} elapsed=40.001s host_calls=10")
    else:
        print(f"{stack}: exit {code} after 40.00s, {frames} frames presented", file=sys.stderr)
sys.exit(config.get("returncode", 0))
'''


class ParityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.runner = self.root / "fake adv-run"
        self.runner.write_text(f"#!{sys.executable}\n" + FAKE)
        self.runner.chmod(0o755)
        self.config = self.root / "config.json"
        self.config.write_text("{}")
        self.numbers = self.root / "numbers.json"
        self.arguments = [
            sys.executable, str(Path(__file__).with_name("parity.py")),
            "--adv-run", str(self.runner), "--wine", str(self.root / "distribution"),
            "--exe", str(self.root / "authored.exe"),
            "--old-prefix", str(self.root / "old prefix"),
            "--old-data-dir", str(self.root / "old data"),
            "--wine-prefix", str(self.root / "wine prefix"),
            "--wine-data-dir", str(self.root / "wine data"),
            "--json", str(self.numbers),
        ]

    def invoke(self, config=None, *options):
        if config is not None:
            self.config.write_text(json.dumps(config))
        return subprocess.run(self.arguments + list(options), capture_output=True, text=True)

    # spec: WINE-TEST-2
    def test_sequential_headless_commands_and_per_child_numbers(self):
        result = self.invoke(None, "--threshold", "1000")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 1)
        self.assertIn("PASS old_cpu/frame=", result.stdout)
        self.assertIn("wine_cpu/frame=", result.stdout)
        self.assertIn("ratio=", result.stdout)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual([call["stack"] for call in calls], ["old", "wine"])
        common = [str(self.root / "authored.exe"), "--headless", "--headless-gpu", "--audio",
                  "--auto-dialog-ms", "1000", "--seconds", "40"]
        self.assertEqual(calls[0]["args"], common + ["--install-dir", str(self.root / "old prefix"),
                                                  "--data-dir", str(self.root / "old data")])
        self.assertEqual(calls[1]["args"], common + ["--wine", str(self.root / "distribution"),
                                                  "--wine-prefix", str(self.root / "wine prefix"),
                                                  "--data-dir", str(self.root / "wine data")])
        self.assertEqual([call["mute"] for call in calls], ["1", "1"])
        numbers = json.loads(self.numbers.read_text())
        self.assertTrue(numbers["passed"])
        self.assertEqual(numbers["threshold_percent"], 1000)
        for stack in ("old", "wine"):
            run = numbers[stack]
            self.assertEqual(run["frames"], 100)
            self.assertGreater(run["user_seconds"], 0)
            self.assertGreaterEqual(run["system_seconds"], 0)
            self.assertAlmostEqual(run["cpu_seconds"], run["user_seconds"] + run["system_seconds"])
            self.assertGreater(run["peak_rss_bytes"], 0)
            self.assertAlmostEqual(run["elapsed_seconds"], 40, places=2)
            self.assertAlmostEqual(run["cpu_per_frame_seconds"], run["cpu_seconds"] / 100)
        self.assertAlmostEqual(numbers["cpu_per_frame_ratio"],
                               numbers["wine"]["cpu_seconds"] / numbers["old"]["cpu_seconds"])

    # spec: WINE-TEST-2
    def test_custom_duration_and_exit_line_for_both_stacks(self):
        result = self.invoke({"old": {"exit_line": True}}, "--seconds", "1.5", "--threshold", "1000")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        for call in calls:
            self.assertEqual(call["args"][call["args"].index("--seconds") + 1], "1.5")

    # spec: WINE-TEST-2
    def test_cpu_regression_fails_with_default_five_percent_threshold(self):
        result = self.invoke({"wine": {"cpu": 0.2}})
        self.assertEqual(result.returncode, 1, result.stderr)
        numbers = json.loads(self.numbers.read_text())
        self.assertEqual(numbers["threshold_percent"], 5)
        self.assertGreater(numbers["cpu_per_frame_ratio"], 1.05)
        self.assertIn("FAIL", result.stdout)

    # spec: WINE-TEST-2
    def test_frames_below_ninety_percent_fail_even_with_cpu_allowance(self):
        result = self.invoke({"wine": {"frames": 89}}, "--threshold", "1000")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(json.loads(self.numbers.read_text())["frame_ratio"], 0.89)

    # spec: WINE-TEST-2
    def test_threshold_boundaries_and_cpu_per_frame_normalization(self):
        old = {"frames": 100, "cpu_per_frame_seconds": 1.0}
        wine = {"frames": 90, "cpu_per_frame_seconds": 1.05}
        self.assertTrue(parity.compare(old, wine, 5)["passed"])
        wine["cpu_per_frame_seconds"] = 1.05001
        self.assertFalse(parity.compare(old, wine, 5)["passed"])
        self.assertTrue(parity.compare(old, wine, 6)["passed"])
        wine["frames"] = 89
        self.assertFalse(parity.compare(old, wine, 6)["passed"])
        old["cpu_per_frame_seconds"] = 0.5
        wine.update(frames=200, cpu_per_frame_seconds=0.5)
        self.assertEqual(parity.compare(old, wine, 0)["cpu_per_frame_ratio"], 1)
        self.assertTrue(parity.compare(old, wine, 0)["passed"])

    # spec: WINE-TEST-2
    def test_zero_frames_cannot_pass_and_json_has_no_infinities(self):
        for config in ({"old": {"frames": 0}}, {"wine": {"frames": 0}},
                       {"old": {"frames": 0}, "wine": {"frames": 0}}):
            with self.subTest(config=config):
                result = self.invoke(config)
                self.assertEqual(result.returncode, 1, result.stderr)
                numbers = json.loads(self.numbers.read_text())
                self.assertIsNone(numbers["cpu_per_frame_ratio"])
                self.assertFalse(numbers["passed"])
                self.assertIn("n/a", result.stdout)

    # spec: WINE-TEST-2
    def test_runner_failure_or_missing_summary_is_an_error(self):
        for stack in ("old", "wine"):
            for failure in ({"returncode": 3}, {"missing": True}, {"guest_code": 9}):
                with self.subTest(stack=stack, failure=failure):
                    self.numbers.unlink(missing_ok=True)
                    result = self.invoke({stack: failure})
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("parity:", result.stderr)
                    self.assertEqual(result.stdout, "")
                    self.assertFalse(self.numbers.exists())

    # spec: WINE-TEST-2
    def test_invalid_duration_or_threshold_does_not_launch(self):
        for option, value in (("--seconds", "0"), ("--seconds", "-1"), ("--seconds", "nan"),
                              ("--threshold", "-1"), ("--threshold", "inf")):
            with self.subTest(option=option, value=value):
                result = self.invoke(None, option, value)
                self.assertEqual(result.returncode, 2)
                self.assertFalse((self.root / "calls.jsonl").exists())

    # spec: WINE-TEST-2
    def test_json_is_optional(self):
        self.arguments = self.arguments[:-2]
        result = self.invoke(None, "--threshold", "1000")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.numbers.exists())


if __name__ == "__main__":
    unittest.main()
