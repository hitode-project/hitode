"""Verify authored x86 heap exception contracts on Windows and optional guests.

Usage: python tools/fixtures/heap_generate_exceptions.py --output
       <target/investigations/heap-exceptions/run> [--runner <adv-run.exe>]
Inputs: heap-generate-exceptions.c, existing exception builder, LLVM/Windows SDK.
Outputs: 20 isolated cases per backend with JSON scalars, command/environment and
source/binary/runner hashes. summary.json contains no addresses or sample data.
Without --runner only native Windows runs. --baseline preserves failed guests.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil

from personality_contract import digest, run

APIS = ["HeapAlloc", "RtlAllocateHeap", "HeapReAlloc", "RtlReAllocateHeap"]


def expected(case: int) -> dict[str, int | str]:
    """Public flags plus native continuable OOM payload and NULL continuation."""
    if not 0 <= case < 20:
        raise ValueError("case must be 0..19")
    api, mode = divmod(case, 4) if case < 16 else (case - 16, 4)
    throws = mode in (2, 3, 4)
    return {
        "api": APIS[api], "mode": mode,
        "heap_flags": 0 if mode == 1 else 4 if mode in (2, 4) else 12,
        "request": 64 if mode == 0 else 0xffffffff,
        "result": int(mode == 0),
        "exception_code": 0xc0000017 if throws else 0,
        "exception_flags": 0, "exception_parameters": int(throws),
        "argument0": 0xffffffff if throws else 0,
        "original_ok": 1, "zero_ok": 1,
    }


def evaluate(case: int, values: object) -> list[str]:
    """Reject missing/wrong scalar data; LastError remains advisory per the SDK."""
    if not isinstance(values, dict):
        return ["missing_results"]
    return [key for key, value in expected(case).items() if values.get(key) != value]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", action="store_true")
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    output = args.output.resolve()
    allowed = (here / "../../target/investigations").resolve()
    if not output.is_relative_to(allowed) or output == allowed:
        parser.error("output must be below this workspace target/investigations")
    if output.exists() and any(output.iterdir()):
        parser.error("use a fresh output directory to preserve earlier evidence")
    output.mkdir(parents=True, exist_ok=True)
    command = ["pwsh", "-NoProfile", "-File", str(here / "build-exception-dispatch.ps1"),
               "-OutputDirectory", str(output), "-Fixture", "heap-generate-exceptions"]
    if run(command, output, output / "build.log"):
        raise RuntimeError("heap exception fixture build failed")
    fixture = output / "fixture_exception_heap-generate-exceptions.exe"
    runner = args.runner.resolve() if args.runner else None
    hashes = {"source_sha256": digest(here / "heap-generate-exceptions.c"),
              "fixture_sha256": digest(fixture),
              "runner_sha256": digest(runner) if runner else None}
    records = []
    for backend in (["native"] if runner is None else ["native", "interpreter", "jit"]):
        for case in range(20):
            directory = output / f"{backend}-{case}"
            directory.mkdir()
            executable = directory / f"heap-{case}.exe"
            shutil.copyfile(fixture, executable)
            command = [str(executable)]
            result = directory / "heap-generate.json"
            if backend != "native":
                command = [str(runner), str(executable), "--headless", "--backend", backend,
                           "--seconds", "5", "--data-dir", str(directory / "data"),
                           "--report", str(directory / "report.json"), "--api-mode", "strict",
                           "--sync-compilation"]
                result = directory / "data/overlay/heap-generate.json"
            (directory / "cmd.txt").write_text(json.dumps({**hashes, "command": command,
                "cwd": str(directory), "environment": {key: os.environ.get(key)
                    for key in ("TEMP", "TMP", "CARGO_TARGET_DIR")}}, indent=2), encoding="utf-8")
            code = run(command, directory, directory / "run.log", timeout=15)
            values = json.loads(result.read_text(encoding="utf-8")) if result.exists() else None
            failures = evaluate(case, values)
            if code:
                failures.append("process_exit")
            if backend != "native":
                report = directory / "report.json"
                if not report.exists():
                    failures.append("missing_diagnostics")
                else:
                    evidence = json.loads(report.read_text(encoding="utf-8"))
                    if evidence["guest_exit_code"] != 0 or not evidence["api"]["acceptance_eligible"]:
                        failures.append("runtime_diagnostics")
            records.append({"backend": backend, "case": case, "values": values, "failures": failures})
    (output / "summary.json").write_text(json.dumps({**hashes, "records": records}, indent=2),
                                         encoding="utf-8")
    failed = sum(bool(record["failures"]) for record in records)
    print(json.dumps({"contracts": len(records), "passed": len(records) - failed, "failed": failed}))
    return int(failed != 0 and not args.baseline)


if __name__ == "__main__":
    raise SystemExit(main())
