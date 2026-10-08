"""Build and compare authored Windows personality contracts; no game input.

Usage: python tools/fixtures/personality_contract.py --runner <adv-run.exe>
       --output <target/investigations/personality/contracts> [--baseline]
Inputs: this directory's C fixture, checked-in MinHook, Windows SDK and LLVM.
Outputs: isolated native/interpreter/JIT runs, cmd.txt with runner/fixture hashes,
and scalar summary.json. --baseline records known differences without failing;
normal mode requires every contract. Native export sizes and nested loader call
counts are observations, not portable Windows ABI invariants.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def evaluate(test: int, values: list[int]) -> list[str]:
    """Return violated contracts; never print pointers or binary contents."""
    expected_lengths = {1: 3, 2: 16, 3: 7, 4: 9, 5: 6, 6: 9, 7: 5, 8: 4, 9: 21, 10: 11, 11: 3, 12: 38, 13: 30}
    if len(values) != expected_lengths[test]:
        return ["missing_results"]
    failures = []
    def check(name: str, condition: bool) -> None:
        if not condition:
            failures.append(name)
    v = values
    if test == 1:
        check("threads_200", v[0] == 200)
        check("threads_1000", v[1] == 1000 and v[2] == 0)
    elif test == 2:
        check("file_hook_setup", v[:6] == [1, 1, 1, 1, 7, 1])
        for index, name in enumerate(("create", "crt_open", "attributes", "find", "profile")):
            check(name, v[6 + 2*index] > 0 and v[7 + 2*index] > 0)
    elif test == 3:
        check("thread_return_image", v[:3] == [1, 1, 0x1000000])
        check("thread_seh", v[3] == 1)
        check("main_return_image", v[4:6] == [1, 0x1000000])
        check("main_seh", v[6] == 1)
    elif test == 4:
        check("core_exports", v[:4] == [56, 0xffffffff, 0xffffff, 0])
        check("heap_forwarder", v[6] == 1 and v[8] == 1)
        check("critical_forwarder", v[7] == 1)
    elif test == 5:
        check("initial_context", v == [1, 0, 1, 1, 1, 0x1234])
    elif test == 6:
        check("heap_hook_setup", v[0] == 1)
        for index, name in enumerate(("heap", "local", "global", "crt")):
            check(name, v[1 + 2*index] == 1 and v[2 + 2*index] > 0)
    elif test == 7:
        check("loader_hook_setup", v[0] == 1)
        check("load_library", v[1] == 1 and v[2] > 0)
        check("load_library_ex", v[3] == 1 and v[4] > 0)
    elif test == 8:
        check("ansi_filename", v == [936, 1, 1, 1])
    elif test == 9:
        check("memory_hook_setup", v[0] == 1)
        for index, name in enumerate(("alloc", "alloc_ex", "write", "read", "protect", "protect_ex", "query", "query_ex", "free", "free_ex")):
            check(name, v[1+2*index] == 1 and v[2+2*index] > 0)
    elif test == 10:
        check("exception_setup", v[0] == 1)
        check("hardware_dispatch", v[1] > 0)
        check("raw_dispatch_frame", v[2:6] == [1, 0, 1, 0])
        check("vectored_handlers", v[6] >= 2)
        check("native_continue", v[7] >= 2)
        check("native_raise", v[8] > 0)
        check("software_dispatch", v[9] > v[1])
        check("native_raise_service", v[10] > 0)
    elif test == 11:
        check("unload_setup", v[0] == 1)
        check("native_unload", v[1] == 1 and v[2] > 0)
    elif test == 12:
        check("lookup_setup", v[0] == 1)
        check("altered_native_load", v[1] == 1 and v[2] > 0 and v[3] == 1)
        check("named_a", v[4] == 1 and v[5] > 0 and v[6] > 0)
        check("named_w", v[7] == 1 and v[8] > 0 and v[9] > 0)
        for index, name in ((10, "unchanged"), (13, "retain"), (16, "address_unchanged"),
                            (19, "address_retain"), (22, "pin")):
            check(name, v[index:index+3] == [1, 1, 1])
        check("exe_shortcut", v[25:27] == [1, 1])
        check("missing_named", v[27:29] == [1, 1])
        check("native_missing", v[29:32] == [0xc0000135, 1, 1])
        check("native_invalid_flags", v[32:34] == [0xc000000d, 1])
        check("native_altered", v[34:36] == [0, 1])
        check("inner_lookup_hook", v[36:38] == [1, 1])
    elif test == 13:
        check("mutant_recursion", v[:6] == [0, 0, 0, 0xffffffff, 0, 0])
        check("mutant_not_owned", v[6:10] == [0xc0000046, 0x12345678, 0, 288])
        check("semaphore_create", v[10] == 0)
        check("semaphore_zero_release", v[11:13] == [0xc000000d, 0x12345678])
        check("semaphore_limit", v[13:15] == [0xc0000047, 0x12345678])
        check("semaphore_previous_count", v[15:17] == [0, 1])
        check("semaphore_negative_release", v[17:20] == [0, 87, 0x12345678])
        check("semaphore_invalid_initial", v[20:22] == [0xc000000d, 0])
        check("semaphore_invalid_maximum", v[22:24] == [0xc000000d, 0])
        check("semaphore_output_fault", v[24] == 0xc0000005)
        check("semaphore_shared_count", v[25:28] == [0, 0, 258])
        check("mutant_invalid_handle", v[28:30] == [0, 6])
    return failures


def run(command: list[str], cwd: Path, log: Path, timeout: int = 60) -> int:
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with log.open("wb") as stream:
        try:
            return subprocess.run(command, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT,
                                  timeout=timeout, creationflags=flags, check=False).returncode
        except subprocess.TimeoutExpired:
            return -1


def build(output: Path) -> Path:
    here = Path(__file__).resolve().parent
    llvm = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "LLVM/bin"
    sdk = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10"
    versions = sorted((sdk / "Include").glob("10.*"), key=lambda p: tuple(map(int, p.name.split('.'))))
    version = versions[-1].name
    objects = []
    sources = ["patch_crt.c", "vendor/minhook/src/buffer.c", "vendor/minhook/src/hook.c",
               "vendor/minhook/src/trampoline.c", "vendor/minhook/src/hde/hde32.c", "personality-contract.c"]
    for source in sources:
        obj = output / (Path(source).stem + ".obj")
        command = [str(llvm / "clang-cl.exe"), "/nologo", "/c", "/O2", "/GS-", "/Gs9999999", "/Zl",
                   "/clang:-fno-builtin", "/clang:--target=i686-pc-windows-msvc"]
        includes = [here / "patch_headers", here] if source != "personality-contract.c" else [
            sdk / "Include" / version / part for part in ("um", "shared", "ucrt")]
        includes.append(here / "vendor/minhook/include")
        command += [f"/I{p}" for p in includes] + [str(here / source), f"/Fo{obj}"]
        if run(command, output, output / (obj.stem + ".log")):
            raise RuntimeError(f"fixture compile failed: {obj.stem}")
        objects.append(str(obj))
    fixture = output / "personality.exe"
    command = [str(llvm / "lld-link.exe"), "/nologo", "/machine:x86", "/nodefaultlib", "/dynamicbase",
               "/base:0x400000", "/safeseh:no", "/timestamp:0", f"/libpath:{sdk / 'Lib' / version / 'um/x86'}",
               "/subsystem:console", "/entry:mainCRTStartup", f"/out:{fixture}", *objects, "kernel32.lib"]
    if run(command, output, output / "link.log"):
        raise RuntimeError("fixture link failed")
    return fixture


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--native-only", action="store_true")
    parser.add_argument("--tests", default="1,2,3,4,5,6,7,8,9,10,11,12,13")
    args = parser.parse_args()
    output = args.output.resolve()
    if "target" not in output.parts or "investigations" not in output.parts:
        parser.error("output must be under target/investigations")
    if output.exists() and any(output.iterdir()):
        parser.error("use a fresh output directory to preserve earlier evidence")
    output.mkdir(parents=True, exist_ok=True)
    fixture = build(output)
    if not args.native_only and not args.runner:
        parser.error("--runner is required for guest comparison")
    runner = args.runner.resolve() if args.runner else None
    runner_hash = digest(runner) if runner else None
    fixture_hash = digest(fixture)
    records = []
    for test in map(int, args.tests.split(',')):
        for backend in (["native"] if args.native_only else ["native", "interpreter", "jit"]):
            directory = output / f"{backend}-{test}"
            directory.mkdir()
            label = str(test) if test < 10 else chr(ord("a") + test - 10)
            exe = directory / f"personality-{label}.exe"
            shutil.copyfile(fixture, exe)
            command = [str(exe)]
            result = directory / f"probe-{label}.bin"
            if backend != "native":
                command = [str(runner), str(exe), "--headless", "--backend", backend, "--seconds", "30",
                           "--data-dir", str(directory / 'data'), "--report", str(directory / 'report.json'),
                           "--api-mode", "strict", "--codepage", "936", "--sync-compilation"]
                result = directory / "data/overlay" / result.name
            (directory / "cmd.txt").write_text(json.dumps({"command": command, "cwd": str(directory),
                "runner_sha256": runner_hash, "fixture_sha256": fixture_hash,
                "environment": {k: os.environ.get(k) for k in ("TEMP", "TMP", "CARGO_TARGET_DIR")}},
                ensure_ascii=False, indent=2), encoding="utf-8")
            code = run(command, directory, directory / "run.log")
            raw = result.read_bytes() if result.exists() else b""
            values = list(struct.unpack(f"<{len(raw)//4}I", raw)) if len(raw) % 4 == 0 else []
            failures = evaluate(test, values)
            if code:
                failures.append("process_exit")
            report = directory / "report.json"
            eligible = None
            if report.exists():
                eligible = json.loads(report.read_text(encoding="utf-8"))["api"]["acceptance_eligible"]
                if not eligible:
                    failures.append("runtime_diagnostics")
            records.append({"test": test, "backend": backend, "values": values,
                            "failures": failures, "runtime_acceptance_eligible": eligible})
    (output / "summary.json").write_text(json.dumps({"runner_sha256": runner_hash,
        "fixture_sha256": fixture_hash, "acceptance_eligible": not any(r["failures"] for r in records),
        "records": records}, indent=2), encoding="utf-8")
    print(json.dumps({"contracts": len(records), "passed": sum(not r['failures'] for r in records),
                      "failed": sum(bool(r['failures']) for r in records)}))
    return int(not args.baseline and any(r['failures'] for r in records))


if __name__ == "__main__":
    raise SystemExit(main())
