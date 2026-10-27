import copy
import argparse
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from sample_matrix import (analyze_report, diagnostic_completeness, milestones, quick_samples, run,
                           sample_exe, validate_native_baseline)


class CampaignTests(unittest.TestCase):
    def test_sample_root_is_not_written_to_probe_metadata_or_runner_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            samples = root / "private-samples"
            game = samples / "title" / "fixture.exe"
            game.parent.mkdir(parents=True)
            game.write_bytes(b"Mocked guest")
            binary = root / "runner.exe"
            binary.write_bytes(b"Mocked runner")
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [{"id": "one", "exe": "title/fixture.exe"}]}))

            def launch(command, **kwargs):
                self.assertEqual(command[1], "fixture.exe")
                self.assertEqual(kwargs["cwd"], game.parent)
                self.assertIn("--wine", command)
                self.assertIn("--codec-kernels", command)
                report = Path(command[command.index("--report") + 1])
                report.write_text(json.dumps({"exe": command[1], "frames": 1}))
                return SimpleNamespace(wait=lambda timeout: 0)

            args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, binary=binary,
                                      output=root / "output", seconds=1, timeout_grace=1,
                                      mode="hidden", codec="native", trace=False)
            with patch.dict(os.environ, {"ADV_SAMPLES": str(samples)}), \
                    patch("sample_matrix.subprocess.Popen", side_effect=launch):
                run(args)
            for path in args.output.rglob("*"):
                if path.is_file():
                    content = path.read_text(encoding="utf-8")
                    self.assertNotIn(str(samples), content)
                    self.assertNotIn(str(samples).replace("\\", "\\\\"), content)
            result = json.loads((args.output / "results.json").read_text())["samples"][0]
            self.assertEqual(result["command"][1], "$ADV_SAMPLES/title/fixture.exe")

    def test_relative_samples_resolve_under_the_samples_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "fixture.exe").write_bytes(b"fixture")
            with patch.dict(os.environ, {"ADV_SAMPLES": str(root)}):
                self.assertEqual(sample_exe({"exe": "fixture.exe"}), root / "fixture.exe")


    def test_quick_gate_prefers_marked_samples_then_each_engines_first(self):
        samples = [{"id": "a1", "engine": "A"}, {"id": "a2", "engine": "A"},
                   {"id": "b1", "engine": "B"}, {"id": "n1"}]
        self.assertEqual(quick_samples(samples), {"a1", "b1", "n1"})
        samples[1]["quick"] = True
        self.assertEqual(quick_samples(samples), {"a2"})

    def test_min_seconds_requires_a_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [
                {"id": "one", "exe": str(root / "x.exe"), "min_seconds": 5}]}), encoding="utf-8")
            args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, quick=False)
            with self.assertRaisesRegex(ValueError, "min_seconds_reason"):
                run(args)

    def test_relative_exes_resolve_only_inside_adv_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            game = root / "samples" / "game" / "fixture.exe"
            game.parent.mkdir(parents=True)
            game.write_bytes(b"")
            sample = {"id": "one", "exe": "game/fixture.exe"}
            with patch.dict("os.environ", {"ADV_SAMPLES": str(root / "samples")}):
                self.assertEqual(sample_exe(sample), game)
                with self.assertRaisesRegex(ValueError, "escapes ADV_SAMPLES"):
                    sample_exe({"id": "two", "exe": "../outside.exe"}, strict=False)
            with patch.dict("os.environ", {}, clear=True):
                with self.assertRaisesRegex(ValueError, "needs ADV_SAMPLES"):
                    sample_exe(sample)
                self.assertEqual(sample_exe({"id": "abs", "exe": str(game)}), game)

    def test_quick_run_launches_only_the_gate_and_records_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            game = root / "game" / "fixture.exe"
            game.parent.mkdir()
            game.write_bytes(b"Not executed: subprocess is mocked")
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [
                {"id": name, "exe": str(game), "engine": engine}
                for name, engine in (("one", "A"), ("two", "A"), ("three", "B"))
            ]}), encoding="utf-8")
            data = json.loads(manifest.read_text(encoding="utf-8"))
            data["samples"][2].update(min_seconds=5, min_seconds_reason="late first frame")
            manifest.write_text(json.dumps(data), encoding="utf-8")
            launched = []

            def launch(command, **kwargs):
                launched.append(Path(command[command.index("--report") + 1]).parent.name)
                return SimpleNamespace(wait=lambda timeout: 0)

            args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, binary=game,
                                      output=root / "output", seconds=1, timeout_grace=1, quick=True,
                                      mode="hidden", codec="native", trace=False)
            with patch("sample_matrix.subprocess.Popen", side_effect=launch):
                run(args)
            self.assertEqual(launched, ["one", "three"])
            results = json.loads((root / "output" / "results.json").read_text())["samples"]
            self.assertEqual([result["seconds"] for result in results], [1, 5])
            self.assertTrue(json.loads((root / "output" / "results.json").read_text())["quick_gate"])
            args.output, args.only = root / "other", ["two"]
            with self.assertRaisesRegex(ValueError, "no quick-gate samples"):
                run(args)

    def test_multiple_samples_keep_review_evidence_separate_from_failure_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            game = root / "game" / "fixture.exe"
            game.parent.mkdir()
            game.write_bytes(b"Not executed: subprocess is mocked")
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [
                {"id": name, "exe": str(game)} for name in ("one", "two")
            ]}), encoding="utf-8")

            def launch(command, **kwargs):
                self.assertIn("--hidden", command)
                self.assertIn("--audio", command)
                self.assertEqual(kwargs["env"]["ADV_AUDIO_MUTE_DEVICE"], "1")
                self.assertEqual(Path(command[command.index("--data-dir") + 1]).parent.parent,
                                 root / "output")
                report = Path(command[command.index("--report") + 1])
                report.write_text(json.dumps({"frames": 1, "startup": {"loaded": True, "guest_windows": 1},
                                               "api": {"mode": "discovery"}}), encoding="utf-8")
                return SimpleNamespace(wait=lambda timeout: 0)

            args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, binary=game,
                                      output=root / "output", seconds=1, timeout_grace=1,
                                      mode="hidden", codec="native", trace=False)
            with patch("sample_matrix.subprocess.Popen", side_effect=launch):
                run(args)
            result = json.loads((args.output / "results.json").read_text(encoding="utf-8"))
            self.assertEqual(len(result["samples"]), 2)
            self.assertTrue(all(row["milestones"]["stage_reached"] == "S1" for row in result["samples"]))
            self.assertTrue(all(row["milestones"]["accepted_stage"] is None for row in result["samples"]))
            self.assertTrue(all(row["blocker_api"] is None for row in result["samples"]))
            self.assertTrue(all(row["diagnostic_completeness"]["version"] is None for row in result["samples"]))

    def test_campaign_copies_elapsed_time_so_sparse_time_limit_frames_remain_undiagnosed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            game = root / "game" / "fixture.exe"
            game.parent.mkdir()
            game.write_bytes(b"Mocked executable")
            manifest = root / "samples.json"
            manifest.write_text(json.dumps({"samples": [{"id": "sparse", "exe": str(game)}]}), encoding="utf-8")

            def launch(command, **kwargs):
                report = Path(command[command.index("--report") + 1])
                report.write_text(json.dumps({"stop_reason": "time_limit", "frames": 3, "elapsed_seconds": 30,
                                               "api": {"mode": "discovery"}}), encoding="utf-8")
                return SimpleNamespace(wait=lambda timeout: 0)

            args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, binary=game,
                                      output=root / "output", seconds=30, timeout_grace=1,
                                      mode="hidden", codec="native", trace=False)
            with patch("sample_matrix.subprocess.Popen", side_effect=launch):
                run(args)
            result = json.loads((args.output / "results.json").read_text(encoding="utf-8"))["samples"][0]
            self.assertEqual(result["status"], "frames_observed")
            self.assertEqual(result["elapsed_seconds"], 30)
            self.assertEqual(result["observations"]["frame_rate_observed"], 0.1)
            self.assertTrue(result["observations"]["low_frame_observation"])
            self.assertEqual(result["observations"]["interpretation"], "undetermined")
            self.assertIsNone(result["failure_kind"])

    def test_exit_diagnostic_hint_names_only_available_reports_and_optional_trace(self):
        for trace in (False, True):
            with self.subTest(trace=trace), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                game = root / "game" / "fixture.exe"
                game.parent.mkdir()
                game.write_bytes(b"Mocked executable")
                manifest = root / "samples.json"
                manifest.write_text(json.dumps({"samples": [{"id": "authored", "exe": str(game)}]}), encoding="utf-8")

                def launch(command, **kwargs):
                    self.assertEqual("--trace" in command, trace)
                    report = Path(command[command.index("--report") + 1])
                    report.write_text(json.dumps({"stop_reason": "guest_exit", "guest_exit_code": 0,
                                                   "frames": 1}), encoding="utf-8")
                    return SimpleNamespace(wait=lambda timeout: 0)

                args = argparse.Namespace(wine=root, manifest=manifest, only=None, evidence=None, binary=game,
                                          output=root / "output", seconds=1, timeout_grace=1,
                                          mode="hidden", codec="native", trace=trace)
                with patch("sample_matrix.subprocess.Popen", side_effect=launch), patch("sample_matrix.print") as console:
                    run(args)
                hint = next(call.args[0] for call in console.call_args_list if "guest exited" in call.args[0])
                self.assertIn("report.json", hint)
                self.assertEqual("api.log" in hint, trace)


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

    def test_named_startup_error_disqualifies_but_declared_fixture_exit_keeps_abi_rules(self):
        self.report.update(stop_reason="guest_exit", guest_exit_code=0, frames=0)
        proof = {"report_sha256": "digest", "reviewer": "tester", "note": "Requested authored fixture exit.",
                 "exit_expectation": {"kind": "requested", "guest_exit_code": 0}}
        self.assertEqual(milestones(self.report, proof, "digest")["accepted_stage"], "S1")
        self.report["api"]["message_boxes"] = [{"caption": "Error", "text": "Required initialization failed.", "style": 0}]
        self.assertIsNone(milestones(self.report)["accepted_stage"])
        self.assertEqual(milestones(self.report, proof, "digest")["accepted_stage"], "S1")
        self.report["api"]["message_boxes"] = []
        self.report["api"]["stub_calls"] = 1
        self.assertIsNone(milestones(self.report, proof, "digest")["accepted_stage"])


