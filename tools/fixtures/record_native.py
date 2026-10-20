#!/usr/bin/env python3
"""Record authored fixtures' native Windows results for the portable reference test.

Usage (Windows, with LLVM and the Windows SDK the build scripts name):
  python tools/fixtures/record_native.py [NAME ...]          # record (all by default)
  python tools/fixtures/record_native.py --check [NAME ...]  # rebuild and compare
  python tools/fixtures/record_native.py --sources           # any host: recordings match sources
Input: the FIXTURES table below; each build script compiles one authored
fixture deterministically (/timestamp:0).
Output: tools/fixtures/native/<name>/ with the fixture executable, the scalar
output file it wrote natively, and record.json (source, executable and output
SHA-256, Windows version, capture date). crates/adv-runtime/tests/
native_reference.rs replays every recorded fixture under each CPU backend on
any host and compares the output word by word.
--check rebuilds and reruns without writing; it fails when the executable or
the native output no longer matches the recording, so a stale or
nondeterministic recording is caught on Windows. --sources only compares
each record.json with the current fixture sources; tools/check.py runs it so
a fixture edited without re-recording is caught before commit.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import platform
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "tools/fixtures/native"
# name: (build script, executable, output file, fixture sources)
FIXTURES = {
    "condition": ("build-srw.ps1", "fixture_condition.exe", "condition.bin", ["condition.c"]),
    "drawtext-lines": ("build-drawtext-lines.ps1", "fixture_drawtext_lines.exe", "drawtext-lines.bin",
                       ["drawtext-lines.c"]),
    "mouse-track": ("build-mouse-track.ps1", "fixture_mouse_track.exe", "mouse-track.bin", ["mouse-track.c"]),
    "nls-compare": ("build-nls-compare.ps1", "fixture_nls_compare.exe", "nls-compare.bin", ["nls-compare.c"]),
    "rich-edit": ("build-rich-edit.ps1", "fixture_rich_edit.exe", "rich-edit.bin", ["rich_edit.c"]),
    "shell-directories": ("build-shell-directories.ps1", "fixture_shell_directories.exe",
                          "shell-directories.bin", ["shell_directories.c"]),
    "srw": ("build-srw.ps1", "fixture_srw.exe", "srw.bin", ["srw.c"]),
    "window-property": ("build-window-property.ps1", "fixture_window_property.exe", "window-property.bin",
                        ["window_property.c"]),
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_sha256(sources: list[str]) -> str:
    digest = hashlib.sha256()
    for name in sources:
        # Line endings depend on the checkout, not on the fixture.
        digest.update((ROOT / "tools/fixtures" / name).read_bytes().replace(b"\r\n", b"\n"))
    return digest.hexdigest()


def build_and_run(name: str) -> Path:
    script, exe, output, _ = FIXTURES[name]
    work = ROOT / "target/investigations/native-record" / name
    shutil.rmtree(work, ignore_errors=True)
    subprocess.run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                    str(ROOT / "tools/fixtures" / script), "-OutputDirectory", str(work)],
                   check=True, capture_output=True)
    # CREATE_NO_WINDOW, as the differential tests ran it.
    subprocess.run([str(work / exe)], cwd=work, check=True, creationflags=0x08000000, timeout=120)
    if not (work / output).is_file():
        raise RuntimeError(f"{name}: the fixture wrote no {output}")
    return work


def record(name: str) -> None:
    script, exe, output, sources = FIXTURES[name]
    work = build_and_run(name)
    target = NATIVE / name
    shutil.rmtree(target, ignore_errors=True)
    target.mkdir(parents=True)
    shutil.copy2(work / exe, target / exe)
    shutil.copy2(work / output, target / output)
    meta = {
        "schema_version": 1,
        "build_script": script,
        "sources": sources,
        "source_sha256": source_sha256(sources),
        "executable": exe,
        "executable_sha256": sha256(target / exe),
        "output": output,
        "output_sha256": sha256(target / output),
        "windows": platform.version(),
        "captured_date": datetime.date.today().isoformat(),
    }
    (target / "record.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"recorded {name}: {meta['output_sha256'][:12]}")


def check(name: str) -> bool:
    _, exe, output, sources = FIXTURES[name]
    meta = json.loads((NATIVE / name / "record.json").read_text(encoding="utf-8"))
    work = build_and_run(name)
    problems = []
    if meta["source_sha256"] != source_sha256(sources):
        problems.append("source changed since recording")
    if meta["executable_sha256"] != sha256(work / exe):
        problems.append("executable differs from the recording")
    if meta["output_sha256"] != sha256(work / output):
        problems.append("native output differs from the recording")
    print(f"{'ok  ' if not problems else 'FAIL'}  {name}" + "".join(f"\n      {p}" for p in problems))
    return not problems


def stale() -> list[str]:
    """Recordings whose fixture sources changed since they were made."""
    names = []
    for folder in sorted(NATIVE.iterdir()) if NATIVE.is_dir() else []:
        meta = json.loads((folder / "record.json").read_text(encoding="utf-8"))
        if meta["source_sha256"] != source_sha256(meta["sources"]):
            names.append(folder.name)
    return names


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("names", nargs="*", metavar="NAME", help=", ".join(sorted(FIXTURES)))
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--sources", action="store_true")
    args = parser.parse_args(argv)
    if args.sources:
        names = stale()
        for name in names:
            print(f"{name}: fixture source changed; re-record it on Windows with record_native.py {name}")
        return 1 if names else 0
    if sys.platform != "win32":
        parser.error("recording runs fixtures natively, so it needs Windows")
    names = args.names or sorted(FIXTURES)
    unknown = sorted(set(names) - set(FIXTURES))
    if unknown:
        parser.error(f"unknown fixtures: {', '.join(unknown)}")
    if args.check:
        return 0 if all([check(name) for name in names]) else 1
    for name in names:
        record(name)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
