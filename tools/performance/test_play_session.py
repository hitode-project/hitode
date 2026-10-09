"""Test replay audio and diagnostics without a game or audio device.

Run: python -m unittest discover -s tools/performance -p test_play_session.py
Inputs are authored temporary files; output is the unittest result.
"""
import argparse
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import play_session


DIAGNOSTICS = (
    "ADV_AUDIO_TRACE", "ADV_AUDIO_DUMP_DIR", "ADV_PRINTF_DIAG_PATH", "ADV_PRINTF_DIAG_WORDS",
    "ADV_AUDIO_DUMP_CHUNKS", "ADV_AUDIO_CAPTURE", "ADV_AUDIO_CAPTURE_CALLBACK",
    "ADV_AUDIO_MUTE_DEVICE", "ADV_MMIO_TRACE", "ADV_JIT_DUMP_DIR",
)


class FakeProcess:
    def __init__(self):
        self.stdout = io.StringIO("control ok key-up 17\n")
        self.stdin = io.StringIO()
        self.returncode = None

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        self.returncode = 0
        return 0


class PlaySessionTests(unittest.TestCase):
    def test_failure_preserves_the_frozen_scene_before_final_close(self):
        commands = []
        play_session.preserve_failure(commands.append, Path("fixture"))
        self.assertEqual(commands[0], "key-up 17")
        self.assertTrue(commands[1].startswith("report "))
        self.assertTrue(commands[1].endswith("blocked.json"))
        self.assertTrue(commands[2].endswith("blocked.png"))
        self.assertNotIn("close", commands)

    def test_native_cache_import_and_switches_use_only_the_new_overlay(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native = root / "jit-code.bin"
            native.write_bytes(b"authored native cache")
            args = self.args(root, compile_threads=4, warm_code_cache=native)
            (command, kwargs), _ = self.launch(args)
            self.assertNotIn("--no-code-cache", command)
            self.assertEqual(kwargs["env"]["ADV_COMPILE_THREADS"], "4")
            self.assertEqual((args.output / "overlay/jit-code.bin").read_bytes(), native.read_bytes())

    def test_native_cache_diagnostic_bypass_reaches_the_runner(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(Path(directory), no_code_cache=True)
            (command, _), _ = self.launch(args)
            self.assertIn("--no-code-cache", command)

    def test_optical_mounts_and_pending_dialogs_reach_runner_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(Path(directory))
            args.cdrom = ["D=authored image.iso", "F=second.iso"]
            args.auto_dialog_ms = 0
            (command, _), metadata = self.launch(args)
            mounts = [command[i + 1] for i, value in enumerate(command) if value == "--cdrom"]
            self.assertEqual(mounts, args.cdrom)
            self.assertEqual(command[command.index("--auto-dialog-ms") + 1], "0")
            self.assertEqual(metadata["command"], command)

    def test_shader_profiles_are_only_forwarded_when_explicit(self):
        for sm2, sm3 in ((False, False), (True, False), (False, True)):
            with tempfile.TemporaryDirectory() as directory:
                args = self.args(Path(directory), d3d9_sm2=sm2, d3d9_sm3=sm3)
                (command, _), metadata = self.launch(args)
                self.assertEqual("--d3d9-sm2" in command, sm2)
                self.assertEqual("--d3d9-sm3" in command, sm3)
                self.assertEqual(metadata["command"], command)

    def args(self, root, **changes):
        binary = root / "runner.exe"
        game = root / "fixture.exe"
        schedule = root / "schedule.json"
        binary.write_bytes(b"Mocked runner, never executed")
        game.write_bytes(b"Authored mock guest, never executed")
        schedule.write_text(json.dumps([{"at": 0, "command": "key-up 17"}]), encoding="utf-8")
        values = dict(binary=binary, exe=game, schedule=schedule, output=root / "output",
                      seed_data=None, audio=True, mute_audio=False, capture_audio=False,
                      mode="hidden", tail_seconds=0, codepage=932, codec="native",
                      sync_compilation=False, no_tiering=False, trace=False)
        values.update(changes)
        return argparse.Namespace(**values)

    def launch(self, args):
        calls = []

        def invoke(command, **kwargs):
            calls.append((command, kwargs))
            report = Path(command[command.index("--report") + 1])
            report.write_text(json.dumps({"frames": 1, "audio": {
                "device_requested": True, "non_silent_samples": 64,
                "hardware_muted": kwargs["env"].get("ADV_AUDIO_MUTE_DEVICE") == "1",
            }}), encoding="utf-8")
            return FakeProcess()

        inherited = {name: "inherited-diagnostic" for name in DIAGNOSTICS}
        inherited["ADV_TEST_UNRELATED"] = "preserved"
        with patch.dict(os.environ, inherited, clear=True), \
                patch("play_session.subprocess.Popen", side_effect=invoke), \
                patch("builtins.print"):
            play_session.run(args)
        self.assertEqual(len(calls), 1)
        metadata = json.loads((args.output / "session.json").read_text(encoding="utf-8"))
        return calls[0], metadata

    def test_muted_hidden_and_window_launch_keep_device_without_diagnostics(self):
        for mode in ("hidden", "window"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                args = self.args(Path(directory), mode=mode, mute_audio=True)
                (command, kwargs), metadata = self.launch(args)
                self.assertIn("--audio", command)
                self.assertNotIn("--headless", command)
                self.assertEqual("--hidden" in command, mode == "hidden")
                self.assertEqual(kwargs["env"]["ADV_AUDIO_MUTE_DEVICE"], "1")
                for name in DIAGNOSTICS:
                    if name != "ADV_AUDIO_MUTE_DEVICE":
                        self.assertNotIn(name, kwargs["env"])
                self.assertEqual(kwargs["env"]["ADV_TEST_UNRELATED"], "preserved")
                self.assertTrue(metadata["muted"])
                self.assertTrue(metadata["audio_device_expected"])
                self.assertFalse(metadata["diagnostic_capture"])
                self.assertTrue(metadata["suitable_for_performance_comparison"])
                self.assertEqual(set(metadata["removed_diagnostics"]), set(DIAGNOSTICS))
                self.assertEqual(metadata["exit_code"], 0)

    def test_default_audio_modes_still_scrub_mute_and_capture(self):
        for mode, audio in (("hidden", True), ("window", True), ("headless", False)):
            with self.subTest(mode=mode, audio=audio), tempfile.TemporaryDirectory() as directory:
                args = self.args(Path(directory), mode=mode, audio=audio)
                (command, kwargs), metadata = self.launch(args)
                self.assertEqual("--audio" in command, audio)
                for name in DIAGNOSTICS:
                    self.assertNotIn(name, kwargs["env"])
                self.assertFalse(metadata["muted"])
                self.assertFalse(metadata["diagnostic_capture"])
                self.assertTrue(metadata["suitable_for_performance_comparison"])

    def test_muting_without_audio_or_in_headless_mode_fails_before_launch(self):
        for mode, audio in (("hidden", False), ("window", False), ("headless", True)):
            with self.subTest(mode=mode, audio=audio), tempfile.TemporaryDirectory() as directory:
                args = self.args(Path(directory), mode=mode, audio=audio, mute_audio=True)
                with patch("play_session.subprocess.Popen") as launch:
                    with self.assertRaisesRegex(ValueError, "--mute-audio requires --audio and --mode hidden/window"):
                        play_session.run(args)
                    launch.assert_not_called()
                self.assertFalse(args.output.exists())

    def test_explicit_capture_retains_its_existing_muted_diagnostic_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(Path(directory), capture_audio=True)
            (_, kwargs), metadata = self.launch(args)
            self.assertEqual(kwargs["env"]["ADV_AUDIO_MUTE_DEVICE"], "1")
            self.assertEqual(kwargs["env"]["ADV_AUDIO_CAPTURE_CALLBACK"], str(args.output / "actual.f32"))
            self.assertIn("ADV_AUDIO_TRACE", kwargs["env"])
            self.assertIn("ADV_AUDIO_DUMP_DIR", kwargs["env"])
            self.assertTrue(metadata["muted"])
            self.assertTrue(metadata["diagnostic_capture"])
            self.assertFalse(metadata["suitable_for_performance_comparison"])


class ReplayDiagnosticsTest(unittest.TestCase):
    def test_trace_is_forwarded_to_isolated_output_and_excludes_performance(self):
        scratch = Path(os.environ.get("ADV_ARTIFACT_ROOT", Path(__file__).resolve().parents[2] / "target")) / "scratch/play-session-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        for trace in (False, True):
            with self.subTest(trace=trace), tempfile.TemporaryDirectory(dir=scratch) as temporary:
                root = Path(temporary)
                binary, game = root / "runner.exe", root / "authored.exe"
                binary.write_bytes(b"authored runner")
                game.write_bytes(b"authored guest")
                schedule = root / "schedule.json"
                schedule.write_text('[{"at":0,"command":"key 13"}]', encoding="utf-8")
                output = root / "session"
                args = argparse.Namespace(binary=binary, exe=game, schedule=schedule,
                    output=output, seed_data=None, mode="hidden", codepage=932,
                    audio=False, capture_audio=False, trace=trace, tail_seconds=0,
                    codec="native", sync_compilation=False, no_tiering=False)

                class Runner:
                    returncode = None
                    stdout = io.StringIO("control ok key 13\n")
                    stdin = io.StringIO()

                    def poll(self):
                        return self.returncode

                    def wait(self, timeout=None):
                        self.returncode = 0
                        (output / "final.json").write_text('{}', encoding="utf-8")
                        return 0

                with patch.object(play_session.subprocess, "Popen", return_value=Runner()) as launch:
                    play_session.run(args)
                command = launch.call_args.args[0]
                metadata = json.loads((output / "session.json").read_text(encoding="utf-8"))
                self.assertEqual(command, metadata["command"])
                self.assertEqual(metadata["diagnostic_api_trace"], trace)
                self.assertEqual(metadata["suitable_for_performance_comparison"], not trace)
                self.assertEqual(metadata["exit_code"], 0)
                self.assertEqual(len(json.loads((output / "actions.json").read_text())), 1)
                if trace:
                    self.assertEqual(command[command.index("--trace") + 1], str(output / "api.trace"))
                else:
                    self.assertNotIn("--trace", command)


if __name__ == "__main__":
    unittest.main()
