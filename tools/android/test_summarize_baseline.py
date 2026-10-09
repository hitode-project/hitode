"""Authored scene evidence; no actual device performance is claimed."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("summarize_baseline", Path(__file__).with_name("summarize-baseline.py"))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)


def write_json(path, data):
    path.write_text(json.dumps(data), encoding="utf-8")


def distribution(count, scale=1):
    return {"samples": count, "total_ms": count * scale, "percentile_window_samples": min(count, 4096),
            "percentile_window_capacity": 4096, "mean_ms": scale, "p50_ms": scale,
            "p95_ms": scale * 2, "p99_ms": scale * 3, "max_ms": scale * 4}


class BaselineTests(unittest.TestCase):
    def make_run(self, root, name, mode="cold", *, warm_hash="e" * 64, scale=1):
        directory = root / name
        directory.mkdir()
        font_manifest = {"profile": {"path": "/fonts/profile.json", "sha256": "a" * 64},
                         "files": [{"path": "/fonts/fixture.ttf", "sha256": "b" * 64}]}
        game_files = [{"path": "fixture.exe", "bytes": 10, "sha256": "c" * 64}]
        seed_files = [{"path": "savedata.bin", "bytes": 20, "sha256": "d" * 64}]
        binding = {"guest_exe_sha256": "c" * 64, "game_files": game_files, "seed_files": seed_files,
                   "font_manifest": font_manifest, "codepage": 932, "scene_sha256": "f" * 64}
        steps = [{"checkpoint": "loaded", "await": {"frame_fnv1a64": "0000000000000001", "timeout_ms": 1000},
                  "assert": {}, "reset_metrics": True},
                 {"checkpoint": "end", "await": {"frame_fnv1a64": "0000000000000002", "timeout_ms": 1000}, "assert": {}},
                 {"command": "close"}]
        reference_path = root / "approved.json"
        if not reference_path.exists():
            write_json(reference_path, {"schema": "adv-scene-reference-v1", "status": "approved", "binding": binding,
                       "checkpoints": {row["checkpoint"]: {"frame_fnv1a64": row["await"]["frame_fnv1a64"], "assert": {}}
                                       for row in steps if "checkpoint" in row}})
        checkpoints, actions = [], []
        final = None
        for index, step in enumerate(steps[:2]):
            frames = 10 * (index + 1)
            count = 10 if index == 0 else 5
            gate = {"arms": index + 1, "matches": index + 1, "pending": False, "frozen": True,
                    "frame_checks": 2 * (index + 1), "full_hash_checks": index + 1,
                    "check_seconds": 0.003 * (index + 1), "hash_seconds": 0.002 * (index + 1),
                    "hash_max_seconds": 0.002, "frozen_observation_seconds": 20 * (index + 1)}
            performance = {"measurement_start_frame": 0 if index == 0 else 10,
                           "measurement_elapsed_seconds": 99, "runtime_steps": count, "runtime_soft_yields": count,
                           "runtime_step_seconds": count / 10, "frame_observations": count + 1,
                           "guest_serial_skipped_between_observations": 0, "wait_requested_seconds": count / 10,
                           "wait_deadline_wakes": count, "wait_input_wakes": count,
                           baseline.ACTIVE: distribution(count, scale), "wait_deadline_overshoot": distribution(count),
                           "warm_wall_time_between_frame_observations": distribution(count, 900)}
            report = {"frames": frames, "frame_fnv1a64": step["await"]["frame_fnv1a64"], "failure": None,
                      "screenshot_error": None, "stop_reason": "running", "guest_exit_code": None,
                      "api": {"mode": "strict", "acceptance_eligible": True, "stub_calls": 0, "signature_conflicts": []},
                      "audio": {"device_requested": False, "device_running": False},
                      "pcm": {"missing_stream_frames": 0, "missing_output_frames": 0},
                      "backend": "jit", "compile_strategy": "fast", "region_linking": True,
                      "tiered_compilation": False, "async_compilation": True, "codepage": 932,
                      "codec": {"provider": "native"}, "codec_kernels": {"mode": "native", "installed": 1},
                      "time_skip": {"enabled": True},
                      "compilations": 10 + index * 5, "compile_seconds": 1 + index / 2, "compile_requests": 10 + index * 5,
                      "compile_failures": 0, "stale_compilations": 0, "idle_waits": 10 + index * 5,
                      "idle_wait_seconds": 2 + index, "host_calls": 100 * (index + 1),
                      "native_entries": 1000 * (index + 1), "interpreter_entries": 100 * (index + 1),
                      "input_messages": index + 1, "pending_compilations": 0, "scene_gate": gate,
                      "performance": performance}
            report_path = directory / (step["checkpoint"] + ".json")
            png_path = directory / (step["checkpoint"] + ".png")
            write_json(report_path, report)
            png_path.write_bytes(b"authored screenshot checksum evidence" + bytes([index]))
            ack = (f"control ok await-frame hash={report['frame_fnv1a64']} frame={frames} "
                   f"start_frame={0 if index == 0 else 10} frozen=true release=none release_posted=false")
            checkpoints.append({"name": step["checkpoint"], "frames": frames, "frame_fnv1a64": report["frame_fnv1a64"],
                                "gate_ack": ack, "report_sha256": baseline.sha256(report_path),
                                "png_sha256": baseline.sha256(png_path), "scene_gate": gate})
            actions += [{"command": baseline.await_command(step["await"]), "ack": ack},
                        {"command": f"report /device/{step['checkpoint']}.json", "ack": f"control ok report /device/{step['checkpoint']}.json"}]
            if index == 0:
                actions.append({"command": "reset-metrics", "ack": "control ok reset-metrics start_frame=10"})
            final = copy.deepcopy(report)
        final.update(stop_reason="guest_exit", guest_exit_code=0)
        final["scene_gate"]["frozen"] = False
        write_json(directory / "final.json", final)
        write_json(directory / "actions.json", actions)
        for filename, data in (("final.png", b"final screenshot"), ("jit-warm.bin", b"produced guest-code profile"),
                               ("stderr.log", b""), ("remote-stdout.log", b""), ("exit-code.txt", b"0\n")):
            (directory / filename).write_bytes(data)
        metadata = {"completed": True, "evidence_complete": True, "fixed_workload_verified": True,
                    "suitable_for_performance_comparison": True, "remote_executable_hashes_verified": True,
                    "remote_asset_seed_hashes_verified": True, "remote_font_hashes_verified": True,
                    "warm_profile_input_verified": True, "scene_validation_status": "validated", "remote_exit_code": 0,
                    "adb_exit_code": 0, "font_diagnostics": False, "capture_pcm": False, "context": "adb-shell",
                    "scene_binding": binding, "font_manifest": font_manifest, "game_files": game_files, "seed_files": seed_files,
                    "scene_reference": str(reference_path), "scene_reference_sha256": baseline.sha256(reference_path),
                    "guest_exe_sha256": "c" * 64, "binary_sha256": "a" * 64, "schedule_sha256": "b" * 64,
                    "schedule": steps, "warm_mode": mode, "warm_input_sha256": warm_hash if mode == "warm" else None,
                    "warm_output_sha256": baseline.sha256(directory / "jit-warm.bin"),
                    "final_report_sha256": baseline.sha256(directory / "final.json"),
                    "final_png_sha256": baseline.sha256(directory / "final.png"), "checkpoints": checkpoints,
                    "command": ["/device/adv-run", "--codec", "native", "--codec-kernels", "native"],
                    "device": {"ro.product.model": "authored fixture", "host_page_size": "4096"}, "serial": "fixture"}
        write_json(directory / "session.json", metadata)
        return directory

    def edit_session(self, directory, **changes):
        data = baseline.read_json(directory / "session.json")
        data.update(changes)
        write_json(directory / "session.json", data)

    def edit_report(self, directory, name, edit):
        path = directory / (name + ".json")
        report = baseline.read_json(path)
        edit(report)
        write_json(path, report)
        metadata = baseline.read_json(directory / "session.json")
        if name == "final":
            metadata["final_report_sha256"] = baseline.sha256(path)
        else:
            for entry in metadata["checkpoints"]:
                if entry["name"] == name:
                    entry["report_sha256"] = baseline.sha256(path)
        write_json(directory / "session.json", metadata)

    def test_groups_keep_every_run_percentile_and_reset_counter_scope(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            cold = self.make_run(root, "cold", scale=1)
            warm_a = self.make_run(root, "warm-a", "warm", scale=2)
            warm_b = self.make_run(root, "warm-b", "warm", scale=4)
            summary = baseline.summarize([cold, warm_a, warm_b])
            self.assertEqual([row["repeat_count"] for row in summary["groups"]], [1, 2])
            runs = summary["groups"][1]["runs"]
            self.assertEqual([row["checkpoints"][1]["active_frame_interval"]["p95_ms"] for row in runs], [4, 8])
            row = runs[0]["checkpoints"][1]
            self.assertEqual(row["metrics_epoch"], 1)
            self.assertEqual(row["window_counter_delta"]["runtime_steps"], 5)
            self.assertEqual(row["process_counter_delta"]["compilations"], 5)
            self.assertEqual(row["gate_counter_delta"]["full_hash_checks"], 1)
            self.assertIn("not pooled", baseline.table(summary))
            self.assertNotIn("fps", {key.lower() for key in row})

    def test_different_binary_profile_or_tiering_cannot_be_mixed(self):
        for change in ("binary", "warm", "tiering", "threads"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                a = self.make_run(root, "a", "warm")
                b = self.make_run(root, "b", "warm")
                if change == "binary":
                    self.edit_session(b, binary_sha256="0" * 64)
                elif change == "warm":
                    self.edit_session(b, warm_input_sha256="0" * 64)
                elif change == "tiering":
                    for checkpoint in ("loaded", "end", "final"):
                        self.edit_report(b, checkpoint, lambda report: report.update(tiered_compilation=True))
                else:
                    field = {"compilation_schedule": {"compile_threads": 4}}
                    for checkpoint in ("loaded", "end", "final"):
                        self.edit_report(b, checkpoint, lambda report: report.update(field))
                with self.assertRaises(ValueError):
                    baseline.summarize([a, b])

    def test_execution_histograms_are_kept_without_replacing_missing_evidence(self):
        self.assertIsNone(baseline.execution_metrics({})["compilation_schedule"])
        counters = {"compilation_schedule": {"fast": {"service_us": {"buckets": [0, 1, 3]}}}}
        self.assertEqual(baseline.execution_metrics(counters)["compilation_schedule"], counters["compilation_schedule"])

    def test_device_conditions_require_a_stable_screen_and_complete_temperature(self):
        state = {"screen": "Awake", "foreground": "fixture", "allowed_cpus": "0-6", "battery_temperature_c": 28.5}
        self.assertEqual(baseline.condition_identity({"device_conditions_before": state,
                         "device_conditions_after": {**state, "battery_temperature_c": 29.1}}),
                         {key: state[key] for key in ("screen", "foreground", "allowed_cpus")})
        for change in ({"screen": "Asleep"}, {"foreground": "other"}, {"allowed_cpus": "0-2"}, {"battery_temperature_c": None}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                baseline.condition_identity({"device_conditions_before": state, "device_conditions_after": state | change})

    def test_observation_diagnostics_verify_and_ambiguous_seed_are_rejected(self):
        for change in ({"scene_validation_status": "observation_only"}, {"fixed_workload_verified": False},
                       {"capture_pcm": True}, {"font_diagnostics": True}, {"warm_mode": "seed"}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temp:
                directory = self.make_run(Path(temp), "run")
                self.edit_session(directory, **change)
                with self.assertRaises(ValueError):
                    baseline.summarize([directory])
        with tempfile.TemporaryDirectory() as temp:
            directory = self.make_run(Path(temp), "verify")
            self.edit_report(directory, "final", lambda report: report["codec_kernels"].update(mode="verify"))
            with self.assertRaisesRegex(ValueError, "Verify"):
                baseline.summarize([directory])

    def test_tampered_missing_evidence_and_reference_are_rejected(self):
        for filename in ("end.png", "final.json", "jit-warm.bin", "approved.json"):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                directory = self.make_run(root, "run")
                path = root / filename if filename == "approved.json" else directory / filename
                path.write_bytes(b"changed evidence")
                with self.assertRaises(ValueError):
                    baseline.summarize([directory])

    def test_counter_decrease_without_reset_and_false_frozen_gate_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = self.make_run(Path(temp), "run")
            self.edit_report(directory, "end", lambda report: report.update(compilations=1))
            with self.assertRaisesRegex(ValueError, "counter decreased"):
                baseline.summarize([directory])
        with tempfile.TemporaryDirectory() as temp:
            directory = self.make_run(Path(temp), "run")
            self.edit_report(directory, "end", lambda report: report["scene_gate"].update(frozen=False))
            with self.assertRaisesRegex(ValueError, "frozen"):
                baseline.summarize([directory])

    def test_bounded_percentile_window_is_retained_and_inconsistent_count_fails(self):
        value = distribution(5000)
        result = baseline.distribution(value, "window")
        self.assertEqual(result["percentiles_cover"], "last window only")
        value["percentile_window_samples"] = 4999
        with self.assertRaisesRegex(ValueError, "window"):
            baseline.distribution(value, "window")


if __name__ == "__main__":
    unittest.main()
