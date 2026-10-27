"""Frame attribution and checkpoint tests with synthetic inputs.

Run: python -m unittest discover -s tools/performance -p test_frame_budget.py
No game or GPU is required; output is unittest statistics.
"""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import play_session
import frame_budget as budget
from scene_schedule import load_scene, validate_reference


def frame(**changes):
    return dict(serial=1, startup=False, wall_ns=50_000_000, active_ns=45_000_000,
                guest_exec_ns=40_000_000, hle_ns={"graphics": 5_000_000, "file": 0, "audio": 0, "other": 0},
                present_call_ns=1_000_000, wait_ns=0, wait_overshoot_ns=0, wait_timeouts=0,
                cpu={}, core=None, frequency_khz=None, native_reference="unmeasured") | changes


class FrameBudgetTests(unittest.TestCase):
    def test_measured_fallback_lanes_outrank_entry_and_helper_count_hints(self):
        cpu = {"tier2_blocks": 100000, "interpreter_entries": 9000, "softfloat_calls": 9000,
               "reason_ns": {"uncovered": 1000, "pending": 1000}}
        self.assertEqual(budget.classify(frame(cpu=cpu))["cause"], "structure.tier2_execution")
        cpu["reason_ns"]["uncovered"] = 30_000_000
        self.assertEqual(budget.classify(frame(cpu=cpu))["cause"], "execution.interpreter_uncovered")

    def test_ux_sequence_splits_loading_but_keeps_compute_stalls(self):
        rows = [frame(serial=i, present_interval_ns=20_000_000, wall_ns=20_000_000,
                      active_ns=18_000_000, guest_exec_ns=17_000_000) for i in range(1, 23)]
        rows[5].update(present_interval_ns=60_000_000, wall_ns=60_000_000)
        rows[12].update(present_interval_ns=800_000_000, wall_ns=800_000_000, file_read_bytes=1024)
        ux = budget.ux_metrics(dict(sequence=rows, frames=len(rows), sequence_dropped=0), {"cadence_ms": 20})
        self.assertTrue(ux["sequence_complete"])
        self.assertEqual(len(ux["animation"]["windows"]), 2)
        first = ux["animation"]["windows"][0]
        self.assertEqual(first["estimated_missed_presents"], 2)
        self.assertEqual(first["gaps_missing_at_least_two"], 1)
        self.assertEqual(first["stall_costs"]["wall_ns"], 60_000_000)
        self.assertEqual(ux["animation"]["boundaries"][0]["kind"], "possible_loading")
        rows[12]["file_read_bytes"] = 0
        self.assertEqual(len(budget.animation_windows(rows, 20)["windows"]), 1)

    def test_ux_cannot_use_sparse_long_frames_or_evicted_rows(self):
        for data in (dict(rows=[frame()], frames=1),
                     dict(sequence=[frame(serial=1), frame(serial=3)], frames=2),
                     dict(sequence=[frame()], frames=1, sequence_dropped=1),
                     dict(sequence=[frame()], frames=1, coalesced_frames=1)):
            ux = budget.ux_metrics(data, {})
            self.assertFalse(ux["sequence_complete"])
            self.assertNotIn("animation", ux)

    def test_ux_waiting_phase_is_not_missed_frames_against_an_earlier_fast_burst(self):
        rows = [frame(serial=i, present_interval_ns=10_000_000, wall_ns=10_000_000,
                      wait_ns=0) for i in range(1, 17)]
        rows += [frame(serial=i, present_interval_ns=50_000_000, wall_ns=50_000_000,
                       wait_ns=45_000_000) for i in range(17, 33)]
        value = budget.animation_windows(rows)
        self.assertEqual(len(value["windows"]), 2)
        self.assertEqual(value["unclassified_frames"], 0)
        self.assertTrue(value["windows"][1]["waiting_dominated"])
        self.assertEqual(sum(w["estimated_missed_presents"] for w in value["windows"]), 0)

    def test_ux_marks_self_cadence_as_proxy_and_excludes_freezes(self):
        rows = [frame(serial=i, present_interval_ns=16_000_000) for i in range(1, 20)]
        rows[9].update(present_interval_ns=100_000_000, control_pause_ns=90_000_000)
        value = budget.animation_windows(rows)
        self.assertEqual(len(value["windows"]), 2)
        self.assertTrue(all(w["cadence_reference"] == "observed_p20_proxy" for w in value["windows"]))
        self.assertEqual(value["boundaries"][0]["kind"], "observer")
        with self.assertRaises(ValueError):
            budget.animation_windows(rows, float("nan"))

    def test_cost_totals_keep_compilation_and_api_subsets_inclusive(self):
        row = frame(operation_ns={"text": 1000}, file_read_bytes=17,
                    cpu={"reason_ns": {"pending": 30}, "sync_compile_ns": 5})
        total = budget.cost_totals([budget.clean_row(row)] * 2)
        self.assertEqual(total["file_read_bytes"], 34)
        self.assertEqual(total["cpu"]["reason_ns"]["pending"], 60)
        self.assertEqual(total["operation_ns"]["text"], 2000)
        self.assertNotIn("image_decode", total)

    def test_first_entry_uses_title_ack_and_does_not_claim_warm_cold(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            budget.write_json(root / "actions.json", [
                {"command": "await-frame 1111111111111111 10000", "ack_seconds": 2.5},
                {"command": "report /private/title.json", "ack_seconds": 3.2}])
            value = budget.first_entry(root, "title", True, {}, True)
            self.assertEqual(value["seconds"], 2.5)
            self.assertEqual(value["status"], "observed_upper_bound")
            self.assertEqual(budget.first_entry(root, "title", False, {}, True)["status"], "unmeasured")

    def test_timed_ctrl_requires_frozen_start_successful_hold_and_no_early_release(self):
        scenario = {"schedule": {"steps": [{"checkpoint": "body", "await": {"frame_fnv1a64": "1" * 16}}]},
                    "scenes": {"ctrl": {"start_checkpoint": "body"}}}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            budget.write_json(root / "body.json", {"frames": 3, "frame_fnv1a64": "1" * 16,
                "scene_gate": {"frozen": True, "pending": False}})
            budget.write_json(root / "ctrl.json", {"frames": 33})
            actions = [{"command": "key-down 17", "ack_seconds": 1, "ack": "control ok key-down 17"},
                       {"command": "report /private/ctrl.json", "sent_seconds": 6},
                       {"command": "key-up 17", "ack": "control ok key-up 17"}]
            budget.write_json(root / "actions.json", actions)
            with patch.object(budget.baseline, "valid_report"):
                value = budget.timed_ctrl_segment(root, scenario, "ctrl")
                self.assertEqual(value["commits_per_second"], 6)
                self.assertFalse(value["fixed_workload"])
                actions.insert(1, {"command": "key-up 17"})
                budget.write_json(root / "actions.json", actions)
                self.assertIsNone(budget.timed_ctrl_segment(root, scenario, "ctrl"))

    def test_native_cache_summary_keeps_statistics_without_private_payloads(self):
        result = budget.execution_metrics({"native_entries": 4, "interpreter_entries": 6,
            "code_cache": {"enabled": True, "hits": 3, "bytes": 128, "raw": "private native bytes"},
            "aot": {"private_completions": 2, "publications": 1, "guest_pc": 12345}})
        self.assertEqual(result["counts"], {"native_entries": 4, "interpreter_entries": 6})
        self.assertEqual(result["code_cache"], {"enabled": True, "hits": 3, "bytes": 128})
        self.assertEqual(result["aot"], {"private_completions": 2, "publications": 1})
        self.assertNotIn("private native bytes", json.dumps(result))


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

    def test_wait_summary_keeps_numeric_data_and_drops_private_labels(self):
        report = {"wait_diagnostics": {"schema": "adv-wait-diagnostics-v1", "sources": {
            "sleep": {"segments": 3, "private": "hidden", "requested_us": {"p50": 16000, "private": "hidden"}},
            "private": {"segments": 1}}, "private": "hidden"}}
        result = budget.wait_metrics(report)
        self.assertEqual(result["sources"]["sleep"]["segments"], 3)
        self.assertEqual(result["sources"]["sleep"]["requested_us"], {"p50": 16000})
        self.assertNotIn("hidden", json.dumps(result))
        self.assertNotIn("private", json.dumps(result))


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


    def test_all_scenarios_use_known_scenes_and_local_sample_paths(self):
        for path in budget.SCENARIOS.glob("frame-budget-*.json"):
            scenario = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(set(scenario["scenes"]), set(budget.SCENES))
            self.assertFalse(Path(scenario["game_dir"]).is_absolute())
            with tempfile.TemporaryDirectory() as directory:
                schedule = Path(directory) / "schedule.json"
                budget.write_json(schedule, scenario["schedule"])
                if load_scene(schedule) is None:
                    play_session.load_schedule(schedule)


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


if __name__ == "__main__":
    unittest.main()