class ClassificationTests(unittest.TestCase):
    def setUp(self):
        self.lookup = {"sequence": 10, "module": "mpr.dll", "name": "WNetGetConnectionA",
                       "failure_module": "mpr.dll", "failure_name": "WNetGetConnectionA",
                       "caller": 0x401010, "context": 0, "kind": "unimplemented",
                       "reason": "known export has no installed implementation"}
        self.box = {"sequence": 11, "api": "user32.dll!MessageBoxA", "caller": 0x401020,
                    "context": 0, "caption": "Startup error", "text": "Required function WNetGetConnectionA is unavailable.",
                    "style": 0x10, "result": 1, "preceding_lookup_failure": self.lookup}
        self.report = {"stop_reason": "guest_exit", "guest_exit_code": 1, "frames": 0,
                       "elapsed_seconds": 0.14, "failure": None,
                       "api": {"mode": "strict", "acceptance_eligible": True, "stub_calls": 0,
                               "called_gaps": {}, "recent_stubs": [], "signature_conflicts": [],
                               "discovery_diagnostics_version": 1, "truncated_diagnostic_fields": 0,
                               "dynamic_lookup_failures": [self.lookup], "dynamic_lookup_failure_count": 1,
                               "dropped_dynamic_lookup_failures": 0, "message_boxes": [],
                               "message_box_count": 0, "dropped_message_boxes": 0}}

    def terminal_error(self):
        self.report["api"].update(message_boxes=[self.box], message_box_count=1)
        return analyze_report(self.report)

    def test_gpa_zero_without_an_error_is_a_candidate_even_when_the_process_exits(self):
        result = analyze_report(self.report)
        self.assertEqual(result["status"], "startup_exit")
        self.assertIsNone(result["blocker_api"])
        self.assertEqual(len(result["dynamic_lookup_candidates"]), 1)
        self.assertEqual(result["called_stubs"], [])

    def test_optional_probe_with_fallback_and_frames_never_becomes_a_blocker(self):
        self.report.update(stop_reason="time_limit", frames=450, elapsed_seconds=20, guest_exit_code=None)
        result = analyze_report(self.report)
        self.assertEqual(result["status"], "frames_observed")
        self.assertIsNone(result["blocker_api"])
        self.assertIsNone(result["failure_kind"])

    def test_terminal_engine_message_names_a_prior_known_unimplemented_query(self):
        result = self.terminal_error()
        self.assertEqual(result["status"], "startup_error")
        self.assertEqual(result["failure_kind"], "engine_startup_error")
        self.assertEqual(result["blocker_api"], "mpr.dll!WNetGetConnectionA")
        self.assertEqual(result["blocker_evidence"]["kind"], "message_box_named_unimplemented_lookup")
        self.assertEqual(result["called_stubs"], [])

    def test_snapshot_can_survive_bounded_lookup_history_drops(self):
        self.report["api"].update(dynamic_lookup_failures=[], dynamic_lookup_failure_count=4,
                                  dropped_dynamic_lookup_failures=4)
        result = self.terminal_error()
        self.assertEqual(result["blocker_api"], "mpr.dll!WNetGetConnectionA")
        self.assertFalse(result["diagnostic_completeness"]["dynamic_lookups"]["complete"])
        self.assertEqual(result["diagnostic_completeness"]["dynamic_lookups"]["dropped_records"], 4)

    def report_shaped_startup_error(self):
        # Scalar counts and event ordering follow a supplied no-trace report.
        # Captions, call sites and optional probes are anonymous authored data.
        report = copy.deepcopy(self.report)
        lookup = dict(self.lookup, sequence=155, module="user32.dll", name="LoadAcceleratorsA",
                      failure_module="user32.dll", failure_name="LoadAcceleratorsA",
                      reason="user32.dll: export LoadAcceleratorsA not found")
        missing = [dict(self.lookup, sequence=sequence, module="ntdll.dll", name="ZwOpenFile",
                        failure_module="ntdll.dll", failure_name="ZwOpenFile", kind="missing_export",
                        reason="ntdll.dll: export ZwOpenFile not found") for sequence in range(92, 153)]
        probes = [dict(self.lookup, sequence=sequence, module="kernel32.dll", name=name,
                       failure_module="kernel32.dll", failure_name=name)
                  for sequence, name in ((153, "GetErrorMode"), (154, "CreateActCtxW"))]
        box = dict(self.box, sequence=156, frame=0, text="Cannot find function LoadAcceleratorsA in library USER32.dll",
                   preceding_lookup_failure=lookup)
        report.update(elapsed_seconds=0.145688, startup={"loaded": True, "guest_windows": 1})
        report["api"].update(dynamic_lookup_failures=missing + probes + [lookup],
                              dynamic_lookup_failure_count=155, dropped_dynamic_lookup_failures=91,
                              message_boxes=[box], message_box_count=1)
        return report

    def test_report_shaped_no_trace_startup_error_names_one_known_query_amid_failed_probes(self):
        report = self.report_shaped_startup_error()
        sample = {"sha256": "a" * 64, "native_baseline": {
            "exe_sha256": "a" * 64, "outcome": "playable", "source": "user", "reviewer": "user",
            "note": "The same entry supports normal native play."}}
        result = analyze_report(report, sample=sample)
        self.assertEqual(result["status"], "startup_error")
        self.assertEqual(result["failure_kind"], "engine_startup_error")
        self.assertEqual(result["blocker_api"], "user32.dll!LoadAcceleratorsA")
        self.assertEqual(result["called_stubs"], [])
        self.assertEqual(len(result["dynamic_lookup_candidates"]), 64)
        observed = result["diagnostic_completeness"]["dynamic_lookups"]
        self.assertEqual((observed["total"], observed["observed_records"], observed["dropped_records"]), (155, 64, 91))
        self.assertTrue(observed["counters_consistent"])
        self.assertFalse(observed["complete"])
        self.assertTrue(result["diagnostic_completeness"]["message_boxes"]["complete"])
        self.assertEqual(result["engine_messages"][0]["subsequent_frames_observed"], 0)
        self.assertEqual(result["native_comparison"], "guest_startup_differs_from_playable_native")
        self.assertNotIn("codepage", result["native_baseline"])
        self.assertIsNone(milestones(report)["accepted_stage"])

    def test_report_shaped_message_snapshot_retains_named_query_after_history_ring_overwrite(self):
        report = self.report_shaped_startup_error()
        unrelated = report["api"]["dynamic_lookup_failures"][0]
        report["api"].update(dynamic_lookup_failures=[dict(unrelated, sequence=sequence) for sequence in range(157, 221)],
                              dynamic_lookup_failure_count=219, dropped_dynamic_lookup_failures=155)
        result = analyze_report(report)
        self.assertEqual(result["status"], "startup_error")
        self.assertEqual(result["blocker_api"], "user32.dll!LoadAcceleratorsA")
        self.assertTrue(all(candidate["kind"] == "missing_export" for candidate in result["dynamic_lookup_candidates"]))
        self.assertEqual([match["sequence"] for match in result["blocker_evidence"]["queries"]], [155])
        self.assertTrue(result["diagnostic_completeness"]["dynamic_lookups"]["counters_consistent"])
        self.assertFalse(result["diagnostic_completeness"]["dynamic_lookups"]["complete"])
        report["api"]["message_boxes"][0]["preceding_lookup_failure"] = None
        self.assertIsNone(analyze_report(report)["blocker_api"])

    def test_unknown_export_resolution_module_or_initializer_failure_is_not_a_stub(self):
        for kind in ("missing_export", "invalid_module", "initialization_failed", "resolution_failed"):
            with self.subTest(kind=kind):
                self.lookup["kind"] = kind
                result = self.terminal_error()
                self.assertIsNone(result["blocker_api"])
                self.assertEqual(result["failure_kind"], "engine_startup_error")
                self.assertEqual(result["dynamic_lookup_candidates"][0]["kind"], kind)

    def test_ordinal_probe_cannot_be_promoted_by_nearby_error_text(self):
        self.lookup.update(name="#17", failure_name="WNetGetConnectionA")
        self.assertIsNone(self.terminal_error()["blocker_api"])

    def test_query_and_message_must_share_context_and_real_sequence_order(self):
        for context, sequence in ((1, 11), (0, 10), (0, 9), (None, 11), (0, None)):
            with self.subTest(context=context, sequence=sequence):
                self.box.update(context=context, sequence=sequence)
                self.assertIsNone(self.terminal_error()["blocker_api"])

    def test_named_function_uses_symbol_boundaries_and_matching_module(self):
        for text in ("WNetGetConnectionAOptional failed.", "PrefixWNetGetConnectionA failed.",
                     "another.dll!WNetGetConnectionA failed.", "A function could not initialize."):
            with self.subTest(text=text):
                self.box["text"] = text
                self.assertIsNone(self.terminal_error()["blocker_api"])

    def test_recent_unrelated_probe_is_not_used_when_error_names_an_older_query(self):
        newer = dict(self.lookup, sequence=12, module="kernel32.dll", name="GetSystemTimePreciseAsFileTime",
                     failure_module="kernel32.dll", failure_name="GetSystemTimePreciseAsFileTime")
        self.report["api"].update(dynamic_lookup_failures=[self.lookup, newer], dynamic_lookup_failure_count=2)
        self.box.update(sequence=13, preceding_lookup_failure=newer)
        self.assertEqual(self.terminal_error()["blocker_api"], "mpr.dll!WNetGetConnectionA")

    def test_same_symbol_in_multiple_modules_is_ambiguous(self):
        other = dict(self.lookup, sequence=9, module="other.dll", failure_module="other.dll")
        self.report["api"].update(dynamic_lookup_failures=[self.lookup, other], dynamic_lookup_failure_count=2)
        self.assertIsNone(self.terminal_error()["blocker_api"])

    def test_explicit_fallback_message_is_not_an_engine_startup_failure(self):
        self.box["text"] = "Optional probe WNetGetConnectionA failed; continuing with fallback."
        result = self.terminal_error()
        self.assertEqual(result["status"], "startup_exit")
        self.assertIsNone(result["blocker_api"])

    def test_nonterminal_message_and_failed_probe_do_not_explain_a_time_limit(self):
        self.report.update(stop_reason="time_limit", frames=1, elapsed_seconds=30, guest_exit_code=None)
        result = self.terminal_error()
        self.assertEqual(result["status"], "frames_observed")
        self.assertIsNone(result["blocker_api"])
        self.assertTrue(result["observations"]["low_frame_observation"])
        self.assertEqual(result["observations"]["interpretation"], "undetermined")

    def test_one_frame_exit_zero_is_an_unreviewed_startup_exit_not_healthy_frames(self):
        self.report.update(frames=1, guest_exit_code=0)
        result = analyze_report(self.report)
        self.assertEqual(result["status"], "startup_exit")
        self.assertEqual(result["failure_kind"], "guest_exit_unreviewed")
        self.assertTrue(result["observations"]["review_required"])
        self.assertIsNone(result["blocker_api"])

    def test_declared_fixture_and_reviewed_exit_are_not_classified_as_crashes(self):
        self.report["guest_exit_code"] = 0
        for kind, expected in (("requested", "expected_exit"), ("reviewed", "reviewed_exit")):
            proof = {"report_sha256": "digest", "reviewer": "tester", "note": "Confirmed requested exit.",
                     "exit_expectation": {"kind": kind, "guest_exit_code": 0}}
            result = analyze_report(self.report, review=proof, report_hash="digest")
            self.assertEqual(result["status"], expected)
            self.assertIsNone(result["failure_kind"])
            with self.assertRaises(ValueError):
                analyze_report(self.report, review=proof, report_hash="different")
        proof["exit_expectation"]["guest_exit_code"] = 7
        self.assertEqual(analyze_report(self.report, review=proof, report_hash="digest")["status"], "startup_exit")

    def test_called_stub_failure_is_distinct_from_a_failed_dynamic_lookup(self):
        self.report.update(stop_reason="guest_failure", failure="context 0 at 0x00401010: unimplemented: user32.dll!DialogBoxParamW (strict ABI stub)")
        self.report["api"].update(stub_calls=1, called_gaps={"user32.dll!DialogBoxParamW": 1})
        result = analyze_report(self.report)
        self.assertEqual(result["failure_kind"], "called_stub")
        self.assertEqual(result["blocker_api"], "user32.dll!DialogBoxParamW")
        self.assertEqual(result["called_stubs"][0]["count"], 1)
        self.assertEqual(len(result["dynamic_lookup_candidates"]), 1)

    def test_discovery_called_stub_with_fallback_is_recorded_without_causal_attribution(self):
        self.report.update(stop_reason="time_limit", frames=800, elapsed_seconds=20)
        self.report["api"].update(stub_calls=1, called_gaps={"user32.dll!DialogBoxParamW": 1})
        result = analyze_report(self.report)
        self.assertEqual(len(result["called_stubs"]), 1)
        self.assertIsNone(result["blocker_api"])

    def test_cpu_seh_and_runtime_errors_do_not_blame_the_nearest_failed_query(self):
        for text, kind in (("unhandled Windows exception 0xc0000005", "cpu_access_violation"),
                           ("nested frame-based exception dispatch is not implemented", "seh_dispatch"),
                           ("guest callback escaped its continuation stack; nonlocal unwind is unsupported", "seh_dispatch"),
                           ("unsupported instruction at 0x00401010", "unsupported_instruction"),
                           ("unsupported guest instruction Bswap_r16", "unsupported_instruction"),
                           ("cpu fault executing a translated region", "cpu_fault"),
                           ("guest reached the park address without a host continuation", "cpu_control_flow")):
            with self.subTest(kind=kind):
                self.report.update(stop_reason="guest_failure", failure="context 0 at 0x00401010: " + text)
                result = analyze_report(self.report)
                self.assertEqual(result["failure_kind"], kind)
                self.assertEqual(result["status"], "blocked")
                self.assertIsNone(result["blocker_api"])

    def test_error_followed_by_later_frame_progression_is_history_not_a_terminal_api_cause(self):
        self.report.update(frames=40, guest_exit_code=0)
        self.box["frame"] = 3
        result = self.terminal_error()
        self.assertEqual(result["status"], "early_exit")
        self.assertIsNone(result["blocker_api"])
        self.assertEqual(len(result["error_history"]), 1)
        self.assertEqual(result["engine_messages"][0]["subsequent_frames_observed"], 37)
        self.assertEqual(len(result["dynamic_lookup_candidates"]), 1)
        self.report["startup"] = {"loaded": True, "guest_windows": 1}
        self.assertEqual(milestones(self.report)["accepted_stage"], "S1")
        for kind, status in (("requested", "expected_exit"), ("reviewed", "reviewed_exit")):
            proof = {"report_sha256": "digest", "reviewer": "tester", "note": "Confirmed normal exit after continued frames.",
                     "exit_expectation": {"kind": kind, "guest_exit_code": 0}}
            confirmed = analyze_report(self.report, review=proof, report_hash="digest")
            self.assertEqual(confirmed["status"], status)
            self.assertIsNone(confirmed["blocker_api"])
            self.assertTrue(confirmed["error_history"][0]["later_frame_progression"])
            self.assertEqual(milestones(self.report, proof, "digest")["accepted_stage"], "S1")

    def test_reviewed_or_requested_exit_overrides_historical_error_without_hiding_real_failures(self):
        self.report.update(frames=1, guest_exit_code=0)
        self.terminal_error()
        for kind, status in (("requested", "expected_exit"), ("reviewed", "reviewed_exit")):
            proof = {"report_sha256": "digest", "reviewer": "tester", "note": "Confirmed normal exit.",
                     "exit_expectation": {"kind": kind, "guest_exit_code": 0}}
            result = analyze_report(self.report, review=proof, report_hash="digest")
            self.assertEqual(result["status"], status)
            self.assertIsNone(result["blocker_api"])
            self.assertEqual(len(result["error_history"]), 1)
            self.assertIsNone(result["engine_messages"][0]["later_frame_progression"])
        self.report.update(failure="context 0: unhandled Windows exception 0xc0000005")
        self.assertEqual(analyze_report(self.report, review=proof, report_hash="digest")["failure_kind"], "cpu_access_violation")

    def test_old_message_records_do_not_claim_frame_ordering_evidence(self):
        result = self.terminal_error()
        self.assertIsNone(result["engine_messages"][0]["subsequent_frames_observed"])
        self.assertIsNone(result["engine_messages"][0]["later_frame_progression"])

    def test_stderr_api_mentions_are_not_actual_calls(self):
        result = analyze_report(self.report, "optional probe returned zero: unimplemented: optional.dll!MaybeFunction\nMessageBoxA [Error] optional.dll!MaybeFunction")
        self.assertIsNone(result["blocker_api"])
        self.assertEqual(result["called_stubs"], [])
        self.assertEqual(len(result["unstructured_message_mentions"]), 1)

    def test_old_failure_report_is_compatible_but_does_not_claim_new_collection(self):
        result = analyze_report({"failure": "unimplemented: kernel32.dll!AuthoredFixtureApi"})
        self.assertEqual(result["blocker_api"], "kernel32.dll!AuthoredFixtureApi")
        self.assertIsNone(result["diagnostic_completeness"]["version"])
        self.assertIsNone(result["diagnostic_completeness"]["dynamic_lookups"]["total"])
        self.assertFalse(result["diagnostic_completeness"]["dynamic_lookups"]["complete"])

    def test_truncated_fields_and_dropped_message_records_are_explicit(self):
        self.report["api"].update(truncated_diagnostic_fields=1)
        result = self.terminal_error()
        self.assertIsNone(result["blocker_api"])
        self.assertEqual(result["diagnostic_completeness"]["truncated_fields"], 1)
        self.assertFalse(result["diagnostic_completeness"]["message_boxes"]["complete"])
        self.report["api"].update(message_box_count=5, dropped_message_boxes=4)
        result = analyze_report(self.report)
        self.assertEqual(result["diagnostic_completeness"]["message_boxes"]["total"], 5)
        self.assertEqual(result["diagnostic_completeness"]["message_boxes"]["observed_records"], 1)
        self.assertEqual(result["diagnostic_completeness"]["message_boxes"]["dropped_records"], 4)

    def test_counter_array_length_and_unknown_missing_fields_are_not_interchangeable(self):
        old = diagnostic_completeness({"dynamic_lookup_failures": []})
        self.assertIsNone(old["dynamic_lookups"]["total"])
        self.assertIsNone(old["dynamic_lookups"]["dropped_records"])
        self.assertFalse(old["dynamic_lookups"]["complete"])
        self.report["api"]["dynamic_lookup_failure_count"] = 99
        new = diagnostic_completeness(self.report["api"])
        self.assertFalse(new["dynamic_lookups"]["counters_consistent"])
        self.assertFalse(new["dynamic_lookups"]["complete"])
        self.report["api"].update(stub_calls=33, recent_stubs=[{}])
        self.assertFalse(diagnostic_completeness(self.report["api"])["called_stubs"]["recent_history_complete"])


