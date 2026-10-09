"""Audit frame-budget trust boundaries using authored Python-only fixtures.

Usage: python -B -m unittest discover -s tools/performance -p test_frame_budget_audit.py
Inputs: synthetic reports, schedules, clocks and mocked process/transport results.
Output: unittest statistics; temporary fixtures use the caller's TEMP/TMPDIR.
No game, runner, adb command, device, cargo build or network access is required.
The fixtures retain the observed failures as regression assertions.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import io
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import frame_budget as budget
import play_session


START_HASH = "1" * 16
END_HASH = "2" * 16
PRIVATE_MARKER = "PRIVATE_AUDIT_SENTINEL"


def frame_row(**changes):
    return dict(serial=22, startup=False, wall_ns=50_000_000,
                active_ns=40_000_000, guest_exec_ns=30_000_000,
                hle_ns={"graphics": 10_000_000, "file": 0, "audio": 0, "other": 0},
                present_call_ns=2_000_000, wait_ns=0, wait_overshoot_ns=0,
                wait_timeouts=0, cpu={"reasons": {"uncovered": 1}},
                core=None, frequency_khz=None, native_reference="unmeasured") | changes


def frame_metrics(**changes):
    row = frame_row()
    return dict(schema="adv-frame-budget-v1", budget_ns=budget.BUDGET_NS,
                frames=1, over_budget_frames=1, active_over_budget_frames=1,
                max_wall_ns=50_000_000, max_active_ns=40_000_000,
                wall_total_ns=50_000_000, active_total_ns=40_000_000,
                p95_wall_ns=50_000_000, p99_wall_ns=50_000_000,
                p99_active_ns=40_000_000, percentile_window=1,
                percentile_capacity=4096, over_capacity=2048,
                coalesced_frames=0, observer_ns=1, longest=row,
                rows=[row]) | changes


def checkpoint(frame=22, frame_hash=END_HASH, **changes):
    return dict(frames=frame, frame_fnv1a64=frame_hash, failure=None,
                screenshot_error=None, scene_gate={"frozen": True, "pending": False},
                api={"mode": "strict", "stub_calls": 0, "acceptance_eligible": True},
                audio={"device_requested": False, "device_running": False},
                pcm={"missing_stream_frames": 0, "missing_output_frames": 0},
                backend="jit", compile_strategy="fast", region_linking=True,
                tiered_compilation=True, async_compilation=True, codepage=932,
                codec={"provider": "native"}, codec_kernels={"mode": "native"},
                parallel={"workers": 1}, frame_budget=frame_metrics()) | changes


def scenario():
    return {"id": "fixture", "scenario_sha256": "a" * 64,
            "game_dir": "fixture", "exe": "fixture.txt", "codepage": 932,
            "schedule": {"schema": "adv-scene-v1", "timeout_seconds": 15,
                         "steps": [
                             {"checkpoint": "start", "await": {
                                 "frame_fnv1a64": START_HASH, "timeout_ms": 1000}},
                             {"checkpoint": "end", "await": {
                                 "frame_fnv1a64": END_HASH, "timeout_ms": 1000,
                                 "hold_key": 17}}]},
            "scenes": {"title": {"checkpoint": "start", "measurement": "gated"},
                       "normal": {},
                       "ctrl": {"checkpoint": "end", "measurement": "gated",
                                "start_checkpoint": "start"},
                       "transition": {}}}


def gate_reply(frame_hash=END_HASH, frame=22, start_frame=12, **changes):
    fields = dict(hash=frame_hash, frame=frame, start_frame=start_frame,
                  frozen="true", release=17, release_posted="true") | changes
    return "control ok await-frame " + " ".join(f"{k}={v}" for k, v in fields.items())


class SyntheticAudit(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="frame-budget-audit-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write(self, name, value):
        budget.write_json(self.root / name, value)

    def seed_segment(self, **action_changes):
        self.write("start.json", checkpoint(12, START_HASH))
        self.write("end.json", checkpoint())
        self.write("session.json", {"completed": True, "evidence_complete": True,
                                   "exit_code": 0, "remote_exit_code": 0,
                                   "adb_exit_code": 0,
                                   "suitable_for_performance_comparison": True})
        action = dict(command=f"await-frame {END_HASH} 1000 hold 17",
                      sent_seconds=4.0, ack_seconds=7.0, ack=gate_reply())
        start_action = dict(command=f"await-frame {START_HASH} 1000",
                            sent_seconds=1.0, ack_seconds=2.0,
                            ack=gate_reply(START_HASH, 12, 0, release="none",
                                           release_posted="false"))
        self.write("actions.json", [start_action, action | action_changes])

    def title(self):
        return budget.summarize_run(self.root, scenario(), "cold", "android")[0]

    def assert_private_filtered(self, operation):
        try:
            result = operation()
        except (ValueError, TypeError):
            return  # Rejecting unknown data is also a safe trust boundary.
        self.assertNotIn(PRIVATE_MARKER, json.dumps(result))


class FixedEndpointAudit(SyntheticAudit):
    def test_valid_frozen_ctrl_segment_retains_exact_work(self):
        self.seed_segment()
        segment = budget.fixed_segment(self.root, scenario(), "end")
        self.assertEqual((segment["seconds"], segment["advanced_frames"]), (3, 10))
        self.assertFalse(segment["independent_reference"])

    def test_ctrl_rejects_false_hash_frame_freeze_and_release_receipts(self):
        bad_replies = [gate_reply(frame_hash=START_HASH), gate_reply(frame=23),
                       gate_reply(start_frame=11), gate_reply(frozen="false"),
                       gate_reply(release_posted="false"), gate_reply(release=13),
                       "control ok await-frame-other"]
        for reply in bad_replies:
            with self.subTest(reply=reply):
                self.seed_segment(ack=reply)
                self.assertIsNone(budget.fixed_segment(self.root, scenario(), "end"))

    def test_ctrl_receipt_must_prove_the_requested_hold_command(self):
        self.seed_segment(command=f"await-frame {END_HASH} 1000")
        self.assertIsNone(budget.fixed_segment(self.root, scenario(), "end"))

    def test_ctrl_rejects_nonpositive_work_and_invalid_elapsed_time(self):
        for end_frame, sent, acknowledged in ((12, 4, 7), (11, 4, 7),
                                              (22, 7, 4), (22, 4, math.nan)):
            with self.subTest(end_frame=end_frame, sent=sent, acknowledged=acknowledged):
                self.seed_segment(sent_seconds=sent, ack_seconds=acknowledged,
                                  ack=gate_reply(frame=end_frame))
                self.write("end.json", checkpoint(end_frame))
                self.assertIsNone(budget.fixed_segment(self.root, scenario(), "end"))

    def test_ctrl_requires_a_valid_frozen_start_report(self):
        for changes in ({"failure": "synthetic failure"},
                        {"scene_gate": {"frozen": False, "pending": True}}):
            with self.subTest(changes=changes):
                self.seed_segment()
                self.write("start.json", checkpoint(12, START_HASH, **changes))
                self.assertIsNone(budget.fixed_segment(self.root, scenario(), "end"))


class SummaryTrustAudit(SyntheticAudit):
    def test_summary_filters_private_fields_in_rows_and_longest(self):
        for location in ("rows", "longest"):
            with self.subTest(location=location):
                metrics = frame_metrics()
                metrics[location] = ([frame_row(debug_private=PRIVATE_MARKER)]
                                     if location == "rows" else
                                     frame_row(cpu={"debug_private": PRIVATE_MARKER}))
                self.assert_private_filtered(lambda: budget.summarize_budget(metrics))

    def test_attribution_does_not_export_untrusted_histogram_labels(self):
        for row in (frame_row(cpu={"reasons": {PRIVATE_MARKER: 100}}),
                    frame_row(hle_ns={PRIVATE_MARKER: 30_000_000})):
            with self.subTest(row=row):
                self.assert_private_filtered(lambda: budget.classify(row))

    def test_execution_and_overhead_control_objects_need_field_whitelists(self):
        for report in (checkpoint(12, START_HASH, tier1={"debug_private": PRIVATE_MARKER}),
                       checkpoint(12, START_HASH, frame_budget=None,
                                  performance={"debug_private": PRIVATE_MARKER})):
            with self.subTest(report=report):
                self.seed_segment()
                self.write("start.json", report)
                self.assert_private_filtered(self.title)

    def test_non_ctrl_checkpoint_rechecks_the_scheduled_hash(self):
        self.seed_segment()
        self.write("start.json", checkpoint(12, END_HASH))
        self.assertNotIn(self.title()["status"], ("observed", "control"))

    def test_gated_checkpoint_cannot_use_an_unfrozen_report(self):
        self.seed_segment()
        self.write("start.json", checkpoint(12, START_HASH,
                   scene_gate={"frozen": False, "pending": True}))
        self.assertNotIn(self.title()["status"], ("observed", "control"))

    def test_empty_metric_window_is_not_a_frame_baseline(self):
        self.seed_segment()
        self.write("start.json", checkpoint(12, START_HASH,
                   frame_budget=frame_metrics(frames=0, rows=[], longest=None)))
        self.assertNotEqual(self.title()["status"], "observed")

    def test_invalid_budget_schema_counts_and_numbers_fail_closed(self):
        for changes in ({"schema": "unknown"}, {"budget_ns": 1}, {"frames": True},
                        {"frames": -1}, {"over_budget_frames": 100},
                        {"p99_wall_ns": math.nan}, {"active_total_ns": -1}):
            with self.subTest(changes=changes):
                self.seed_segment()
                self.write("start.json", checkpoint(12, START_HASH,
                           frame_budget=frame_metrics(**changes)))
                self.assertNotEqual(self.title()["status"], "observed")

    def test_partial_checkpoint_json_does_not_abort_all_summary_rows(self):
        self.seed_segment()
        (self.root / "start.json").write_text('{"frames":', encoding="utf-8")
        rows = budget.summarize_run(self.root, scenario(), "cold", "android")
        self.assertEqual(len(rows), len(budget.SCENES))
        self.assertEqual(rows[0]["status"], "blocked")

    def test_failed_session_is_not_presented_as_an_unqualified_observation(self):
        for failure in ({"completed": False, "error": "synthetic failure"},
                        {"remote_exit_code": 137}, {"adb_exit_code": 1},
                        {"exit_code": 124}, {"evidence_complete": False}):
            with self.subTest(failure=failure):
                self.seed_segment()
                metadata = budget.load_json(self.root / "session.json") | failure
                self.write("session.json", metadata)
                row = self.title()
                if row["status"] in ("observed", "control"):
                    self.assertIn(row.get("run_status"), ("failed", "incomplete"))


class LockAudit(SyntheticAudit):
    def test_owner_write_failure_releases_the_directory_it_just_created(self):
        lock = self.root / "device-lock"
        original_write = Path.write_text

        def write(path, *args, **kwargs):
            if path == lock / "owner.txt":
                raise OSError("synthetic owner write failure")
            return original_write(path, *args, **kwargs)

        with patch.object(budget, "os", SimpleNamespace(name="posix")), \
                patch.object(Path, "write_text", write), self.assertRaises(OSError):
            with budget.device_lock(lock, self.root):
                self.fail("an owner write failure must prevent entry")
        self.assertFalse(lock.exists())

    def test_competing_owner_finishing_requires_a_fresh_sixty_second_gap(self):
        lock = self.root / "device-lock"
        lock.mkdir()
        (lock / "owner.txt").write_text("synthetic-other-owner", encoding="utf-8")
        current = [100.0]

        def sleep(seconds):
            current[0] += seconds
            if (lock / "owner.txt").exists():
                (lock / "owner.txt").unlink()
                lock.rmdir()
                (self.root / "last-lock-end.txt").write_text(str(current[0]), encoding="utf-8")

        with patch.object(budget, "os", SimpleNamespace(name="posix")), \
                patch.object(budget.time, "time", side_effect=lambda: current[0]), \
                patch.object(budget.time, "monotonic", side_effect=lambda: current[0]), \
                patch.object(budget.time, "sleep", side_effect=sleep), patch("builtins.print"):
            with budget.device_lock(lock, self.root):
                self.assertGreaterEqual(current[0], 190.0)

    def test_changed_foreign_owner_is_never_deleted(self):
        lock = self.root / "device-lock"
        with patch.object(budget, "os", SimpleNamespace(name="posix")):
            with budget.device_lock(lock, self.root):
                (lock / "owner.txt").write_text("synthetic-other-owner", encoding="utf-8")
        self.assertEqual((lock / "owner.txt").read_text(), "synthetic-other-owner")


class CleanupAudit(SyntheticAudit):
    def mocked_android_run(self, mkdir_fails=False, cleanup_fails=False):
        samples = self.root / "samples"
        (samples / "fixture").mkdir(parents=True)
        binary = self.root / "runner.txt"
        binary.touch()
        schedules = self.root / "scenarios"
        schedules.mkdir()
        budget.write_json(schedules / "frame-budget-fixture.json", scenario())
        args = argparse.Namespace(output=self.root / "output", binary=binary,
                                  games=["fixture"], heats=["cold"], platform="android",
                                  no_instrumentation=False, perf_map=False, seed_root=None,
                                  font_profile_source=None, device_lock=self.root / "mock-lock",
                                  adb="adb", serial="", adb_port=5037, transfer_timeout=1)
        client = Mock()
        if mkdir_fails:
            client.shell.side_effect = RuntimeError("synthetic mkdir collision")
        owned = "/data/local/tmp/adv-host-sample-" + "1" * 32

        def run_child(command, log, timeout):
            output = Path(command[command.index("--output") + 1])
            output.mkdir()
            budget.write_json(output / "session.json", {"remote_run_directory": owned})
            (output / "remote-owned.txt").write_text(owned, encoding="utf-8")
            return 0

        @contextmanager
        def lock(*args):
            yield math.inf

        cleanup = Mock(side_effect=[RuntimeError("synthetic cleanup failure"), None]
                       if cleanup_fails else None)
        with patch.dict(os.environ, ADV_SAMPLES=str(samples), ADV_ARTIFACT_ROOT=str(self.root)), \
                patch.object(budget, "SCENARIOS", schedules), \
                patch.object(budget.replay, "Adb", return_value=client), \
                patch.object(budget, "device_lock", lock), \
                patch.object(budget, "cleanup_remote", cleanup), \
                patch.object(budget, "run_child", side_effect=run_child), \
                patch.object(budget, "digest", return_value="a" * 64), \
                patch.object(budget.subprocess, "check_output", return_value=b"synthetic revision"), \
                patch("builtins.print"):
            try:
                budget.run(args)
            except RuntimeError:
                if not cleanup_fails:
                    raise
        return cleanup, owned

    def test_failed_remote_mkdir_does_not_authorize_recursive_cleanup(self):
        cleanup, _ = self.mocked_android_run(mkdir_fails=True)
        cleanup.assert_not_called()

    def test_one_cleanup_failure_does_not_skip_the_other_owned_directory(self):
        cleanup, owned = self.mocked_android_run(cleanup_fails=True)
        roots = [call.args[1] for call in cleanup.call_args_list]
        self.assertIn(owned, roots)
        self.assertTrue(any(root.startswith("/data/local/tmp/adv-host-bud-") for root in roots))


class WindowsProtocolAudit(SyntheticAudit):
    def run_fake_session(self, reply=None, report_changes=None):
        binary = self.root / "runner.txt"
        guest = self.root / "fixture.txt"
        binary.touch()
        guest.touch()
        scene = {"schema": "adv-scene-v1", "timeout_seconds": 15, "steps": [
            {"checkpoint": "title", "await": {
                "frame_fnv1a64": END_HASH, "timeout_ms": 1000, "hold_key": 17}}]}
        self.write("schedule.json", scene)
        output = self.root / "output"
        args = argparse.Namespace(binary=binary, exe=guest, schedule=self.root / "schedule.json",
                                  output=output, seed_data=None, audio=False, mode="headless",
                                  capture_audio=False, trace=False, tail_seconds=0, codepage=932,
                                  codec="native", sync_compilation=False,
                                  no_tiering=False, cold=True)

        class Runner:
            returncode = None

            def __init__(self):
                self.stdin = io.StringIO()
                self.stdout = io.StringIO("\n".join([
                    reply or gate_reply(), "control ok report", "control ok capture",
                    "control ok close"]) + "\n")

            def poll(self):
                return self.returncode

            def wait(self, timeout=None):
                self.returncode = 0
                return 0

        def launch(*args, **kwargs):
            budget.write_json(output / "title.json", checkpoint(**(report_changes or {})))
            budget.write_json(output / "final.json", checkpoint())
            return Runner()

        with patch.object(play_session.subprocess, "Popen", side_effect=launch), patch("builtins.print"):
            play_session.run(args)

    def test_windows_rejects_the_wrong_acknowledgement_verb(self):
        with self.assertRaises(RuntimeError):
            self.run_fake_session(reply="control ok key-up 17")

    def test_windows_gate_checks_ack_hash_frame_and_release(self):
        with self.assertRaises(RuntimeError):
            self.run_fake_session(reply=gate_reply(frame_hash=START_HASH, release_posted="false"))

    def test_windows_report_must_match_the_frozen_gate_receipt(self):
        with self.assertRaises(RuntimeError):
            self.run_fake_session(report_changes={"frames": 23,
                                  "scene_gate": {"frozen": False, "pending": True}})

    def test_failed_windows_checkpoint_is_not_performance_eligible(self):
        with self.assertRaises(RuntimeError):
            self.run_fake_session(report_changes={"failure": "synthetic failure"})
        metadata = budget.load_json(self.root / "output" / "session.json")
        self.assertFalse(metadata["suitable_for_performance_comparison"])


class TransportClassificationAudit(unittest.TestCase):
    def test_push_failed_is_a_retryable_transport_marker(self):
        self.assertTrue(budget.replay.Adb.transient_transport_error("adb: Push failed"))


if __name__ == "__main__":
    unittest.main()
