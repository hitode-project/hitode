#!/usr/bin/env python3
"""Generate x86 MinHook wrappers from signatures.tsv; no sample bytes are read.

Usage: python tools/compat/generate_hooks.py --output <build>/hooks.inc
Input is the repository ABI. Output is build-only C plus coverage.json containing
signature counts and exclusions. Raw DWORD parameters preserve the x86 stack ABI;
aggregate returns and variadic functions are deliberately excluded.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path

MODULES = {f"{name}.dll" for name in
           ("kernel32", "user32", "gdi32", "advapi32", "winmm", "ole32", "ntdll")}
KINDS = {name: index for index, name in enumerate(
    ("void", "u32", "i32", "handle", "ptr", "str8", "str16", "u64", "i64", "f32", "aggregate"))}


def scalar_bits(type_name: str) -> int:
    tail = type_name.rsplit(".", 1)[-1]
    if tail in ("BOOLEAN", "BYTE", "UCHAR", "CHAR", "u8", "i8"):
        return 8
    if tail in ("WORD", "USHORT", "SHORT", "ATOM", "u16", "i16"):
        return 16
    return 64


def signatures(path: Path):
    for row in path.read_text(encoding="utf-8").splitlines():
        if not row or row.startswith("#"):
            continue
        fields = row.split("\t")
        if fields[0] in MODULES:
            yield fields


def generate(path: Path, output: Path) -> dict:
    lines = ["/* Generated from the repository ABI; do not commit this file. */"]
    entries, excluded, counts = [], [], Counter()
    for fields in signatures(path):
        module, name, convention, result, _, parameters = fields[:6]
        params = [p.rsplit(":", 3) for p in parameters.split("|") if p]
        if (convention not in ("stdcall", "cdecl") or result not in KINDS
                or result == "aggregate" or any(p[-2] not in KINDS for p in params)):
            excluded.append({"api": f"{module}!{name}", "reason": "unsupported ABI"})
            continue
        words = sum(int(p[-1]) for p in params)
        if words > 32 or any(int(p[-1]) < 1 for p in params):
            excluded.append({"api": f"{module}!{name}", "reason": "unsupported stack size"})
            continue
        index = len(entries)
        cc = "__stdcall" if convention == "stdcall" else "__cdecl"
        ret = "void" if result == "void" else "unsigned long long" if result in ("u64", "i64") else "DWORD"
        decl = ", ".join(f"DWORD a{i}" for i in range(words)) or "void"
        args = ", ".join(f"a{i}" for i in range(words))
        raw = ", ".join(f"a{i}" for i in range(words)) or "0"
        lines += [f"static {ret} ({cc} *real_{index})({decl});"]
        offset, specs, child_info = 0, [], -1
        for param in params:
            width = int(param[-1])
            kind = "ptr" if param[1].rsplit(".", 1)[-1] in ("LPARAM", "WPARAM", "LRESULT") else param[-2]
            specs.append(f"{{{KINDS[kind]},{offset},{width}}}")
            if param[1].rsplit(".", 1)[-1] == "PROCESS_INFORMATION*" and name.startswith("CreateProcess"):
                child_info = offset
            offset += width
        lines += [f"static const Param params_{index}[] = {{{','.join(specs) or '{0,0,0}'}}};",
                  f"static {ret} {cc} hook_{index}({decl}) {{",
                  f"    DWORD raw[] = {{{raw}}};",
                  "    Call call; DWORD before = last_error();",
                  f"    int active = begin_call(&call, {index}, raw, _ReturnAddress());",
                  "    set_error(before);"]
        invocation = f"real_{index}({args})"
        lines.append(f"    {invocation};" if result == "void" else f"    {ret} value = {invocation};")
        lines += ["    DWORD after = last_error();",
                  f"    if (active) end_call(&call, raw, {'0' if result == 'void' else 'value'});",
                  "    set_error(after);"]
        if result != "void":
            lines.append("    return value;")
        lines.append("}")
        result_kind = "ptr" if fields[4].rsplit(".", 1)[-1] in ("LPARAM", "WPARAM", "LRESULT") else result
        entries.append(f'{{"{module}","{name}",{KINDS[result_kind]},params_{index},{len(params)},'
                       f'(LPVOID)hook_{index},(LPVOID *)&real_{index},0,0,{child_info},{scalar_bits(fields[4])}}}')
        counts[module] += 1
    lines += ["static Hook hooks[] = {", ",\n".join(entries), "};"]
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    coverage = {"generated": len(entries), "modules": dict(counts), "excluded": excluded}
    output.with_name("coverage.json").write_text(json.dumps(coverage, indent=2) + "\n", encoding="utf-8")
    return coverage


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--signatures", type=Path, default=Path(__file__).resolve().parents[2] / "data/abi/signatures.tsv")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    coverage = generate(args.signatures, args.output)
    print(f"generated={coverage['generated']} excluded={len(coverage['excluded'])}")


if __name__ == "__main__":
    main()
