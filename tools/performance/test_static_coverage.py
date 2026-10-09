"""Test static discovery and aggregate-only sampled coverage with authored data.

Usage: python -m unittest discover -s tools/performance -p test_static_coverage.py
Inputs: synthetic PE metadata/CFG/sample dictionaries. Output: test statistics.
No game, native process, disassembler or guest-address artifact is required.
"""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import static_coverage as coverage


class StaticCoverageTests(unittest.TestCase):
    def test_image_selection_prefers_actual_executable_and_rejects_ambiguity(self):
        with tempfile.TemporaryDirectory() as temporary:
            game = Path(temporary).resolve()
            for sub in ("current", "backup"):
                (game / sub).mkdir()
                (game / sub / "main.exe").touch()
                (game / sub / "audio.dll").touch()
            exe = game / "current" / "main.exe"
            self.assertEqual(coverage.locate_image(game, str(exe), "main.exe", True), exe)
            self.assertEqual(coverage.locate_image(game, str(exe), "audio.dll", False), exe.parent / "audio.dll")
            self.assertIsNone(coverage.locate_image(game, "unknown/main.exe", "audio.dll", False))

    def test_seeds_include_tls_ordinal_exports_and_relocated_jump_candidates(self):
        words = {300: 150, 320: 1, 328: 200, 200: 50,
                 400: 100, 404: 12, 408: 0x3000, 410: 0x3004,
                 100: 120, 104: 140, 512: 250, 150: 160, 154: 0}
        module = SimpleNamespace(image_base=100, entry=10,
            sections=[dict(flags=0x20000000, rva=10, raw_size=90)],
            directories=[(300, 40), *[(0, 0)] * 4, (400, 12), *[(0, 0)] * 3, (500, 24)],
            word=lambda rva, width=4: words.get(rva, 0))
        roots, counts = coverage.seeds(module)
        self.assertEqual(roots, {110, 120, 140, 150, 160})
        self.assertEqual(counts["export"], 1)
        self.assertEqual(counts["tls"], 1)
        self.assertEqual(counts["adjacent_relocated_pointer_pairs"], 1)

    def test_closure_handles_calls_both_branches_indirect_jumps_and_padding(self):
        rows = [(10, "call", "0x20"), (12, "jne", "0x28"), (14, "jmp", "eax"),
                (16, "nop", ""), (32, "ret", ""), (40, "mov", "eax, 1"), (42, "ret", "")]
        self.assertEqual(coverage.closure(rows, {10}), {10, 12, 14, 32, 40, 42})
        self.assertEqual(coverage.closure([(1, "nop", ""), (100, "ret", "")], {1}), {1})

    def test_rebased_samples_group_game_dlls_and_never_export_pcs_or_paths(self):
        images = [dict(base=1000, preferred_base=100, size=50, group="primary", closure={110}),
                  dict(base=2000, preferred_base=200, size=50, group="dll", closure={220}),
                  dict(base=3000, preferred_base=3000, size=50, group="host_image")]
        result = coverage.aggregate([dict(pc=p, path="PRIVATE") for p in (1010, 1010, 1020, 2020, 3030, 4000, 0)], images)
        self.assertAlmostEqual(result["primary"]["sampled_coverage_percent"], 200 / 3)
        self.assertEqual(result["primary"]["unique_sampled_pcs"], 2)
        self.assertEqual(result["dll"]["sampled_coverage_percent"], 100)
        self.assertEqual(result["host_image"]["samples"], 1)
        self.assertEqual(result["non_image"]["samples"], 1)
        self.assertNotIn("PRIVATE", json.dumps(result))
        self.assertIsNone(coverage.aggregate([dict(pc=1010)], [images[0] | {"closure": set()}])["dll"]["sampled_coverage_percent"])


if __name__ == "__main__":
    unittest.main()
