"""Authored evidence tests; no game assets, device, or codec execution."""
import importlib.util
import io
import json
import math
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("codec_pcm_compare", Path(__file__).with_name("codec-pcm-compare.py"))
codec = importlib.util.module_from_spec(spec)
spec.loader.exec_module(codec)


class EvidenceTests(unittest.TestCase):
    def test_fnv_matches_a_known_external_test_vector(self):
        self.assertEqual(codec.fnv1a64(b"hello"), "a430d84680aabd0b")

    def make_run(self, root, name, values, *, bits=16, channels=1, extra_start=False):
        directory = root / name
        (directory / "pcm").mkdir(parents=True)
        raw = struct.pack("<" + "h" * len(values), *values) if bits == 16 else bytes(values)
        digest = codec.fnv1a64(raw)
        path = directory / "pcm" / f"100-00000001-1-{digest}.pcm"
        path.write_bytes(raw)
        primary = ("start us=0 id=00000002 generation=1 storage=00000000 bytes=0 "
                   "hash=cbf29ce484222325 base=0 loop=1 rate=44100 channels=2 bits=16\n")
        start = (f"start us=100 id=00000001 generation=1 storage=00001000 bytes={len(raw)} "
                 f"hash={digest} base=0 loop=0 rate=44100 channels={channels} bits={bits}\n")
        (directory / "audio.tsv").write_text(primary + start + (start if extra_start else ""))
        return directory, path

    def test_known_s16_error_and_snr_includes_every_sample(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [100, -100, 0, 0])
            b, _ = self.make_run(root, "b", [101, -101, 0, 1])
            ia = codec.inventory(a, "audio.tsv", "pcm")
            ib = codec.inventory(b, "audio.tsv", "pcm")
            result = codec.compare(ia[0], ib[0], {"label": "known"}, a, b, "pcm")
            self.assertEqual(result["samples"], 4)
            self.assertEqual(result["different_samples"], 3)
            self.assertEqual(result["max_error_lsb"], 1)
            self.assertEqual(result["reference_active_samples"], 2)
            self.assertEqual(result["active_samples"], 3)
            self.assertEqual(result["error_energy_lsb_squared"], 3)
            self.assertAlmostEqual(result["rms_lsb"], math.sqrt(3 / 4))
            self.assertEqual(result["rms_active_lsb"], 1)
            self.assertAlmostEqual(result["snr_db"], 10 * math.log10(20_000 / 3))

    def test_u8_silence_and_error_use_unsigned_offset(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [128, 0, 255], bits=8)
            b, _ = self.make_run(root, "b", [128, 1, 254], bits=8)
            ia = codec.inventory(a, "audio.tsv", "pcm")
            ib = codec.inventory(b, "audio.tsv", "pcm")
            self.assertEqual(ia[0]["active_samples"], 2)
            result = codec.compare(ia[0], ib[0], {"label": "u8"}, a, b, "pcm")
            self.assertEqual(result["different_samples"], 2)
            self.assertEqual(result["max_error_lsb"], 1)
            self.assertEqual(result["reference_energy_lsb_squared"], 128 ** 2 + 127 ** 2)

    def test_frame_range_respects_interleaved_stereo(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [1, 2, 3, 4, 5, 6], channels=2)
            b, _ = self.make_run(root, "b", [100, 100, 3, 4, 100, 100], channels=2)
            ia = codec.inventory(a, "audio.tsv", "pcm")
            ib = codec.inventory(b, "audio.tsv", "pcm")
            pair = {"label": "middle", "first_frame": 1, "frames": 1}
            result = codec.compare(ia[0], ib[0], pair, a, b, "pcm")
            self.assertEqual(result["samples"], 2)
            self.assertTrue(result["bit_exact"])
            with self.assertRaises(ValueError):
                codec.compare(ia[0], ib[0], {**pair, "frames": 3}, a, b, "pcm")

    def test_missing_corrupt_truncated_and_duplicate_snapshots_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, path = self.make_run(root, "a", [1, 2])
            path.unlink()
            with self.assertRaisesRegex(ValueError, "audio.tsv:2"):
                codec.inventory(a, "audio.tsv", "pcm")
            path.write_bytes(struct.pack("<hh", 1, 3))
            with self.assertRaisesRegex(ValueError, "FNV"):
                codec.inventory(a, "audio.tsv", "pcm")
            path.write_bytes(struct.pack("<h", 1))
            with self.assertRaisesRegex(ValueError, "length"):
                codec.inventory(a, "audio.tsv", "pcm")
            b, _ = self.make_run(root, "b", [1, 2], extra_start=True)
            with self.assertRaisesRegex(ValueError, "duplicate"):
                codec.inventory(b, "audio.tsv", "pcm")

    def test_invalid_metadata_and_frame_length_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [1, 2, 3], channels=2)
            with self.assertRaisesRegex(ValueError, "incomplete frame"):
                codec.inventory(a, "audio.tsv", "pcm")
            b, _ = self.make_run(root, "b", [1, 2])
            text = (b / "audio.tsv").read_text().replace("rate=44100", "rate=0")
            (b / "audio.tsv").write_text(text)
            with self.assertRaisesRegex(ValueError, "invalid PCM"):
                codec.inventory(b, "audio.tsv", "pcm")

    def test_snapshot_change_between_inventory_and_comparison_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [1, 2])
            b, path = self.make_run(root, "b", [1, 2])
            ia = codec.inventory(a, "audio.tsv", "pcm")
            ib = codec.inventory(b, "audio.tsv", "pcm")
            path.write_bytes(struct.pack("<hh", 1, 3))
            with self.assertRaisesRegex(ValueError, "changed after inventory"):
                codec.compare(ia[0], ib[0], {"label": "changed"}, a, b, "pcm")

    def test_every_excluded_event_needs_a_reason_and_every_event_is_accounted(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest = Path(temp) / "pairs.json"
            data = {"pairs": [{"label": "fixed", "reference": 1, "candidate": 1}],
                    "exclusions": [{"side": "reference", "index": 0, "reason": "startup source"},
                                   {"side": "candidate", "index": 0, "reason": "startup source"}]}
            manifest.write_text(json.dumps(data))
            pairs, exclusions = codec.pair_manifest(manifest, [{}, {}], [{}, {}])
            self.assertEqual(len(pairs), 1)
            self.assertEqual(len(exclusions), 2)
            data["exclusions"].pop()
            manifest.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "every candidate"):
                codec.pair_manifest(manifest, [{}, {}], [{}, {}])
            data["exclusions"][0]["reason"] = ""
            manifest.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "missing reason"):
                codec.pair_manifest(manifest, [{}, {}], [{}, {}])

    def test_default_pairing_rejects_event_count_and_metadata_changes(self):
        with self.assertRaisesRegex(ValueError, "different start counts"):
            codec.pair_manifest(None, [{}], [{}, {}])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [1, 2])
            b, _ = self.make_run(root, "b", [1, 2, 3])
            ia = codec.inventory(a, "audio.tsv", "pcm")
            ib = codec.inventory(b, "audio.tsv", "pcm")
            with self.assertRaisesRegex(ValueError, "bytes differs"):
                codec.compare(ia[0], ib[0], {"label": "length"}, a, b, "pcm")
            ib[0].update(bytes=ia[0]["bytes"], channels=2)
            with self.assertRaisesRegex(ValueError, "channels differs"):
                codec.compare(ia[0], ib[0], {"label": "channels"}, a, b, "pcm")

    def test_cli_enforces_explicit_contract_and_refuses_silent_only_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            a, _ = self.make_run(root, "a", [100, -100])
            b, _ = self.make_run(root, "b", [101, -101])
            argv = ["codec", "--reference", str(a), "--candidate", str(b), "--max-error", "0"]
            for maximum, expected in (("0", 1), ("1", 0)):
                argv[-1] = maximum
                stdout = io.StringIO()
                with patch.object(codec.sys, "argv", argv), patch("sys.stdout", stdout):
                    self.assertEqual(codec.main(), expected)
                report = json.loads(stdout.getvalue())
                self.assertEqual(report["eligible"], expected == 0)
                self.assertEqual(report["contract"]["max_error_lsb"], int(maximum))
            argv += ["--min-snr-db", "50"]
            with patch.object(codec.sys, "argv", argv), patch("sys.stdout", io.StringIO()):
                self.assertEqual(codec.main(), 1)  # The authored signal has 40 dB SNR.
            silent, _ = self.make_run(root, "silent", [0, 0])
            argv = ["codec", "--reference", str(silent), "--candidate", str(silent), "--max-error", "0"]
            stdout = io.StringIO()
            with patch.object(codec.sys, "argv", argv), patch("sys.stdout", stdout):
                self.assertEqual(codec.main(), 1)
            self.assertIn("entirely silent", json.loads(stdout.getvalue())["failures"][0])

    def test_cli_missing_evidence_returns_error_without_a_success_report(self):
        with tempfile.TemporaryDirectory() as temp:
            a, path = self.make_run(Path(temp), "a", [1, 2])
            path.unlink()
            argv = ["codec", "--reference", str(a), "--candidate", str(a), "--max-error", "0"]
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch.object(codec.sys, "argv", argv), patch("sys.stdout", stdout), patch("sys.stderr", stderr):
                self.assertEqual(codec.main(), 2)
            report = json.loads(stdout.getvalue())
            self.assertFalse(report["eligible"])
            self.assertTrue(report["evidence_errors"])
            self.assertIn("evidence error", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