class NativeBaselineTests(unittest.TestCase):
    def setUp(self):
        self.sample = {"sha256": "a" * 64, "codepage": 932,
                       "native_baseline": {"exe_sha256": "a" * 64, "outcome": "playable", "source": "user",
                                           "reviewer": "user", "note": "Native executable supports normal play."}}

    def test_user_native_playability_does_not_invent_a_locale(self):
        baseline = validate_native_baseline(self.sample)
        self.assertNotIn("codepage", baseline)
        for value in (None, "unknown", 932):
            self.sample["native_baseline"]["codepage"] = value
            self.assertEqual(validate_native_baseline(self.sample)["codepage"], value)

    def test_provenance_rejects_the_wrong_entry_digest_and_known_codepage_mismatch(self):
        self.sample["native_baseline"]["exe_sha256"] = "b" * 64
        with self.assertRaises(ValueError):
            validate_native_baseline(self.sample)
        self.sample["native_baseline"]["exe_sha256"] = "a" * 64
        self.sample["native_baseline"]["codepage"] = 936
        with self.assertRaises(ValueError):
            validate_native_baseline(self.sample)
        with self.assertRaises(ValueError):
            validate_native_baseline(self.sample, "b" * 64)

    def test_native_evidence_requires_an_identified_observer_and_note(self):
        for key in ("source", "reviewer", "note"):
            sample = copy.deepcopy(self.sample)
            sample["native_baseline"].pop(key)
            with self.assertRaises(ValueError):
                validate_native_baseline(sample)

    def test_user_validated_native_can_reveal_an_unattributed_guest_startup_discrepancy(self):
        # Sanitized scalar observations from the supplied startup report; no game
        # name, path, resources, or executable-specific classifier rule is used.
        report = {"stop_reason": "guest_exit", "guest_exit_code": 1, "elapsed_seconds": 0.138084,
                  "frames": 0, "first_frame_seconds": None, "failure": None,
                  "startup": {"loaded": True, "guest_windows": 0},
                  "api": {"mode": "discovery", "stub_calls": 0, "called_gaps": {}, "warnings": []}}
        result = analyze_report(report, sample=self.sample)
        self.assertEqual(result["status"], "startup_exit")
        self.assertEqual(result["native_comparison"], "guest_startup_differs_from_playable_native")
        self.assertIsNone(result["blocker_api"])
        self.assertEqual(result["dynamic_lookup_candidates"], [])
        self.assertNotIn("codepage", result["native_baseline"])

    def test_native_failed_entry_is_not_a_game_or_replacement_entry_failure(self):
        self.sample["native_baseline"]["outcome"] = "startup_failed"
        report = {"stop_reason": "guest_exit", "guest_exit_code": 0, "frames": 1}
        result = analyze_report(report, sample=self.sample)
        self.assertEqual(result["status"], "startup_exit")
        self.assertEqual(result["native_comparison"], "native_startup_also_failed")
        self.assertIsNone(result["blocker_api"])
        replacement = copy.deepcopy(self.sample)
        replacement["sha256"] = "b" * 64
        with self.assertRaises(ValueError):
            analyze_report(report, sample=replacement)


if __name__ == "__main__":
    unittest.main()
