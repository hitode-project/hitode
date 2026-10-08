"""Frame attribution, checkpoint and transport safety tests with synthetic inputs.

Run: python -m unittest discover -s tools/performance -p test_frame_budget.py
No game, Android device or GPU is required; output is unittest statistics.
"""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import time

import frame_budget as budget
from scene_schedule import load_scene, validate_reference


def frame(**changes):
    return dict(serial=1, startup=False, wall_ns=50_000_000, active_ns=45_000_000,
                guest_exec_ns=40_000_000, hle_ns={"graphics": 5_000_000, "file": 0, "audio": 0, "other": 0},
                present_call_ns=1_000_000, wait_ns=0, wait_overshoot_ns=0, wait_timeouts=0,
                cpu={}, core=None, frequency_khz=None, native_reference="unmeasured") | changes


class FrameBudgetTests(unittest.TestCase):
    def test_scene_check_can_push_combined_work_over_the_budget(self):
        row = frame(wall_ns=40_000_000, active_ns=25_000_000, guest_exec_ns=22_000_000,
                    hle_ns={"other": 4_000_000}, wait_ns=3_000_000,
                    service_audio_ns=2_000_000, scene_check_ns=7_000_000)
        self.assertEqual(budget.classify(row)["cause"], "observer.mixed_scene_work")
        self.assertEqual(budget.classify(row | {"scene_check_ns": 0})["cause"], "unattributed")

    def test_mixed_execution_and_wait_are_not_a_hidden_host_gap(self):
        result = budget.classify(frame(wall_ns=50_000_000, active_ns=29_000_000,
                                       guest_exec_ns=22_000_000, wait_ns=18_000_000))
        self.assertEqual(result["cause"], "platform.mixed_wait_execution")

    def test_runtime_pumps_are_work_outside_backend_and_not_hle(self):
        row = frame(wall_ns=36_000_000, active_ns=15_000_000,
                    guest_exec_ns=10_000_000, wait_ns=11_000_000, service_audio_ns=10_000_000)
        result = budget.classify(row)
        self.assertEqual(result["cause"], "host.audio_service")
        row["service_audio_ns"] = 7_000_000
        self.assertEqual(budget.classify(row)["cause"], "platform.mixed_wait_execution_services")
        row["service_audio_ns"] = 0
        row["scene_check_ns"] = 10_000_000
        self.assertEqual(budget.classify(row)["cause"], "observer.scene_check")

    def test_explicit_host_costs_and_observer_freezes_have_separate_causes(self):
        for field, cause in (("scheduler_ns", "host.scheduler"), ("frontend_ns", "host.frontend_control"),
                             ("control_pause_ns", "observer.control_pause")):
            result = budget.classify(frame(**{field: 30_000_000}, active_ns=5_000_000))
            self.assertEqual(result["cause"], cause)

    def test_api_allowlist_and_retained_subset_totals(self):
        row = budget.clean_row(frame(top_apis=[{"api": "kernel32.dll!ReadFile", "ns": 17, "calls": 2},
                                               {"api": "private/path!bad", "ns": 999}]))
        self.assertEqual(len(row["top_apis"]), 1)
        self.assertEqual(budget.top_apis([row, row]), [{"api": "kernel32.dll!ReadFile", "ns": 34, "calls": 4}])

    def test_native_partial_estimate_does_not_promote_rep_units_or_blocks(self):
        row = frame(cpu={"retired_tier0_instructions": 100_000_000, "tier0_units": 999_000_000,
                         "tier2_blocks": 999_000_000})
        estimate = budget.native_estimate(row, 2_000_000_000)
        self.assertEqual(estimate["native_time_lower_estimate_ns"], 50_000_000)
        self.assertTrue(estimate["native_over_budget_estimate"])
        self.assertEqual(estimate["retirement_coverage"], "tier0_only")
        self.assertIsNone(budget.native_estimate(frame(cpu={"tier2_blocks": 999_000_000}), 1)["native_time_lower_estimate_ns"])
    def test_broker_sharing_error_retries_only_acquisition(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper = root / "device.ps1"
            helper.touch()
            error = budget.subprocess.CalledProcessError(1, ["powershell"])
            with patch.object(budget.subprocess, "run", side_effect=[error, Mock(), Mock()]) as run, patch.object(budget.time, "sleep") as sleep:
                with budget.device_lock(None, root, helper):
                    pass
                self.assertEqual(run.call_count, 3)
                self.assertIn("Request-Device", run.call_args_list[1].args[0][-1])
                self.assertIn("Release-Device", run.call_args_list[2].args[0][-1])
                sleep.assert_called_once_with(2)

    def test_wait_summary_keeps_numeric_data_and_drops_private_labels(self):
        report = {"wait_diagnostics": {"schema": "adv-wait-diagnostics-v1", "sources": {
            "sleep": {"segments": 3, "private": "hidden", "requested_us": {"p50": 16000, "private": "hidden"}},
            "private": {"segments": 1}}, "private": "hidden"}}
        result = budget.wait_metrics(report)
        self.assertEqual(result["sources"]["sleep"]["segments"], 3)
        self.assertEqual(result["sources"]["sleep"]["requested_us"], {"p50": 16000})
        self.assertNotIn("hidden", json.dumps(result))
        self.assertNotIn("private", json.dumps(result))

    def test_broker_uses_the_calling_tasks_queue_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper = root / "device.ps1"
            helper.touch()
            with patch.dict("os.environ", {"ADV_DEVICE_TASK": "wait"}), patch.object(budget.subprocess, "run") as run:
                with budget.device_lock(None, root, helper):
                    pass
                self.assertEqual(run.call_count, 2)
                for call in run.call_args_list:
                    self.assertEqual(call.kwargs["env"]["ADV_DEVICE_TASK"], "wait")

    def test_compile_overlap_is_explicit_and_worker_time_is_not_charged(self):
        row = frame(cpu={"compile_wait_execution_ns": 30_000_000, "compile_time": 2_000_000_000})
        self.assertEqual(budget.classify(row)["cause"], "cold.compile_pending_execution")
        self.assertEqual(budget.classify(row)["confidence"], "measured_overlap")
        self.assertEqual(budget.classify(frame(cpu={"compile_time": 2_000_000_000}))["cause"], "structure.guest_execution")

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
        self.assertEqual(result["cause"], "structure.guest_execution")
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

    def test_deployment_pushes_independent_files_with_a_shared_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "data").mkdir()
            (root / "fixture.txt").write_text("fixture", encoding="utf-8")
            (root / "data" / "resource.txt").write_text("resource", encoding="utf-8")
            adb = Mock()
            budget.deploy_game(adb, root, "/private/game", {}, time.monotonic() + 1000)
            self.assertEqual(adb.push.call_count, 2)
            self.assertTrue(all(call.args[0].is_file() for call in adb.push.call_args_list))
            self.assertLessEqual(adb.timeout, 120)
            self.assertTrue(adb.call.call_args.kwargs["idempotent"])

    def test_broker_releases_on_failure_without_legacy_lock_or_cooldown(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper = root / "device.ps1"
            helper.touch()
            with patch.dict(budget.os.environ, {"ADV_DEVICE_TASK": "fixture-cvg"}), \
                    patch.object(budget.subprocess, "run") as invoke, patch.object(budget.time, "sleep") as sleep:
                with self.assertRaisesRegex(RuntimeError, "fixture"):
                    with budget.device_lock(None, root, helper) as deadline:
                        self.assertGreater(deadline, time.monotonic())
                        raise RuntimeError("fixture")
                self.assertEqual(invoke.call_count, 2)
                self.assertIn("Request-Device", invoke.call_args_list[0].args[0][-1])
                self.assertIn("Release-Device", invoke.call_args_list[1].args[0][-1])
                self.assertNotIn("-Done", invoke.call_args_list[1].args[0][-1])
                self.assertEqual(invoke.call_args.kwargs["env"]["ADV_DEVICE_TASK"], "fixture-cvg")
                sleep.assert_not_called()

    def test_pcm_failure_is_explicit_without_exporting_raw_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            budget.write_json(root / "title.json", {"frames": 1})
            scenario = {"id": "fixture", "scenario_sha256": "fixture", "schedule": [],
                        "scenes": {"title": {"checkpoint": "title"}}}
            with patch.object(budget.baseline, "valid_report", side_effect=ValueError("checkpoint: missing PCM frames; private details")):
                rows = budget.summarize_run(root, scenario, "cold", "android")
            self.assertEqual(rows[0]["status"], "blocked")
            self.assertEqual(rows[0]["reason"], "pcm_incomplete")
            self.assertNotIn("private details", json.dumps(rows))
            self.assertNotIn("metrics", rows[0])

    def test_deployment_reserves_cleanup_time_before_another_transfer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "fixture.txt").touch()
            adb = Mock()
            with self.assertRaises(TimeoutError):
                budget.deploy_game(adb, root, "/private/game", {}, time.monotonic() + 300)
            adb.push.assert_not_called()

    def test_resume_retries_deployment_without_replacing_guest_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "fixture-cold"
            output.mkdir()
            (output / "cmd.txt").write_text("failed deployment", encoding="utf-8")
            budget.preserve_undeployed(output, root)
            self.assertFalse(output.exists())
            self.assertEqual(len(list(root.glob("fixture-cold-undeployed-*"))), 1)
            output.mkdir()
            (output / "session.json").write_text("{}", encoding="utf-8")
            with self.assertRaises(ValueError):
                budget.preserve_undeployed(output, root)
            self.assertTrue((output / "session.json").is_file())

    def test_resume_rejects_a_move_outside_the_explicit_run_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "outside"
            output.mkdir()
            with self.assertRaises(ValueError):
                budget.preserve_undeployed(output, root / "intended")
            self.assertTrue(output.is_dir())


if __name__ == "__main__":
    unittest.main()
