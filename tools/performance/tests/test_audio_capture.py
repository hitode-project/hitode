"""Small authored captures test measurement semantics, without an audio device."""
import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest

import numpy as np

SCRIPT = Path(__file__).resolve().parents[1] / "analyze_audio_capture.py"
SPEC = importlib.util.spec_from_file_location("audio_capture_analyzer", SCRIPT)
ANALYZER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ANALYZER
SPEC.loader.exec_module(ANALYZER)


class MeasurementSemantics(unittest.TestCase):
    def test_zeros_invalid_values_and_supply_counters_are_independent(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            np.array([[.25, .25], [0, 0], [0, 0], [0, 0],
                      [float("nan"), 0], [.3, .3], [float("inf"), .2], [0, .5]],
                     dtype="<f4").tofile(directory / "actual.f32")
            text = "device rate=1000 channels=2 hardware_muted=true first_file_sample=0\n"
            for i in range(4):
                text += (f"callback={i} frame_offset=0 pts_us={i * 2000} playback_delay_us=0 "
                         f"host_us={i * 2000} file_sample={i * 4} samples=4 lock_miss=false "
                         f"missing_output_frames={2 if i >= 2 else 0} "
                         f"missing_stream_frames={3 if i >= 2 else 0}\n")
            (directory / "actual.timeline.txt").write_text(text)
            args = SimpleNamespace(directory=directory, output=directory / "out",
                                   pcm="actual.f32", timeline="actual.timeline.txt",
                                   min_zero_ms=.25, jump=.02, source_rms=.0001,
                                   source_peak=.001, sources=False)
            with contextlib.redirect_stdout(io.StringIO()):
                report = ANALYZER.analyze(args)
            self.assertEqual(report["waveform"]["nonfinite_samples"], 2)
            self.assertEqual(report["waveform"]["exact_zero_frames"], 3)
            self.assertEqual(report["timeline"]["callback_missing_output_frame_deltas"], 2)
            self.assertEqual(report["timeline"]["callback_missing_stream_frame_deltas"], 3)
            self.assertEqual(report["waveform"]["recorded_seconds"], .008)
            # Legacy captures have no callback counters. Optional source mode
            # still works when no source trace was recorded.
            legacy = "\n".join(line.split(" missing_output_frames=")[0] for line in text.splitlines())
            (directory / "actual.timeline.txt").write_text(legacy)
            args.sources = True
            with contextlib.redirect_stdout(io.StringIO()):
                legacy_report = ANALYZER.analyze(args)
            self.assertFalse(legacy_report["timeline"]["callback_missing_counters_available"])
            self.assertIsNone(legacy_report["timeline"]["callback_missing_output_frame_deltas"])
            self.assertIsNone(legacy_report["timeline"]["callback_missing_stream_frame_deltas"])
            self.assertEqual(legacy_report["waveform"], report["waveform"])

    def test_late_stop_and_handle_reuse_cannot_extend_natural_end(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / "sound.log").write_text(
                "start us=1000000 id=00000001 generation=1 rate=8000 channels=1 bits=8 bytes=8000 base=0 loop=0\n"
                "start us=3000000 id=00000002 generation=1 rate=8000 channels=1 bits=8 bytes=8000 base=0 loop=0\n"
                "api us=5000000 method=Stop args=[00000001, 00000000]\n"
                "start us=8000000 id=00000002 generation=2 rate=8000 channels=1 bits=8 bytes=8000 base=0 loop=0\n")
            sources, _ = ANALYZER.source_log(directory)
            self.assertEqual(sources[0].end_us, 2000000)
            self.assertEqual(sources[1].end_us, 4000000)
            self.assertEqual(sources[2].end_us, 9000000)


if __name__ == "__main__":
    unittest.main()
