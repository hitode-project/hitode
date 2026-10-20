"""Tests for where.py and triage_report.py against a throwaway source tree."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import triage_report  # noqa: E402
import where  # noqa: E402

TABLE = "api\troute\n<win32>!dll_init\tguest\nwinmm.dll!mixerOpen\tordinary\nMSVCRT.dll!fprintf\tordinary\n"
SOURCE = "// " + "spec: " + """FIXTURE-RULE-1
fn register() {
    for (name, argc, handler) in [
        ("mixerOpen", 5, mixer_open),
    ] {}
}

fn mixer_open() {}
"""


class Tree(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / "docs").mkdir()
        (self.root / "docs/host-call-boundary.tsv").write_text(TABLE)
        src = self.root / "crates/adv-win32/src"
        src.mkdir(parents=True)
        (src / "winmm.rs").write_text(SOURCE)
        tests = self.root / "crates/adv-win32/tests"
        tests.mkdir()
        (tests / "winmm.rs").write_text('call("mixerOpen");\n')

    def tearDown(self) -> None:
        self.directory.cleanup()


class Where(Tree):
    def test_registration_ignores_pseudo_modules_and_lowers_dlls(self) -> None:
        self.assertEqual(where.registered(self.root), {"mixerOpen": {"winmm.dll"}, "fprintf": {"msvcrt.dll"}})

    def test_finds_source_handler_specs_and_tests(self) -> None:
        place, = where.locate(["mixerOpen"], self.root)
        self.assertEqual(place.sources, ['crates/adv-win32/src/winmm.rs:4  ("mixerOpen", 5, mixer_open),'])
        self.assertEqual(place.handlers, ["crates/adv-win32/src/winmm.rs:8  fn mixer_open"])
        self.assertEqual(place.specs, ["FIXTURE-RULE-1"])
        self.assertEqual(place.tests, ["crates/adv-win32/tests/winmm.rs"])

    def test_reports_a_dll_mismatch_and_unregistered_names(self) -> None:
        mixer, picture = where.locate(["mixerOpen", "OleLoadPicture"], self.root)
        self.assertIn("(not under user32.dll)", where.render(mixer, "user32.dll"))
        self.assertEqual(where.render(picture, "oleaut32.dll"), "OleLoadPicture: not registered")


class Triage(Tree):
    def test_anonymous_api_failure_suggests_a_discovery_run_with_its_code_page(self) -> None:
        report = {"schema": "hitode.report/1", "game": {"codepage": 936},
                  "failure": {"kind": "api", "api": "winmm.dll!mixerOpen"}}
        text = triage_report.triage(report, self.root)
        self.assertIn("## winmm.dll (1)", text)
        self.assertIn("mixerOpen: registered in winmm.dll", text)
        self.assertIn("--codepage 936 --api-mode discovery", text)

    def test_run_report_groups_every_missing_api_by_dll(self) -> None:
        report = {"missing_apis": ["oleaut32.dll!OleLoadPicture", "winmm.dll!mixerOpen", "msvcrt.dll!_iob"]}
        text = triage_report.triage(report, self.root)
        self.assertEqual([l for l in text.splitlines() if l.startswith("## ")],
                         ["## msvcrt.dll (1)", "## oleaut32.dll (1)", "## winmm.dll (1)"])
        self.assertNotIn("## Next", text)

    def test_performance_issues_point_at_the_debugger(self) -> None:
        report = {"schema": "hitode.report/1", "failure": None, "perf_issues": ["stutter"]}
        text = triage_report.triage(report, self.root)
        self.assertIn("No missing APIs", text)
        self.assertIn("hitode_debug.py watch", text)


if __name__ == "__main__":
    unittest.main()
