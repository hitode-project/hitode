import argparse
import importlib.util
import io
import json
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("replay_sample", Path(__file__).with_name("replay-sample.py"))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


class FakeProcess:
    def __init__(self, responses):
        self.stdin = io.StringIO()
        self.stdout = io.StringIO("\n".join(responses) + "\n")
        self.returncode = None

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        self.returncode = 0
        return 0

    def kill(self):
        self.returncode = -9


class DeviceConditionsTests(unittest.TestCase):
    def test_held_pointer_inputs_are_allowed_and_keep_their_acknowledgement(self):
        for command in ("mouse-down", "mouse-up"):
            self.assertIn(command, replay.INPUT_COMMANDS)
            self.assertTrue(replay.valid_acknowledgement(command + " 400 380", "control ok " + command))

    def test_temperature_and_display_state_do_not_include_window_identity(self):
        class Device:
            def call(self, *args, **kwargs):
                return subprocess.CompletedProcess(args, 0, "  temperature: 271\n  mWakefulness=Awake\n  mCurrentFocus=Window{abc123 u0 fixture/app}\nCpus_allowed_list:\t0-6\n", "")
        self.assertEqual(replay.device_conditions(Device()), {
            "battery_temperature_c": 27.1, "screen": "Awake", "foreground": "Window{u0 fixture/app}", "allowed_cpus": "0-6"})


