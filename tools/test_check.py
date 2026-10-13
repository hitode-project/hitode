"""Tests for tools/check.py against a throwaway repository."""
from __future__ import annotations

import importlib.util
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check", HERE / "check.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


@unittest.skipUnless(shutil.which("rustfmt") and shutil.which("git"), "needs git and rustfmt")
class StagedFormatting(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.saved = check.ROOT
        check.ROOT = self.root

    def tearDown(self) -> None:
        check.ROOT = self.saved
        self.directory.cleanup()

    def stage(self, path: str, text: str) -> None:
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(text.encode())
        subprocess.run(["git", "-C", str(self.root), "add", path], check=True)

    def test_judges_the_staged_blob_not_the_working_copy(self) -> None:
        self.stage("a.rs", "fn main() {}\n")
        (self.root / "a.rs").write_text("fn   main( ) {}\n")
        self.assertTrue(check.formatted("a.rs"))
        self.stage("a.rs", "fn   main( ) {}\n")
        (self.root / "a.rs").write_text("fn main() {}\n")
        self.assertFalse(check.formatted("a.rs"))

    def test_line_endings_alone_are_not_a_formatting_difference(self) -> None:
        subprocess.run(["git", "-C", str(self.root), "config", "core.autocrlf", "false"], check=True)
        self.stage("a.rs", "fn main() {\r\n    let x = 1;\r\n}\r\n")
        self.assertTrue(check.formatted("a.rs"))

    def test_generated_and_third_party_sources_are_skipped(self) -> None:
        self.stage("data/abi/structs.rs", "pub   struct A;\n")
        self.stage("vendor/x/src/lib.rs", "pub   struct B;\n")
        self.stage("crates/c/src/lib.rs", "pub struct C;\n")
        self.assertEqual(check.staged_rust(), ["crates/c/src/lib.rs"])



class LauncherGate(unittest.TestCase):
    def test_runs_only_for_launcher_paths(self) -> None:
        self.assertTrue(check.touches_launcher(["crates/a/src/lib.rs", "apps/adv-launcher/src/App.svelte"]))
        self.assertTrue(check.touches_launcher(["apps/adv-launcher/src-tauri/src/lib.rs"]))
        self.assertFalse(check.touches_launcher(["apps/adv-run/src/main.rs", "apps/adv-launcher-old/x"]))
        self.assertFalse(check.touches_launcher([]))


if __name__ == "__main__":
    unittest.main()
