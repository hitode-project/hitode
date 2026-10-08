"""Frame attribution, checkpoint and transport safety tests with synthetic inputs.

Run: python -m unittest discover -s tools/performance -p test_frame_budget.py
No game, Android device or GPU is required; output is unittest statistics.
"""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import frame_budget as budget
from scene_schedule import load_scene, validate_reference


def frame(**changes):
    return dict(serial=1, startup=False, wall_ns=50_000_000, active_ns=45_000_000,
                guest_exec_ns=40_000_000, hle_ns={"graphics": 5_000_000, "file": 0, "audio": 0, "other": 0},
                present_call_ns=1_000_000, wait_ns=0, wait_overshoot_ns=0, wait_timeouts=0,
                cpu={}, core=None, frequency_khz=None, native_reference="unmeasured") | changes


class FrameBudgetTests(unittest.TestCase):
    def test_compile_overlap_is_explicit_and_worker_time_is_not_charged(self):
        row = frame(cpu={"compile_wait_execution_ns": 30_000_000, "compile_time": 2_000_000_000})
        self.assertEqual(budget.classify(row)["cause"], "cold.compile_pending_execution")
        self.assertEqual(budget.classify(row)["confidence"], "measured_overlap")
        self.assertEqual(budget.classify(frame(cpu={"compile_time": 2_000_000_000}))["cause"], "unattributed")

    def test_largest_measured_cost_wins_before_count_hints(self):
        row = frame(hle_ns={"file": 32_000_000}, cpu={"sync_compile_ns": 20_000_000, "softfloat_calls": 99999})
        self.assertEqual(budget.classify(row)["cause"], "host.file")

    def test_native_work_is_only_a_residual_hint(self):
        result = budget.classify(frame(cpu={"tier2_blocks": 1000}))
        self.assertEqual((result["cause"], result["confidence"]), ("structure.tier2_execution", "residual_hint"))

    def test_softfloat_counter_does_not_blame_a_sleeping_frame(self):
        result = budget.classify(frame(active_ns=5_000_000, wait_ns=42_000_000, cpu={"softfloat_calls": 5000}))
        self.assertEqual(result["cause"], "platform.wait_or_idle")

    def test_frequency_and_sparse_core_change_are_not_proof_of_throttling(self):
        result = budget.classify(frame(core=0, frequency_khz=300000))
        self.assertEqual(result["cause"], "unattributed")
        self.assertEqual(result["evidence"]["frequency_khz"], 300000)

    def test_wait_overshoot_distinguishes_requested_idle(self):
        result = budget.classify(frame(active_ns=5_000_000, wait_ns=42_000_000, wait_overshoot_ns=20_000_000))
        self.assertEqual(result["cause"], "platform.wait_overshoot")

    def test_summary_preserves_cumulative_counts_and_discloses_ring_loss(self):
        data = {key: 0 for key in ("frames", "over_budget_frames", "active_over_budget_frames", "max_wall_ns",
                "max_active_ns", "wall_total_ns", "active_total_ns", "p95_wall_ns", "p99_wall_ns", "p99_active_ns",
                "percentile_window", "percentile_capacity", "coalesced_frames", "observer_ns")}
        data.update(budget_ns=budget.BUDGET_NS, frames=5000, over_budget_frames=10, rows=[frame()], longest=frame())
        result = budget.summarize_budget(data)
        self.assertEqual((result["over_budget_frames"], result["dropped_over_budget_rows"]), (10, 9))
        self.assertEqual(sum(result["cause_distribution_retained"].values()), 1)
        self.assertEqual(result["native_reference"]["excluded_frames"], 0)

    def test_checkpoints_require_both_exact_endpoints_and_successful_ack(self):
        scenario = {"schedule": {"steps": [
            {"checkpoint": "start", "await": {"frame_fnv1a64": "1111111111111111"}},
            {"checkpoint": "end", "await": {"frame_fnv1a64": "2222222222222222", "hold_key": 17}}]},
            "scenes": {"ctrl": {"start_checkpoint": "start"}}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            def report(frame, digest):
                return {"frame_fnv1a64": digest, "frames": frame,
                    "scene_gate": {"frozen": True, "pending": False},
                    "api": {"mode": "strict", "stub_calls": 0, "acceptance_eligible": True},
                    "audio": {"device_requested": False, "device_running": False},
                    "pcm": {"missing_stream_frames": 0, "missing_output_frames": 0}}
            budget.write_json(path / "start.json", report(12, "1111111111111111"))
            budget.write_json(path / "end.json", report(22, "2222222222222222"))
            action = {"command": "await-frame 2222222222222222 90000 hold 17", "sent_seconds": 4,
                      "ack_seconds": 7, "ack": "control ok await-frame hash=2222222222222222 frame=22 start_frame=12 frozen=true release=17 release_posted=true"}
            budget.write_json(path / "actions.json", [action])
            segment = budget.fixed_segment(path, scenario, "end")
            self.assertEqual((segment["seconds"], segment["advanced_frames"]), (3, 10))
            action["ack"] = "control error timeout"
            budget.write_json(path / "actions.json", [action])
            self.assertIsNone(budget.fixed_segment(path, scenario, "end"))

    def test_ungated_checkpoint_is_explicit_and_cannot_be_a_reference(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "scene.json"
            value = {"schema": "adv-scene-v1", "timeout_seconds": 30,
                     "steps": [{"checkpoint": "observation", "observe": True}]}
            budget.write_json(path, value)
            self.assertTrue(load_scene(path)["steps"][0]["observe"])
            with self.assertRaises(ValueError):
                validate_reference({}, {}, value["steps"])
            value["steps"][0]["await"] = {}
            budget.write_json(path, value)
            with self.assertRaises(ValueError):
                load_scene(path)

    def test_cleanup_rejects_shared_and_traversing_paths(self):
        adb = Mock()
        for path in ("/data/local/tmp", "/data/local/tmp/adv-host-a-fonts", "/data/local/tmp/adv-host-bud-../"):
            with self.assertRaises(ValueError):
                budget.cleanup_remote(adb, path)
        adb.call.assert_not_called()
        budget.cleanup_remote(adb, "/data/local/tmp/adv-host-bud-" + "1" * 32)
        self.assertIn("readlink /proc/$p/exe", adb.call.call_args.args[1])

    def test_all_scenarios_use_known_scenes_and_local_sample_paths(self):
        for path in budget.SCENARIOS.glob("frame-budget-*.json"):
            scenario = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(set(scenario["scenes"]), set(budget.SCENES))
            self.assertFalse(Path(scenario["game_dir"]).is_absolute())
            with tempfile.TemporaryDirectory() as directory:
                schedule = Path(directory) / "schedule.json"
                budget.write_json(schedule, scenario["schedule"])
                if load_scene(schedule) is None:
                    budget.replay.load_schedule(schedule)

    def test_android_switches_reuse_existing_runner(self):
        args = Mock(exe_relative="fixture.exe", codepage=932, codec="native", codec_kernels="native",
                    frame_budget=True, perf_map=True, no_tiering=False, schedule_profile=False,
                    auto_dialog_ms=50, tier1="off", compile_threads=None, font_diagnostics=False,
                    font_profile=None, capture_pcm=False, cpu_mask=None)
        command = budget.replay.runner_command(args, "/private/run", "/readonly/game", 30)
        self.assertIn("--frame-budget", command)
        self.assertIn("ADV_PERF_MAP_DIR=/private/run", command)
        self.assertNotIn("--profile", command)


if __name__ == "__main__":
    unittest.main()
