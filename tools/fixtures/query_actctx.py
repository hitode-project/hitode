"""Build and validate authored x86 Windows default activation queries.

Usage: python tools/fixtures/query_actctx.py --output
       <target/investigations/query-actctx/run-name> [--runner <adv-run.exe>]
Inputs: adjacent C/manifest fixtures, repository patch_crt.c, LLVM, Windows SDK.
Outputs: two isolated native runs (unbound default and manifest control), scalar
summary.json, logs, and cmd.txt with commands, environment and source/PE hashes.
The control demonstrates that NULL current handles do not imply absent SxS.
An optional runner checks the same default fixture with interpreter and JIT.
The version scalars report the compatibility version, not the host kernel build.
No game input, XML binding, guest code/pointers or manifest paths are printed.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import struct

from personality_contract import digest, run

HEADER = ("protocol", "os_major", "os_minor", "os_build", "current_success",
          "current_null", "current_invalid", "current_error", "basic_size",
          "detailed_size", "runlevel_size", "query_export", "activate_success",
          "cookie_nonzero", "deactivate_success", "row_count")
FIELDS = ("id", "flags", "handle_kind", "class", "buffer_bytes", "has_buffer",
          "has_required", "subkind", "success", "error", "required", "unchanged",
          "tail_unchanged") + tuple(f"word{i}" for i in range(12))
SENTINEL = 0xcccccccc
ERROR = 0x12345678


def decode(raw: bytes) -> dict:
    """Decode the bounded scalar protocol; reject truncated/extra output."""
    if len(raw) < 64 or len(raw) % 4:
        raise ValueError("missing_results")
    words = struct.unpack(f"<{len(raw)//4}I", raw)
    header = dict(zip(HEADER, words[:16]))
    count = header["row_count"]
    if header["protocol"] != 1 or count != 118 or len(words) != 16 + count * 25:
        raise ValueError("invalid_protocol")
    rows = [dict(zip(FIELDS, words[i:i+25])) for i in range(16, len(words), 25)]
    if [r["id"] for r in rows] != list(range(count)):
        raise ValueError("invalid_row_order")
    return {"header": header, "rows": rows}


def expected_default(row: dict) -> tuple[int, int, int]:
    """Predict success/error/required bytes for the observed empty x86 scope."""
    flags, cls = row["flags"], row["class"]
    image = flags & 24
    invalid = (flags & ~0x8000001d != 0 or image == 24
               or (image != 0 and (flags & 5 != 0 or row["handle_kind"] == 0))
               or (flags & 5 != 0 and row["handle_kind"] != 0)
               or (not row["has_buffer"] and
                   (row["buffer_bytes"] != 0 or not row["has_required"]))
               or (cls == 2 and flags & 0x80000000 != 0)
               or (cls in (5, 6) and row["subkind"] != 0))
    required = 0 if row["has_required"] else SENTINEL
    if invalid:
        return 0, 87, required
    size = {1: 8, 2: 48, 5: 12, 6: 0}[cls]
    if row["buffer_bytes"] < size:
        return 0, 122, (0 if cls == 2 else size) if row["has_required"] else SENTINEL
    return 1, ERROR, size if row["has_required"] else SENTINEL


def evaluate(record: dict, manifest: bool) -> list[str]:
    """Check only scalars; manifest control is evidence, never fabricated data."""
    failures = []
    h = record["header"]
    if [h[key] for key in HEADER[4:15]] != [1, 1, 0, ERROR, 8, 48, 12, 1, 1, 1, 1]:
        failures.append("default_scope_header")
    rows = record["rows"]
    if manifest:
        # These same authored calls were observed in the discovery probe.
        # Detailed size is path-dependent, so compare only its nonempty bound.
        detail = rows[2]
        fits = detail["required"] <= detail["buffer_bytes"]
        if not (detail["class"] == 2 and detail["flags"] == 0
                and detail["required"] > 48 and detail["success"] == int(fits)
                and detail["error"] == (ERROR if fits else 122)
                and detail["unchanged"] == int(not fits)):
            failures.append("manifest_detailed_scope")
        compat = rows[6]
        if not (compat["class"] == 6 and compat["flags"] == 0
                and compat["success"] == 1 and compat["required"] == 8
                and compat["unchanged"] == 0 and compat["tail_unchanged"] == 1):
            failures.append("manifest_compatibility_scope")
        active = rows[-1]
        if active["class"] != 2 or active["required"] <= 48:
            failures.append("null_activation_keeps_manifest")
        return failures
    for row in rows:
        expected = expected_default(row)
        actual = (row["success"], row["error"], row["required"])
        bad = actual != expected or row["tail_unchanged"] != 1
        if expected[0]:
            cls = row["class"]
            size = {1: 2, 2: 12, 5: 3, 6: 0}[cls]
            payload = [0] * size
            if cls == 1:
                payload[0] = 1  # NULL handle, not a guest address.
            elif cls == 2:
                payload[9:12] = [1] * 3  # NULL path pointers.
            bad |= [row[f"word{i}"] for i in range(12)] != payload + [SENTINEL] * (12-size)
            bad |= row["unchanged"] != int(cls == 6)
        else:
            bad |= row["unchanged"] != 1 or any(row[f"word{i}"] != 1 for i in range(12))
        if bad:
            failures.append(f"default_row_{row['id']}")
    return failures


def build(output: Path) -> tuple[list[Path], list[list[str]]]:
    """Build the same authored PE with and without an embedded manifest."""
    here = Path(__file__).resolve().parent
    llvm = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "LLVM/bin"
    sdk = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10"
    version = max((sdk / "Include").glob("10.*"),
                  key=lambda p: tuple(map(int, p.name.split('.')))).name
    commands, objects = [], []
    for source in ("patch_crt.c", "query-actctx.c"):
        obj = output / (Path(source).stem + ".obj")
        includes = ([here / "patch_headers", here] if source == "patch_crt.c" else
                    [sdk / "Include" / version / p for p in ("um", "shared", "ucrt")])
        command = [str(llvm / "clang-cl.exe"), "/nologo", "/c", "/O2", "/GS-", "/Gs9999999", "/Zl",
                   "/clang:-fno-builtin", "/clang:--target=i686-pc-windows-msvc",
                   *[f"/I{p}" for p in includes], str(here / source), f"/Fo{obj}"]
        commands.append(command)
        if run(command, output, output / (obj.stem + ".log")):
            raise RuntimeError("fixture_compile")
        objects.append(str(obj))
    rc, resource = output / "manifest.rc", output / "manifest.res"
    rc.write_text(f'1 24 "{(here / "query-actctx.manifest").as_posix()}"\n', encoding="utf-8")
    command = [str(llvm / "llvm-rc.exe"), "/fo", str(resource), str(rc)]
    commands.append(command)
    if run(command, output, output / "resource.log"):
        raise RuntimeError("fixture_resource")
    fixtures = []
    for manifest in (False, True):
        fixture = output / ("manifest.exe" if manifest else "default.exe")
        command = [str(llvm / "lld-link.exe"), "/nologo", "/machine:x86", "/nodefaultlib",
                   "/dynamicbase", "/safeseh:no", "/timestamp:0", "/subsystem:console",
                   "/entry:mainCRTStartup", f"/libpath:{sdk / 'Lib' / version / 'um/x86'}",
                   f"/out:{fixture}", *objects, *([str(resource)] if manifest else []), "kernel32.lib"]
        commands.append(command)
        if run(command, output, output / (fixture.stem + "-link.log")):
            raise RuntimeError("fixture_link")
        fixtures.append(fixture)
    return fixtures, commands


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runner", type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    if "target" not in output.parts or "investigations" not in output.parts:
        parser.error("output must be under target/investigations")
    if output.exists() and any(output.iterdir()):
        parser.error("use a fresh output directory")
    output.mkdir(parents=True, exist_ok=True)
    fixtures, commands = build(output)
    environment = {key: os.environ.get(key) for key in ("TEMP", "TMP", "CARGO_TARGET_DIR", "PATH")}
    source = Path(__file__).with_name("query-actctx.c")
    runner = args.runner.resolve() if args.runner else None
    cases = [("native", False, fixtures[0]), ("native", True, fixtures[1])]
    if runner:
        cases += [(backend, False, fixtures[0]) for backend in ("interpreter", "jit")]
    records = []
    for backend, manifest, fixture in cases:
        directory = output / f"{backend}-{'manifest' if manifest else 'default'}"
        directory.mkdir()
        exe = directory / "authored.exe"
        shutil.copyfile(fixture, exe)
        command = [str(exe)]
        result = directory / "result.bin"
        if backend != "native":
            command = [str(runner), str(exe), "--headless", "--backend", backend,
                       "--seconds", "10", "--data-dir", str(directory / "data"),
                       "--report", str(directory / "report.json"), "--api-mode", "strict",
                       "--sync-compilation"]
            result = directory / "data/overlay/result.bin"
        evidence = {"command": command, "cwd": str(directory), "environment": environment,
                    "source_sha256": digest(source), "fixture_sha256": digest(exe),
                    "runner_sha256": digest(runner) if runner else None}
        (directory / "cmd.txt").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        code = run(command, directory, directory / "run.log", timeout=20)
        try:
            record = decode(result.read_bytes())
            failures = evaluate(record, manifest)
        except (OSError, ValueError) as error:
            record = {}
            failures = ["missing_results" if isinstance(error, OSError) else str(error)]
        if code:
            failures.append("process_exit")
        if backend != "native":
            report = directory / "report.json"
            if not report.exists():
                failures.append("missing_diagnostics")
            else:
                runtime = json.loads(report.read_text(encoding="utf-8"))
                if runtime["guest_exit_code"] != 0 or not runtime["api"]["acceptance_eligible"]:
                    failures.append("runtime_diagnostics")
        records.append({"backend": backend, "manifest": manifest, "exit_code": code,
                        **record, "failures": failures})
    (output / "cmd.txt").write_text(json.dumps({"build_commands": commands,
        "environment": environment, "source_sha256": digest(source),
        "fixture_sha256": [digest(p) for p in fixtures]}, indent=2), encoding="utf-8")
    (output / "summary.json").write_text(json.dumps({"records": records}, indent=2), encoding="utf-8")
    failed = sum(bool(r["failures"]) for r in records)
    print(json.dumps({"runs": len(records), "queries_per_run": 118, "failed": failed}))
    return int(failed != 0)


if __name__ == "__main__":
    raise SystemExit(main())
