"""One owned Windows capture smoke test; set ADV_API_DIFF_BUILD to build output.

The trace and subprocess output are written beneath that directory. Tests verify
static/dynamic calls, ANSI/wide strings, terminal flush, LastError and two threads.
No games or private input are used.
"""
import os
from pathlib import Path
import subprocess
import unittest


@unittest.skipUnless(os.name == "nt" and os.environ.get("ADV_API_DIFF_BUILD"),
                     "requires Windows and ADV_API_DIFF_BUILD")
class NativeCaptureTests(unittest.TestCase):
    def test_owned_fixture(self):
        build = Path(os.environ["ADV_API_DIFF_BUILD"]).resolve()
        trace = build / "fixture.trace"
        command = [str(build / "native_capture.exe"), "--dll", str(build / "native_trace.dll"),
                   "--output", str(trace), "--seconds", "3", "--max", "1000", "--",
                   str(build / "api_trace_fixture.exe")]
        result = subprocess.run(command, capture_output=True, timeout=40)
        (build / "fixture-launch.log").write_bytes(result.stdout + result.stderr)
        self.assertEqual(result.returncode, 0, result.stdout.decode(errors="replace"))
        self.assertIn(b"target_exit=0", result.stdout)
        lines = trace.read_text(encoding="ascii").splitlines()
        rows = [r.split("\t") for r in lines if r and not r.startswith("#")]
        self.assertTrue(rows)
        self.assertTrue(all(len(row) == 6 for row in rows))
        self.assertTrue(any("kernel32.dll!CreateEventA" == row[2] and "s8:" in row[3] for row in rows))
        self.assertTrue(any("kernel32.dll!CreateEventW" == row[2] and "s16:" in row[3] for row in rows))
        self.assertTrue(any("winmm.dll!timeGetTime" == row[2] for row in rows))
        self.assertTrue(any("ntdll.dll!RtlLockHeap" == row[2] and row[4] == "1" for row in rows))
        self.assertGreaterEqual(len({row[0] for row in rows}), 2)
        self.assertTrue(any(line.startswith("# terminal") and "ExitProcess" in line for line in lines))
        self.assertTrue(any(line.startswith("# child_untraced") for line in lines))
        self.assertLess(len(rows), 100)


if __name__ == "__main__":
    unittest.main()
