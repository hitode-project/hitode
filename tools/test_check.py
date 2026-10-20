"""Tests for tools/check.py against a throwaway repository."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import os
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

    def test_notes_code_committed_on_the_default_branch_only(self) -> None:
        subprocess.run(["git", "-C", str(self.root), "checkout", "-q", "-b", "master"], check=True)
        self.stage("docs/a.md", "text\n")
        self.assertEqual(self.note(), "")
        self.stage("crates/c/src/lib.rs", "pub struct C;\n")
        self.assertIn("committing code on master", self.note())
        subprocess.run(["git", "-C", str(self.root), "checkout", "-q", "-b", "fix/topic"], check=True)
        self.assertEqual(self.note(), "")

    def note(self) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            check.master_note()
        return out.getvalue()



class LauncherGate(unittest.TestCase):
    def test_runs_only_for_launcher_paths(self) -> None:
        self.assertTrue(check.touches_launcher(["crates/a/src/lib.rs", "apps/adv-launcher/src/App.svelte"]))
        self.assertTrue(check.touches_launcher(["apps/adv-launcher/src-tauri/src/lib.rs"]))
        self.assertFalse(check.touches_launcher(["apps/adv-run/src/main.rs", "apps/adv-launcher-old/x"]))
        self.assertFalse(check.touches_launcher([]))


class CodePaths(unittest.TestCase):
    def test_documentation_alone_is_not_code(self) -> None:
        names = ["docs/a.md", "README.md", "crates/a/src/lib.rs", "Cargo.lock", ".github/workflows/x.yml",
                 "tools/check.py", "docs/images/a.png"]
        self.assertEqual(check.code_paths(names),
                         ["crates/a/src/lib.rs", "Cargo.lock", ".github/workflows/x.yml", "tools/check.py"])


class CargoEnvironment(unittest.TestCase):
    def test_keeps_variables_that_are_already_set(self) -> None:
        saved = dict(os.environ)
        try:
            os.environ["FFMPEG_DIR"] = "/set/ffmpeg"
            os.environ["LIBCLANG_PATH"] = "/set/clang"
            env = check.cargo_env()
        finally:
            os.environ.clear()
            os.environ.update(saved)
        self.assertEqual((env["FFMPEG_DIR"], env["LIBCLANG_PATH"]), ("/set/ffmpeg", "/set/clang"))


if __name__ == "__main__":
    unittest.main()
