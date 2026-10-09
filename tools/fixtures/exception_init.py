"""Compare authored x86 DLL initialization exception and flag contracts.

Usage: python tools/fixtures/exception_init.py --output
       <target/investigations/exception-init/run> --runner <adv-run.exe>
Inputs: exception-init.c, exception-init-dll.c, LLVM, Windows SDK, and runner.
Outputs: isolated native/interpreter/JIT cases, frozen runner, command and
SHA-256 records, and scalar summary.json. No game files or addresses are used.
--baseline records known failures without returning a failing process status.
The default nine cases recover inside DllMain. --case 5 separately probes an
unhandled initializer fault and the native loader's ERROR_DLL_INIT_FAILED path.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import struct

from personality_contract import digest, run

CASES = tuple(range(10))
RECOVERABLE_CASES = tuple(case for case in CASES if case != 5)


def expected(case: int) -> list[int]:
    """Expected observations; RF belongs to faults, DF to saved context only."""
    if case not in CASES:
        raise ValueError("unsupported fixture case")
    if case == 5:
        return [0x1005, 0x4005, 0x2000, 0, 1114, 0x3000]
    code = {4: 0x80000003, 6: 0xc0000096, 7: 0xc0000096,
            8: 0xc000001d, 9: 0xc0000094}.get(case, 0xc0000005)
    flags = 0xed5 if case == 4 else 0x10ed5
    count = 2 if case < 4 else int(case == 4)

    def observation(kind: int) -> list[int]:
        values = [kind, code, 0, 1, 1, 1, 1, 1, flags, 0, count]
        return values + ([1, 1] if case < 4 else [])

    values = [0x1000 + case, 0x4000 + case] + observation(0x101)
    handlers = 1
    if case in (1, 3, 7):
        values += observation(0x201)
        handlers += 1
    if case in (2, 3):
        values += [0x110, 0, 0x111 if case == 2 else 0x211]
        handlers += 1
    return values + [0x5000, 1, 1, 1, 0xe91, 1, handlers, 1, 1,
                     0x2000, 1, 0, 1, 0x3000]


def evaluate(case: int, values: list[int]) -> list[str]:
    """Reject missing observations as well as mismatched exception scalars."""
    wanted = expected(case)
    if len(values) != len(wanted):
        return ["missing_results"]
    return [f"scalar_{index}" for index, (a, b) in enumerate(zip(wanted, values)) if a != b]


def build(output: Path) -> Path:
    here = Path(__file__).resolve().parent
    llvm = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "LLVM/bin"
    sdk = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10"
    version = max((sdk / "Include").glob("10.*"),
                  key=lambda p: tuple(map(int, p.name.split('.')))).name
    compile_args = [str(llvm / "clang-cl.exe"), "/nologo", "/c", "/O2", "/GS-",
                    "/Gs9999999", "/Zl", "/clang:-fno-builtin",
                    "/clang:--target=i686-pc-windows-msvc",
                    *[f"/I{sdk / 'Include' / version / part}" for part in ("um", "shared", "ucrt")]]
    link_args = [str(llvm / "lld-link.exe"), "/nologo", "/machine:x86", "/nodefaultlib",
                 "/dynamicbase", "/safeseh:no", "/timestamp:0",
                 f"/libpath:{sdk / 'Lib' / version / 'um/x86'}"]
    for stem, is_dll in (("exception-init", False), ("exception-init-dll", True)):
        obj = output / (stem + ".obj")
        if run(compile_args + [str(here / (stem + ".c")), f"/Fo{obj}"],
               output, output / (stem + "-compile.log")):
            raise RuntimeError(f"fixture compile failed: {stem}")
        target = output / ("exception-init.dll" if is_dll else "case.exe")
        flags = ["/dll", "/entry:DllMain@12"] if is_dll else ["/subsystem:console", "/entry:mainCRTStartup"]
        if run(link_args + flags + [f"/out:{target}", str(obj), "kernel32.lib"],
               output, output / (stem + "-link.log")):
            raise RuntimeError(f"fixture link failed: {stem}")
    return output / "case.exe"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--case", type=int, choices=CASES, action="append")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    allowed = root / "target/investigations"
    if not output.is_relative_to(allowed) or output == allowed:
        parser.error("output must be below this workspace target/investigations")
    if output.exists() and any(output.iterdir()):
        parser.error("use a fresh output directory")
    output.mkdir(parents=True, exist_ok=True)
    fixture = build(output)
    runner = args.runner.resolve()
    frozen = output / "bin"
    frozen.mkdir()
    shutil.copy2(runner, frozen / runner.name)
    for dll in runner.parent.glob("*.dll"):
        shutil.copy2(dll, frozen / dll.name)
    hashes = {"runner_sha256": digest(frozen / runner.name), "guest_sha256": digest(fixture),
              "dll_sha256": digest(output / "exception-init.dll")}
    records = []
    for backend in ("native", "interpreter", "jit"):
        for case in (args.case or RECOVERABLE_CASES):
            directory = output / f"{backend}-{case}"
            directory.mkdir()
            executable = directory / f"case-{case}.exe"
            shutil.copy2(fixture, executable)
            shutil.copy2(output / "exception-init.dll", directory / "exception-init.dll")
            command = [str(executable)]
            result = directory / "exception-init.bin"
            if backend != "native":
                command = [str(frozen / runner.name), str(executable), "--headless", "--backend", backend,
                           "--seconds", "5", "--data-dir", str(directory / "data"),
                           "--report", str(directory / "report.json"), "--api-mode", "strict", "--sync-compilation"]
                result = directory / "data/overlay/exception-init.bin"
            (directory / "cmd.txt").write_text(json.dumps({**hashes, "command": command,
                "cwd": str(directory), "environment": {key: os.environ.get(key)
                    for key in ("TEMP", "TMP", "CARGO_TARGET_DIR")}}, indent=2), encoding="utf-8")
            code = run(command, directory, directory / "run.log", timeout=10)
            raw = result.read_bytes() if result.exists() else b""
            values = list(struct.unpack(f"<{len(raw)//4}I", raw)) if len(raw) % 4 == 0 else []
            failures = evaluate(case, values)
            if code:
                failures.append("process_exit")
            if backend != "native":
                report = directory / "report.json"
                evidence = json.loads(report.read_text(encoding="utf-8")) if report.exists() else None
                if not evidence or evidence["guest_exit_code"] != 0 or not evidence["api"]["acceptance_eligible"]:
                    failures.append("runtime_diagnostics")
            records.append({"backend": backend, "case": case, "values": values, "failures": failures})
    (output / "summary.json").write_text(json.dumps({**hashes, "records": records}, indent=2), encoding="utf-8")
    failed = sum(bool(r["failures"]) for r in records)
    print(json.dumps({"contracts": len(records), "passed": len(records)-failed, "failed": failed}))
    return int(failed != 0 and not args.baseline)


if __name__ == "__main__":
    raise SystemExit(main())
