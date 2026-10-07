#!/usr/bin/env python3
"""Test replay diagnostics with a fake runner; no game or audio device is used.

Run: python -m unittest discover -s tools/performance -p test_play_session.py
Inputs are authored temporary files; output is the unittest result.
"""
import argparse
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import play_session


class ReplayDiagnosticsTest(unittest.TestCase):
    def test_trace_is_forwarded_to_isolated_output_and_excludes_performance(self):
        scratch = Path(__file__).resolve().parents[2] / "target/scratch/play-session-tests"
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
                    codec="native", guest_workers=1, sync_compilation=False, no_tiering=False)

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
