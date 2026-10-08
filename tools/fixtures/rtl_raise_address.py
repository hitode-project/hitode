"""Compare authored x86 RTL caller-address and hook-return contracts.

Usage: python tools/fixtures/rtl_raise_address.py --output
       <target/investigations/rtl-raise-address/run> [--runner <adv-run.exe>]
       [--native-only] [--baseline]
Inputs: rtl-raise-address.c, checked-in MinHook, LLVM and the Windows SDK.
Outputs: isolated native/interpreter/JIT runs with command and binary hashes,
and summary.json containing only scalar results. No game input is used.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import struct

from personality_contract import digest, run


def evaluate(values: list[int]) -> list[str]:
    """Check address equality booleans, payload preservation and continuation."""
    if len(values) != 20:
        return ["missing_results"]
    names = ["rtl_lookup", "handler_setup", "first_caller", "first_record",
             "first_payload", "first_resume", "second_caller", "second_record",
             "second_payload", "second_resume", "hook_caller", "hook_record",
             "hook_payload", "hook_resume", "hook_setup", "hook_entered",
             "hook_returned", "handler_count", "handler_remove", "hook_cleanup"]
    return [name for index, name in enumerate(names)
            if values[index] != (3 if index == 17 else 1)]


def build(output: Path) -> Path:
    here = Path(__file__).resolve().parent
    llvm = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "LLVM/bin"
    sdk = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10"
    version = max((sdk / "Include").glob("10.*"),
                  key=lambda path: tuple(map(int, path.name.split('.')))).name
    sources = ["patch_crt.c", "vendor/minhook/src/buffer.c", "vendor/minhook/src/hook.c",
               "vendor/minhook/src/trampoline.c", "vendor/minhook/src/hde/hde32.c",
               "rtl-raise-address.c"]
    objects = []
    for source in sources:
        obj = output / (Path(source).stem + ".obj")
        includes = [here / "patch_headers", here] if source != "rtl-raise-address.c" else [
            sdk / "Include" / version / part for part in ("um", "shared", "ucrt")]
        includes.append(here / "vendor/minhook/include")
        command = [str(llvm / "clang-cl.exe"), "/nologo", "/c", "/O2", "/GS-", "/Gs9999999",
                   "/Zl", "/clang:-fno-builtin", "/clang:--target=i686-pc-windows-msvc",
                   *[f"/I{path}" for path in includes], str(here / source), f"/Fo{obj}"]
        if run(command, output, output / (obj.stem + ".log")):
            raise RuntimeError(f"fixture compile failed: {obj.stem}")
        objects.append(str(obj))
    fixture = output / "rtl-raise-address.exe"
    command = [str(llvm / "lld-link.exe"), "/nologo", "/machine:x86", "/nodefaultlib",
               "/dynamicbase", "/safeseh:no", "/timestamp:0",
               f"/libpath:{sdk / 'Lib' / version / 'um/x86'}", "/subsystem:console",
               "/entry:mainCRTStartup", f"/out:{fixture}", *objects, "kernel32.lib"]
    if run(command, output, output / "link.log"):
        raise RuntimeError("fixture link failed")
    return fixture


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--native-only", action="store_true")
    parser.add_argument("--baseline", action="store_true")
    args = parser.parse_args()
    output = args.output.resolve()
    if "target" not in output.parts or "investigations" not in output.parts:
        parser.error("output must be under target/investigations")
    if output.exists() and any(output.iterdir()):
        parser.error("use a fresh output directory to preserve earlier evidence")
    if not args.native_only and not args.runner:
        parser.error("--runner is required for guest comparison")
    output.mkdir(parents=True, exist_ok=True)
    fixture = build(output)
    runner = args.runner.resolve() if args.runner else None
    hashes = {"runner_sha256": digest(runner) if runner else None,
              "fixture_sha256": digest(fixture)}
    records = []
    for backend in (["native"] if args.native_only else ["native", "interpreter", "jit"]):
        directory = output / backend
        directory.mkdir()
        executable = directory / fixture.name
        shutil.copyfile(fixture, executable)
        command = [str(executable)]
        result = directory / "rtl-raise-address.bin"
        if backend != "native":
            command = [str(runner), str(executable), "--headless", "--backend", backend,
                       "--seconds", "20", "--data-dir", str(directory / "data"),
                       "--report", str(directory / "report.json"), "--api-mode", "strict",
                       "--sync-compilation"]
            result = directory / "data/overlay/rtl-raise-address.bin"
        (directory / "cmd.txt").write_text(json.dumps({**hashes, "command": command,
            "cwd": str(directory), "environment": {key: os.environ.get(key)
                for key in ("TEMP", "TMP", "CARGO_TARGET_DIR")}}, indent=2), encoding="utf-8")
        code = run(command, directory, directory / "run.log")
        raw = result.read_bytes() if result.exists() else b""
        values = list(struct.unpack("<20I", raw)) if len(raw) == 80 else []
        failures = evaluate(values)
        if code:
            failures.append("process_exit")
        report = directory / "report.json"
        if report.exists():
            evidence = json.loads(report.read_text(encoding="utf-8"))
            if evidence["guest_exit_code"] != 0 or not evidence["api"]["acceptance_eligible"]:
                failures.append("runtime_diagnostics")
        records.append({"backend": backend, "values": values, "failures": failures})
    (output / "summary.json").write_text(json.dumps({**hashes, "records": records}, indent=2),
                                         encoding="utf-8")
    failed = sum(bool(record["failures"]) for record in records)
    print(json.dumps({"contracts": len(records), "passed": len(records) - failed,
                      "failed": failed}))
    return int(failed != 0 and not args.baseline)


if __name__ == "__main__":
    raise SystemExit(main())