class ReplayTests(unittest.TestCase):
    def test_precision_and_time_skip_switches_reach_device_runner(self):
        args = argparse.Namespace(exe_relative="synthetic.exe", codepage=932,
            codec="native", codec_kernels="native", no_tiering=False,
            schedule_profile=False, auto_dialog_ms=None, font_diagnostics=False,
            no_time_skip=True)
        command = replay.runner_command(args, "/fixture", "/game", 10)
        self.assertIn("--no-time-skip", command)
        args.no_time_skip = False
        command = replay.runner_command(args, "/fixture", "/game", 10)
        self.assertNotIn("--no-time-skip", command)
        self.assertNotIn("--no-code-cache", command)
        args.no_code_cache = True
        self.assertIn("--no-code-cache", replay.runner_command(args, "/fixture", "/game", 10))

    def test_scene_gate_ack_requires_the_exact_hash_new_frame_and_actual_release(self):
        command = "await-frame 0000000000000001 1000 hold 17"
        good = "control ok await-frame hash=0000000000000001 frame=5 start_frame=4 frozen=true release=17 release_posted=true receipt=6"
        self.assertTrue(replay.valid_acknowledgement(command, good))
        for bad in (good.replace("hash=0000000000000001", "hash=0000000000000002"),
                    good.replace("frame=5", "frame=4"), good.replace("frozen=true", "frozen=false"),
                    good.replace("release_posted=true", "release_posted=false"),
                    "control ok await-frame same-scene-name"):
            with self.subTest(bad=bad):
                self.assertFalse(replay.valid_acknowledgement(command, bad))
        self.assertTrue(replay.valid_acknowledgement("await-frame 0000000000000001 100 current",
            "control ok await-frame hash=0000000000000001 frame=4 start_frame=4 frozen=true release=none"))

    def test_scene_schema_rejects_unbounded_or_ambiguous_conditions(self):
        base = {"schema": "adv-scene-v1", "timeout_seconds": 30,
                "steps": [{"checkpoint": "target", "await": {
                    "frame_fnv1a64": "0123456789abcdef", "timeout_ms": 1000, "hold_key": 17}}]}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "scene.json"
            for amendment in ({"timeout_ms": 0}, {"timeout_ms": True}, {"hold_key": 256},
                              {"current": True}, {"release_key": 13}, {"unknown": 1},
                              {"probes": [{"x": 0, "y": 0, "bgra": [1, 2, 3, 4]}] * 17}):
                scene = json.loads(json.dumps(base))
                scene["steps"][0]["await"].update(amendment)
                path.write_text(json.dumps(scene))
                with self.subTest(amendment=amendment), self.assertRaises(ValueError):
                    replay.load_scene(path)
            path.write_text(json.dumps(base))
            scene = replay.load_scene(path)
            self.assertEqual(replay.await_command(scene["steps"][0]["await"]),
                             "await-frame 0123456789abcdef 1000 hold 17")

    def test_dotted_report_assertions_reject_missing_fields_and_boolean_counts(self):
        replay.assert_report({"pcm": {"playback_starts": 5}}, {"pcm.playback_starts": {"eq": 5, "min": 1}}, "good")
        for report in ({"pcm": {"playback_starts": 6}}, {"pcm": {"playback_starts": True}}, {}):
            with self.subTest(report=report), self.assertRaises(RuntimeError):
                replay.assert_report(report, {"pcm.playback_starts": {"eq": 5}}, "bad")

    def test_binary_independent_reference_requires_independent_approval_and_exact_binding(self):
        scene = [{"checkpoint": "target", "await": {"frame_fnv1a64": "0123456789abcdef"}}]
        binding = {"game_files": [{"sha256": "assets"}], "font_manifest": "fonts", "seed_files": "seed"}
        reference = {"schema": "adv-scene-reference-v1", "status": "recorded", "binding": binding,
                     "source_binary_sha256": "old binary", "checkpoints": {
                         "target": {"frame_fnv1a64": "0123456789abcdef", "assert": {}}}}
        with self.assertRaisesRegex(ValueError, "approve independently"):
            replay.validate_reference(reference, binding, scene)
        reference["status"] = "approved"
        replay.validate_reference(reference, binding, scene)
        reference["source_binary_sha256"] = "different binary"
        replay.validate_reference(reference, binding, scene)
        with self.assertRaisesRegex(ValueError, "binding differs"):
            replay.validate_reference(reference, {**binding, "font_manifest": "wrong font"}, scene)
        reference["checkpoints"]["target"]["frame_fnv1a64"] = "1111111111111111"
        with self.assertRaisesRegex(ValueError, "hash differs"):
            replay.validate_reference(reference, binding, scene)

    def test_device_manifest_requires_all_paths_and_exact_hashes(self):
        rows = [("/game/a name.exe", "a" * 64), ("/fonts/font.ttf", "b" * 64)]
        class Device:
            def __init__(self, response): self.response = response
            def shell(self, words): return self.response
        replay.verify_remote_hashes(Device("a" * 64 + "  /game/a name.exe\n" + "b" * 64 + "  /fonts/font.ttf"), rows)
        for response in ("a" * 64 + "  /game/a name.exe", "c" * 64 + "  /game/a name.exe\n" + "b" * 64 + "  /fonts/font.ttf"):
            with self.subTest(response=response), self.assertRaises(RuntimeError):
                replay.verify_remote_hashes(Device(response), rows)

    def test_reused_deployment_binds_actual_assets_and_ignores_undeployed_local_extras(self):
        rows = [{"path": "game.exe", "sha256": "a" * 64}, {"path": "unused-patch.bin", "sha256": "b" * 64}]
        class Device:
            def __init__(self, files, links=""): self.files, self.links = files, links
            def shell(self, words): return self.links if words[-1] == "l" else self.files
        actual = replay.deployed_asset_inventory(Device("/game/game.exe"), "/game", rows, "game.exe")
        self.assertEqual(actual, [rows[0]])
        for device in (Device("/game/game.exe\n/game/unknown.bin"), Device("/other/game.exe"),
                       Device("/game/game.exe", "/game/link"), Device("/game/unused-patch.bin")):
            with self.subTest(device=device), self.assertRaises(ValueError):
                replay.deployed_asset_inventory(device, "/game", rows, "game.exe")

    def test_relative_executable_cannot_escape_or_be_interpreted_as_a_drive(self):
        for value in ("../game.exe", "/game.exe", "C:\\game.exe", "a/../game.exe",
                      "a//game.exe", "./game.exe", "game.exe\nclose", "a/./game.exe"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                replay.relative_exe(value)
        self.assertEqual(replay.relative_exe("dll\\日本語.exe"), "dll/日本語.exe")

    def test_shell_arguments_preserve_quotes_and_metacharacters(self):
        words = ["sha256sum", "/data/local/tmp/game's $(touch BAD); 日本語/game.exe"]
        self.assertEqual(shlex.split(replay.shell_words(words)), words)
        script = replay.launch_script(["/run/adv-run", words[1]], "/run", "/game's", 45)
        self.assertIn("timeout -s KILL 45", script)
        self.assertIn("exit-code.txt", script)
        self.assertIn(shlex.quote(words[1]), script)
        self.assertNotIn("rm ", script)

    def test_remote_directory_refuses_ambiguous_traversal(self):
        for path in ("relative", "/data/../sdcard", "/data/./game", "/data/game\nclose"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                replay.remote_directory(path)

    def test_report_rejects_faults_stubs_and_unpresented_frames(self):
        replay.validate_report({"frames": 1, "api": {"mode": "strict", "stub_calls": 0,
                                                    "acceptance_eligible": True}}, "good")
        for report in ({"frames": 0}, {"frames": True}, {"frames": 2, "failure": "fault"},
                       {"frames": 2, "screenshot_error": "missing"},
                       {"frames": 2, "api": {"stub_calls": 1}},
                       {"frames": 2, "api": {"mode": "discovery"}},
                       {"frames": 2, "api": {"mode": "strict", "stub_calls": 0}},
                       {"frames": 2, "api": None}):
            with self.subTest(report=report), self.assertRaises(RuntimeError):
                replay.validate_report(report, "bad")

    def test_shared_schedule_validation_rejects_bad_time_order(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "schedule.json"
            path.write_text('[{"at":2,"command":"close"},{"at":1,"checkpoint":"late"}]')
            with self.assertRaises(ValueError):
                replay.load_schedule(path)

    def run_mocked(self, *, bad_checkpoint=False, bad_exit=False, bad_ack=False, missing_final=False,
                   font_diagnostics=False, failed_evidence=None, dialog_controls=False, experimental=False):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "adv-run"
            binary.write_bytes(b"mocked arm64 binary")
            game = root / "game"
            game.mkdir()
            (game / "game.exe").write_bytes(b"mocked PE")
            seed = root / "seed"
            seed.mkdir()
            (seed / "savedata.bin").write_bytes(b"original immutable save")
            schedule = root / "schedule.json"
            extra_steps = [{"at": 0, "command": "dialog-track 1001 0"},
                           {"at": 0, "command": "dialog-close"}] if dialog_controls else []
            schedule.write_text(json.dumps(extra_steps + [
                {"at": 0, "checkpoint": "story", "reset_metrics": True},
                {"at": 0, "command": "close"}]))
            output = root / "target" / "session"
            args = argparse.Namespace(adb=root / "adb", adb_port=5039, serial="fixture-serial",
                                      binary=binary, game_dir=game, exe_relative="game.exe",
                                      remote_game_dir="/data/local/tmp/shared game's source",
                                      seed_data=seed, schedule=schedule, codepage=932, output=output,
                                      codec="native", codec_kernels="guest", no_tiering=True,
                                      schedule_profile=False, font_diagnostics=font_diagnostics,
                                      auto_dialog_ms=None, tail_seconds=3,
                                      timeout_grace=15, ack_timeout=1, transfer_timeout=1)
            if experimental:
                args.tier1, args.compile_threads, args.cpu_mask = "bridge", 4, "7f"
            operations = []
            hashes = [replay.sha256(binary), replay.sha256(game / "game.exe")]
            good_report = {"frames": 4, "failure": None, "api": {"mode": "strict", "stub_calls": 0,
                                                                  "acceptance_eligible": True},
                           "frame_fnv1a64": "abcdef"}

            class FakeAdb:
                def __init__(self, args):
                    self.prefix = ["adb", "-P", str(args.adb_port), "-s", args.serial]

                def shell(self, words):
                    operations.append(("shell", words))
                    if words[0] == "find":
                        return "" if words[-1] == "l" else "/device/read-only-game/game.exe"
                    if words[0] == "sha256sum":
                        return hashes.pop(0) + "  " + words[1]
                    return "4096" if words[0] == "getconf" else "fixture"

                def push(self, source, destination):
                    operations.append(("push", str(source), destination))

                def pull(self, source, destination):
                    operations.append(("pull", source, str(destination)))
                    name = source.rsplit("/", 1)[-1]
                    if name == failed_evidence:
                        # A failed adb pull can leave a partial file. Its mere
                        # existence must not make the evidence complete.
                        destination.write_bytes(b"partial")
                        raise RuntimeError("transport exhausted after connection reset")
                    if name == "exit-code.txt":
                        destination.write_text("137" if bad_exit else "0")
                    elif name.endswith(".json"):
                        if missing_final and name == "final.json":
                            raise RuntimeError("no final report on device")
                        report = dict(good_report)
                        if bad_checkpoint and name == "story.json":
                            report["failure"] = "guest fault"
                        destination.write_text(json.dumps(report))
                    else:
                        destination.write_bytes(b"fixture evidence")

            responses = ["control error missing" if bad_ack else "control ok report /run/story.json",
                         "control ok capture /run/story.png"]
            if dialog_controls:
                responses = ["control ok dialog 1001", "control ok dialog-close"] + responses
            if not (bad_checkpoint or bad_ack):
                responses += ["control ok reset-metrics start_frame=4", "control ok close"]
            else:
                responses += ["control ok key-up 17", "control ok report /run/blocked.json",
                              "control ok capture /run/blocked.png", "control ok close"]
            process = FakeProcess(responses)
            with patch.object(replay, "REPO", root), patch.object(replay, "Adb", FakeAdb), \
                    patch.object(replay.subprocess, "Popen", return_value=process) as popen:
                if bad_checkpoint or bad_exit or bad_ack or missing_final or failed_evidence:
                    with self.assertRaises(RuntimeError):
                        replay.run(args)
                else:
                    replay.run(args)
                launch = popen.call_args.args[0]
            metadata = json.loads((output / "session.json").read_text(encoding="utf-8"))
            actions = json.loads((output / "actions.json").read_text(encoding="utf-8"))
            self.assertEqual((seed / "savedata.bin").read_bytes(), b"original immutable save")
            self.assertEqual(metadata["seed_files"][0]["sha256"], replay.sha256(seed / "savedata.bin"))
            self.assertIn("-T", launch)
            self.assertIn("timeout -s KILL 30", launch[-1])
            if experimental:
                self.assertIn("taskset 7f env ADV_TIER1=bridge ADV_COMPILE_THREADS=4", launch[-1])
            if font_diagnostics:
                self.assertIn("ADV_TEXT_DIAGNOSTICS=1", launch[-1])
            else:
                self.assertNotIn("ADV_TEXT_DIAGNOSTICS", launch[-1])
            self.assertTrue(metadata["remote_run_directory"].startswith("/data/local/tmp/adv-host-sample-"))
            self.assertTrue(metadata["remote_executable_hashes_verified"])
            self.assertFalse(any(row[0] == "push" and row[1] == str(game) for row in operations))
            self.assertFalse(any(row[0] == "shell" and row[1][0] in ("rm", "cp") for row in operations))
            self.assertEqual(len([row for row in operations if row[0] == "push"]), 2)
            return metadata, actions, {path.name for path in output.iterdir()}

    def test_reused_source_and_seed_are_preserved_while_checkpoints_are_pulled(self):
        metadata, actions, files = self.run_mocked()
        self.assertTrue(metadata["completed"])
        self.assertEqual([row["command"].split()[0] for row in actions],
                         ["report", "capture", "reset-metrics", "close"])
        self.assertEqual(metadata["remote_exit_code"], 0)
        self.assertTrue({"story.json", "story.png", "final.json", "final.png"}.issubset(files))

    def run_scene_mocked(self, *, wrong_hash=False, bad_assert=False, gate_timeout=False,
                         approve=False, record=False, cold=False, warm=False, capture_pcm=False,
                         code_cache=False):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "adv-run"
            binary.write_bytes(b"ARM host")
            game = root / "game"
            game.mkdir()
            (game / "game.exe").write_bytes(b"PE fixture")
            seed = root / "seed"
            seed.mkdir()
            (seed / "savedata.bin").write_bytes(b"immutable save")
            (seed / "jit-warm.bin").write_bytes(b"seed warm archive")
            warm_archive = root / "override-warm.bin"
            warm_archive.write_bytes(b"explicit warm override")
            native_archive = root / "native.bin"
            native_archive.write_bytes(b"authored native cache")
            font_manifest = root / "fonts.json"
            fonts = {"profile": {"path": "/fonts/profile.json", "sha256": "a" * 64},
                     "files": [{"path": "/fonts/fixture.ttf", "sha256": "b" * 64, "bytes": 100}]}
            font_manifest.write_text(json.dumps(fonts))
            schedule = root / "scene.json"
            target_hash = "0123456789abcdef"
            schedule.write_text(json.dumps({"schema": "adv-scene-v1", "timeout_seconds": 15, "steps": [
                {"checkpoint": "story", "await": {"frame_fnv1a64": target_hash, "timeout_ms": 1000,
                                                    "hold_key": 17},
                 "assert": {"pcm.playback_starts": {"eq": 5}}, "reset_metrics": True},
                {"command": "close"}]}))
            scene = replay.load_scene(schedule)
            binding = {"guest_exe_sha256": replay.sha256(game / "game.exe"),
                       "game_files": replay.inventory(game, hashes=True),
                       "seed_files": [row for row in replay.inventory(seed, hashes=True) if row["path"] != "jit-warm.bin"],
                       "font_manifest": fonts, "codepage": 932, "scene_sha256": replay.digest_json(scene)}
            reference = root / "approved.json"
            reference.write_text(json.dumps({"schema": "adv-scene-reference-v1", "status": "approved",
                "binding": binding, "source_binary_sha256": "independent old host",
                "checkpoints": {"story": {"frame_fnv1a64": target_hash,
                                           "assert": {"pcm.playback_starts": {"max": 5}}}}}))
            output = root / "target" / "session"
            args = argparse.Namespace(adb=root / "adb", adb_port=5039, serial="fixture-serial", binary=binary,
                game_dir=game, exe_relative="game.exe", remote_game_dir="/device/read-only-game", seed_data=seed,
                schedule=schedule, codepage=932, output=output, codec="native", codec_kernels="guest",
                no_tiering=True, schedule_profile=False, font_diagnostics=False, auto_dialog_ms=None,
                tail_seconds=3, timeout_grace=15, ack_timeout=1, transfer_timeout=1,
                scene_reference=reference if approve else None,
                record_scene_reference=output / "reference.json" if record else None,
                font_profile="/fonts/profile.json", font_manifest=font_manifest, cold=cold,
                warm_profile=warm_archive if warm else None, capture_pcm=capture_pcm,
                warm_code_cache=native_archive if code_cache and warm else None)
            operations = []
            report = {"frames": 4, "failure": None, "frame_fnv1a64": "bad hash" if wrong_hash else target_hash,
                      "api": {"mode": "strict", "stub_calls": 0, "acceptance_eligible": True},
                      "pcm": {"playback_starts": 6 if bad_assert else 5},
                      "scene_gate": {"frozen": True, "pending": False, "full_hash_checks": 1}}
            pcm_name = "1-00000002-3-0123456789abcdef.pcm"
            class Device:
                def __init__(self, args): self.prefix = ["adb"]
                def shell(self, words):
                    operations.append(("shell", words))
                    if words[0] == "find":
                        return "" if words[-1] == "l" else "/device/read-only-game/game.exe"
                    if words[0] == "sha256sum":
                        hashes = []
                        for name in words[1:]:
                            if name == "/fonts/profile.json": digest = "a" * 64
                            elif name == "/fonts/fixture.ttf": digest = "b" * 64
                            elif name.endswith("/adv-run"): digest = replay.sha256(binary)
                            elif name.endswith("/game.exe"): digest = replay.sha256(game / "game.exe")
                            elif name.endswith("/savedata.bin"): digest = replay.sha256(seed / "savedata.bin")
                            elif name.endswith("/jit-warm.bin"): digest = replay.sha256(warm_archive)
                            elif name.endswith("/jit-code.bin"): digest = replay.sha256(native_archive)
                            else: raise AssertionError(name)
                            hashes.append(digest + "  " + name)
                        return "\n".join(hashes)
                    return "4096" if words[0] == "getconf" else "fixture"
                def push(self, source, destination): operations.append(("push", str(source), destination))
                def pull(self, source, destination):
                    operations.append(("pull", source, str(destination)))
                    name = source.rsplit("/", 1)[-1]
                    if name.endswith(".json"): destination.write_text(json.dumps(report))
                    elif name == "exit-code.txt": destination.write_text("0")
                    elif name == "audio.log": destination.write_text(
                        "start us=1 id=00000002 generation=3 bytes=4 hash=0123456789abcdef base=0 loop=0 rate=44100 channels=1 bits=16\n")
                    elif name == pcm_name: destination.write_bytes(b"pcm!")
                    elif name == "jit-warm.bin": destination.write_bytes(b"produced warm archive")
                    else: destination.write_bytes(b"fixture evidence")
            gate_ack = ("control error await-frame timeout target=" + target_hash if gate_timeout else
                        "control ok await-frame hash=" + target_hash +
                        " frame=4 start_frame=3 frozen=true release=17 release_posted=true receipt=8")
            responses = [gate_ack]
            if not gate_timeout:
                responses += ["control ok report /run/story.json", "control ok capture /run/story.png"]
            if wrong_hash or bad_assert or gate_timeout:
                responses += ["control ok key-up 17", "control ok report /run/blocked.json",
                              "control ok capture /run/blocked.png", "control ok close"]
            else:
                responses += ["control ok reset-metrics start_frame=4", "control ok close"]
            process = FakeProcess(responses)
            with patch.object(replay, "REPO", root), patch.object(replay, "Adb", Device), \
                    patch.object(replay.subprocess, "Popen", return_value=process):
                if wrong_hash or bad_assert or gate_timeout:
                    with self.assertRaises(RuntimeError): replay.run(args)
                else:
                    replay.run(args)
            metadata = json.loads((output / "session.json").read_text())
            actions = json.loads((output / "actions.json").read_text())
            recorded = json.loads((output / "reference.json").read_text()) if record else None
            self.assertEqual((seed / "jit-warm.bin").read_bytes(), b"seed warm archive")
            return metadata, actions, operations, recorded

    def test_scene_verification_has_an_exact_frozen_receipt_and_binary_independent_reference(self):
        metadata, actions, _, _ = self.run_scene_mocked(approve=True)
        self.assertTrue(metadata["completed"])
        self.assertTrue(metadata["fixed_workload_verified"])
        self.assertEqual(metadata["scene_validation_status"], "validated")
        self.assertTrue(metadata["remote_asset_seed_hashes_verified"])
        self.assertTrue(metadata["remote_font_hashes_verified"])
        self.assertEqual(actions[0]["command"], "await-frame 0123456789abcdef 1000 hold 17")
        self.assertTrue(metadata["checkpoints"][0]["scene_gate"]["frozen"])
        self.assertIsNotNone(metadata["warm_output_sha256"])

    def test_first_scene_recording_cannot_claim_validation_or_performance_eligibility(self):
        metadata, _, _, reference = self.run_scene_mocked(record=True)
        self.assertTrue(metadata["completed"])
        self.assertFalse(metadata["fixed_workload_verified"])
        self.assertFalse(metadata["suitable_for_performance_comparison"])
        self.assertEqual(reference["status"], "recorded")

    def test_scene_hash_mismatch_assertion_failure_and_timeout_keep_failure_evidence(self):
        for option in ("wrong_hash", "bad_assert", "gate_timeout"):
            with self.subTest(option=option):
                metadata, actions, operations, _ = self.run_scene_mocked(**{option: True}, approve=True)
                self.assertFalse(metadata["completed"])
                self.assertEqual(metadata["scene_validation_status"], "failed")
                self.assertFalse(metadata["fixed_workload_verified"])
                self.assertEqual(actions[-1]["command"], "close")
                self.assertTrue(any(row[0] == "pull" and row[1].endswith("/blocked.png") for row in operations))

    def test_cold_and_warm_change_only_the_new_overlay_and_recover_produced_archive(self):
        for mode in ("cold", "warm"):
            with self.subTest(mode=mode):
                metadata, _, operations, _ = self.run_scene_mocked(**{mode: True}, approve=True)
                self.assertEqual(metadata["warm_mode"], mode)
                self.assertTrue(metadata["warm_profile_input_verified"])
                self.assertIsNotNone(metadata["warm_output_sha256"])
                if mode == "cold":
                    deletion = [row[1] for row in operations if row[0] == "shell" and row[1][0] == "rm"]
                    self.assertEqual(len(deletion), 1)
                    self.assertTrue(deletion[0][-1].startswith("/data/local/tmp/adv-host-sample-"))
                    self.assertTrue(deletion[0][-2].endswith("/data/jit-warm.bin"))
                    self.assertTrue(deletion[0][-1].endswith("/data/jit-code.bin"))
                    self.assertIsNone(metadata["warm_input_sha256"])
                else:
                    self.assertTrue(any(row[0] == "push" and row[2].endswith("/data/jit-warm.bin") for row in operations))
                    self.assertIsNotNone(metadata["warm_input_sha256"])

    def test_native_cache_warm_import_is_verified_and_cold_output_is_recovered(self):
        for mode in ("cold", "warm"):
            with self.subTest(mode=mode):
                metadata, _, operations, _ = self.run_scene_mocked(**{mode: True}, code_cache=True)
                self.assertNotIn("--no-code-cache", metadata["command"])
                self.assertIsNotNone(metadata["code_cache_output_sha256"])
                self.assertTrue(any(row[0] == "pull" and row[1].endswith("/jit-code.bin") for row in operations))
                imports = [row for row in operations if row[0] == "push" and row[2].endswith("/data/jit-code.bin")]
                self.assertEqual(len(imports), int(mode == "warm"))

    def test_pcm_capture_is_bounded_snapshot_diagnostics_and_excludes_performance_comparison(self):
        metadata, _, operations, _ = self.run_scene_mocked(approve=True, capture_pcm=True)
        self.assertTrue(metadata["pcm_evidence_complete"])
        self.assertFalse(metadata["suitable_for_performance_comparison"])
        self.assertEqual(len(metadata["pcm_captures"]), 1)
        command = metadata["command"]
        self.assertIn("ADV_AUDIO_DUMP_CHUNKS", command)
        self.assertIn("-u", command)
        self.assertTrue(any(word.startswith("ADV_AUDIO_TRACE=") for word in command))
        self.assertFalse(any(row[0] == "pull" and "/chunks-" in row[1] for row in operations))

    def test_dialog_track_actual_reply_does_not_stop_replay(self):
        metadata, actions, _ = self.run_mocked(dialog_controls=True)
        self.assertTrue(metadata["completed"])
        self.assertEqual(actions[0]["command"], "dialog-track 1001 0")
        self.assertEqual(actions[0]["ack"], "control ok dialog 1001")
        self.assertEqual(actions[1]["ack"], "control ok dialog-close")
        self.assertEqual(actions[-1]["command"], "close")

    def test_command_reply_verbs_match_shell_protocol_and_keep_token_boundaries(self):
        replies = {
            "click 1 2": "click 1 2", "move 1 2": "move 1 2", "key 13": "key 13",
            "key-down 17": "key-down 17", "key-up 17": "key-up 17",
            "mouse-down 1 2": "mouse-down 1 2", "mouse-up 1 2": "mouse-up 1 2",
            "menus": 'menus enabled=true "items"', "menu 40005": "menu 40005",
            "dialogs": 'dialogs "controls"', "dialog 1": "dialog 1",
            "dialog-track 1001 0": "dialog 1001", "dialog-close": "dialog-close",
            "memory 0x1000 1": "memory 0x00001000 ff",
            "reset-metrics": "reset-metrics start_frame=3", "close": "close",
            "report /run/test.json": "report /run/test.json",
            "capture /run/test.png": "capture /run/test.png",
        }
        self.assertEqual({line.split()[0] for line in replies}, replay.INPUT_COMMANDS | {"report", "capture"})
        for line, response in replies.items():
            with self.subTest(command=line):
                self.assertTrue(replay.valid_acknowledgement(line, "control ok " + response))
                self.assertFalse(replay.valid_acknowledgement(line, "control error rejected"))
                self.assertFalse(replay.valid_acknowledgement(line, None))
        self.assertFalse(replay.valid_acknowledgement("dialog 1", "control ok dialog-close"))
        self.assertFalse(replay.valid_acknowledgement("key 13", "control ok key-down 13"))

    def test_guest_fault_retains_checkpoint_and_blocked_evidence(self):
        metadata, actions, files = self.run_mocked(bad_checkpoint=True)
        self.assertFalse(metadata["completed"])
        self.assertIn("guest fault", metadata["error"])
        self.assertTrue({"story.json", "story.png", "blocked.json", "blocked.png"}.issubset(files))
        self.assertEqual(actions[-1]["command"], "close")

    def test_timeout_status_cannot_be_hidden_by_successful_tee(self):
        metadata, _, _ = self.run_mocked(bad_exit=True)
        self.assertFalse(metadata["completed"])
        self.assertEqual(metadata["adb_exit_code"], 0)
        self.assertEqual(metadata["remote_exit_code"], 137)

    def test_command_rejection_cannot_be_reported_as_completed(self):
        metadata, actions, _ = self.run_mocked(bad_ack=True)
        self.assertFalse(metadata["completed"])
        self.assertEqual(actions[0]["ack"], "control error missing")

    def test_missing_final_report_preserves_the_failure_metadata(self):
        metadata, _, _ = self.run_mocked(missing_final=True)
        self.assertFalse(metadata["completed"])
        self.assertIn("final.json", metadata["error"])

    def test_explicit_affinity_and_compiler_experiments_reach_the_device(self):
        self.run_mocked(experimental=True)

    def test_font_diagnostics_is_opt_in_and_marks_performance_comparison_ineligible(self):
        metadata, _, _ = self.run_mocked(font_diagnostics=True)
        self.assertTrue(metadata["font_diagnostics"])
        self.assertFalse(metadata["suitable_for_performance_comparison"])

    def test_required_logs_and_exit_status_must_be_successfully_pulled(self):
        for name in ("stderr.log", "stdout.log", "exit-code.txt"):
            with self.subTest(name=name):
                metadata, _, files = self.run_mocked(failed_evidence=name)
                self.assertFalse(metadata["completed"])
                self.assertFalse(metadata["evidence_complete"])
                self.assertEqual(metadata["missing_required_evidence"], [name])
                self.assertEqual(metadata["evidence_errors"][0]["file"], name)
                self.assertIn("final.json", files)
                self.assertIn("final.png", files)

    def adb_client(self):
        return replay.Adb(argparse.Namespace(adb=Path("adb"), adb_port=5037,
                                            serial="fixture-serial", transfer_timeout=1))

    def test_transport_failure_retries_idempotent_pull_at_most_twice(self):
        client = self.adb_client()
        results = [subprocess.CompletedProcess([], 1, "", "connection reset by peer"),
                   subprocess.CompletedProcess([], 1, "", "cannot connect to daemon (10060)"),
                   subprocess.CompletedProcess([], 0, "copied", "")]
        with patch.object(replay.subprocess, "run", side_effect=results) as execute, \
                patch.object(replay.time, "sleep") as sleep:
            client.pull("/run/stderr.log", Path("stderr.log"))
        self.assertEqual(execute.call_count, 3)
        self.assertEqual(sleep.call_count, 2)
        self.assertEqual([row["retry"] for row in client.transport_retries], [1, 2])

    def test_transport_failure_retry_is_bounded_after_repeated_failure(self):
        client = self.adb_client()
        failed = subprocess.CompletedProcess([], 1, "", "daemon still not running")
        with patch.object(replay.subprocess, "run", return_value=failed) as execute, \
                patch.object(replay.time, "sleep"), self.assertRaises(RuntimeError):
            client.pull("/run/stderr.log", Path("stderr.log"))
        self.assertEqual(execute.call_count, 3)
        self.assertEqual(len(client.transport_retries), 2)

    def test_nontransport_error_does_not_retry_even_for_pull(self):
        client = self.adb_client()
        failed = subprocess.CompletedProcess([], 1, "", "remote object does not exist")
        with patch.object(replay.subprocess, "run", return_value=failed) as execute, \
                self.assertRaises(RuntimeError):
            client.pull("/run/stderr.log", Path("stderr.log"))
        self.assertEqual(execute.call_count, 1)
        self.assertEqual(client.transport_retries, [])

    def test_directory_creation_cannot_be_replayed_after_unknown_transport_result(self):
        client = self.adb_client()
        failed = subprocess.CompletedProcess([], 1, "", "connection reset by peer")
        with patch.object(replay.subprocess, "run", return_value=failed) as execute, \
                self.assertRaises(RuntimeError):
            client.shell(["mkdir", "/run/new"])
        self.assertEqual(execute.call_count, 1)
        self.assertEqual(client.transport_retries, [])

    def test_directory_creation_can_retry_when_server_connection_never_existed(self):
        client = self.adb_client()
        results = [subprocess.CompletedProcess([], 1, "", "cannot connect to daemon (10060)"),
                   subprocess.CompletedProcess([], 0, "", "")]
        with patch.object(replay.subprocess, "run", side_effect=results) as execute, \
                patch.object(replay.time, "sleep") as sleep:
            client.shell(["mkdir", "/run/new"])
        self.assertEqual(execute.call_count, 2)
        sleep.assert_called_once_with(30)

    def test_unidentified_subprocess_timeout_does_not_retry(self):
        client = self.adb_client()
        with patch.object(replay.subprocess, "run",
                          side_effect=subprocess.TimeoutExpired(["adb", "pull"], 1)) as execute, \
                self.assertRaises(subprocess.TimeoutExpired):
            client.pull("/run/stderr.log", Path("stderr.log"))
        self.assertEqual(execute.call_count, 1)


if __name__ == "__main__":
    unittest.main()
