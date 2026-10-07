#!/usr/bin/env python3
"""Static profile of a PE module: what it will ask of the CPU and graphics layers.

Usage:
  python tools/survey/module_profile.py PE [PE ...] [--json OUT.json] [--classes 30]

For each module prints:
  - machine, kind, link timestamp, imports per DLL, exports, whether it starts
    threads by itself;
  - the instruction mix of its executable sections: integer, control, string,
    x87, MMX, SSE scalar, SSE packed and SSE integer, with the share that takes
    a memory operand (lea excluded);
  - call shapes: direct, through the import table (by name), through memory
    (virtual and function-pointer calls), through a register, and jump tables;
  - known code-generation patterns that are costly to translate, e.g. an SSE
    compare whose flags go through LAHF, an x87 compare through FNSTSW, pushf
    and popf, rdtsc, cpuid;
  - precompiled Direct3D 9 shaders embedded in the file, by version (a version
    token followed by a comment token, which is how the HLSL compiler starts
    every object);
  - the count of C++ RTTI type names and, with --classes, the first N of them;
  - the count of approximate function starts (direct call targets, exports,
    entry point), the same table native_sample.py uses to name hot addresses.

Disassembly is a linear sweep by llvm-objdump (on PATH or --llvm), so data
inside code sections adds a little noise. Only statistics and API, type and
export names are printed; keep the output local like any other analysis.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess

PREFIXES = {"rep", "repe", "repne", "repz", "repnz", "lock", "data16", "addr32"}
CONTROL = re.compile(r"^(?:j[a-z]+|call|ret[nf]?|loop[a-z]*|iret[dq]?)$")
STRING = re.compile(r"^(?:movs|stos|lods|scas|cmps|ins|outs)[bwdq]?$")
MMX = re.compile(r"\bmm[0-7]\b")
TARGET = re.compile(r"^0x([0-9a-f]+)")
# The absolute operand of `call dword ptr [0x...]` or the `# 0x...` comment
# objdump adds to a RIP-relative `call qword ptr [rip + ...]`.
INDIRECT_ABSOLUTE = re.compile(r"ptr \[0x([0-9a-f]+)\]|# 0x([0-9a-f]+)")
THREAD_IMPORTS = {"CreateThread", "_beginthreadex", "_beginthread", "CreateRemoteThread"}
# (name, mnemonics of consecutive instructions, what it costs a translator)
PATTERNS = (
    ("sse compare via lahf", (("ucomiss", "comiss", "ucomisd", "comisd"), ("lahf",), ("test",)),
     "float compare whose flags are moved into AH"),
    ("x87 compare via fnstsw", (("fcom", "fcomp", "fcompp", "fucom", "fucomp", "fucompp",
                                 "ftst", "fxam"), ("fnstsw",)),
     "x87 condition codes read through the status word"),
    ("x87 fcomi", (("fcomi", "fcomip", "fucomi", "fucomip"),), "x87 compare into EFLAGS"),
    ("pushf/popf", (("pushfd", "popfd", "pushf", "popf"),), "whole-EFLAGS save or restore"),
    ("lahf/sahf", (("lahf", "sahf"),), "flags moved through AH"),
    ("rdtsc", (("rdtsc", "rdtscp"),), "time-stamp counter read"),
    ("cpuid", (("cpuid",),), "CPU feature query"),
    ("divide", (("div", "idiv"),), "integer divide"),
    ("non-temporal/prefetch", (("movntps", "movntpd", "movntq", "movntdq", "movnti",
                                "prefetchnta", "prefetcht0", "prefetcht1", "prefetcht2"),),
     "cache-control hints"),
    ("direction flag", (("cld", "std"),), "string direction changes"),
)


# ---------------------------------------------------------------- PE reading

class Module:
    """Headers, sections, imports and exports of a PE file (standard library only)."""

    def __init__(self, data: bytes):
        if data[:2] != b"MZ":
            raise ValueError("not a PE file")
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        if data[pe:pe + 4] != b"PE\0\0":
            raise ValueError("not a PE file")
        self.data = data
        (self.machine, count, self.timestamp, _, _, optional_size,
         self.characteristics) = struct.unpack_from("<HHIIIHH", data, pe + 4)
        optional = pe + 24
        magic = struct.unpack_from("<H", data, optional)[0]
        self.wide = magic == 0x20B
        self.entry = struct.unpack_from("<I", data, optional + 16)[0]
        self.image_base = (struct.unpack_from("<Q", data, optional + 24)[0] if self.wide
                           else struct.unpack_from("<I", data, optional + 28)[0])
        directories = optional + (112 if self.wide else 96)
        self.directories = [struct.unpack_from("<II", data, directories + 8 * i)
                            for i in range(16)]
        self.sections = []
        for at in range(optional + optional_size, optional + optional_size + 40 * count, 40):
            name = data[at:at + 8].rstrip(b"\0").decode(errors="replace")
            size, rva, raw_size, raw = struct.unpack_from("<IIII", data, at + 8)
            flags = struct.unpack_from("<I", data, at + 36)[0]
            self.sections.append({"name": name, "rva": rva, "size": max(size, raw_size),
                                  "raw": raw, "raw_size": raw_size, "flags": flags})

    @property
    def kind(self) -> str:
        return "DLL" if self.characteristics & 0x2000 else "EXE"

    @property
    def architecture(self) -> str:
        return {0x14C: "x86", 0x8664: "x64", 0xAA64: "arm64"}.get(self.machine, hex(self.machine))

    def offset(self, rva: int) -> int:
        for section in self.sections:
            if section["rva"] <= rva < section["rva"] + section["raw_size"]:
                return rva - section["rva"] + section["raw"]
        raise ValueError(f"RVA {rva:#x} outside the file")

    def word(self, rva: int, width: int = 4) -> int:
        return int.from_bytes(self.data[self.offset(rva):self.offset(rva) + width], "little")

    def string(self, rva: int) -> str:
        start = self.offset(rva)
        return self.data[start:self.data.index(b"\0", start)].decode(errors="replace")

    def imports(self) -> dict[str, list[tuple[int, str]]]:
        """DLL -> [(IAT slot RVA, name or #ordinal)]."""
        rva = self.directories[1][0]
        result: dict[str, list[tuple[int, str]]] = {}
        width = 8 if self.wide else 4
        ordinal_flag = 1 << (63 if self.wide else 31)
        while rva:
            lookup, _, _, name, thunks = (self.word(rva + 4 * i) for i in range(5))
            if not thunks:
                break
            entries = result.setdefault(self.string(name), [])
            table, index = lookup or thunks, 0
            while value := self.word(table + width * index, width):
                slot = thunks + width * index
                entries.append((slot, f"#{value & 0xFFFF}" if value & ordinal_flag
                                else self.string((value & 0x7FFFFFFF) + 2)))
                index += 1
            rva += 20
        return result

    def exports(self) -> list[tuple[int, str]]:
        """[(RVA, name)] of named exports, sorted by RVA."""
        rva, size = self.directories[0]
        if not rva:
            return []
        functions, names, ordinals = (self.word(rva + offset) for offset in (28, 32, 36))
        result = []
        for index in range(self.word(rva + 24)):
            target = self.word(functions + 4 * self.word(ordinals + 2 * index, 2))
            if not rva <= target < rva + size:  # forwarders point into the directory
                result.append((target, self.string(self.word(names + 4 * index))))
        return sorted(result)


# ---------------------------------------------------------------- code

def objdump(llvm: str | None) -> str:
    found = shutil.which("llvm-objdump", path=llvm) if llvm else shutil.which("llvm-objdump")
    if not found:
        raise SystemExit("llvm-objdump not found; pass --llvm DIRECTORY")
    return found


def instructions(path: Path, llvm: str | None = None) -> list[tuple[int, str, str]]:
    """[(address, mnemonic, operands)] of every executable section, prefixes kept."""
    text = subprocess.run([objdump(llvm), "-d", "--no-show-raw-insn", "-M", "intel", str(path)],
                          check=True, capture_output=True, text=True,
                          encoding="utf-8", errors="replace").stdout
    return parse(text)


def parse(text: str) -> list[tuple[int, str, str]]:
    result = []
    for line in text.splitlines():
        address, colon, rest = line.partition(":")
        address = address.strip()
        if not colon or not address or any(c not in "0123456789abcdef" for c in address):
            continue
        fields = [field for field in rest.split("\t") if field.strip()]
        if not fields:
            continue
        words = [field.strip() for field in fields]
        prefix = []
        while words and words[0] in PREFIXES:
            prefix.append(words.pop(0))
        if not words:
            continue
        mnemonic = " ".join(prefix + [words[0]])
        result.append((int(address, 16), mnemonic, " ".join(words[1:])))
    return result


def category(mnemonic: str, operands: str) -> str:
    base = mnemonic.split()[-1]
    if base == "int3":
        return "padding"
    if CONTROL.match(base):
        return "control"
    if "xmm" in operands:
        if base.endswith(("ss", "sd")) and not STRING.match(base):
            return "sse.scalar"
        if base.endswith(("ps", "pd")):
            return "sse.packed"
        return "sse.int"
    if MMX.search(operands):
        return "mmx"
    if base.startswith("f"):
        return "x87"
    if STRING.match(base):
        return "string"
    return "int"


def profile(module: Module, code: list[tuple[int, str, str]], classes: int = 0) -> dict:
    slots = {module.image_base + slot: name
             for names in module.imports().values() for slot, name in names}
    mix, memory = Counter(), Counter()
    calls, imported = Counter(), Counter()
    for _, mnemonic, operands in code:
        kind = category(mnemonic, operands)
        mix[kind] += 1
        base = mnemonic.split()[-1]
        if "[" in operands and base != "lea" and kind != "padding":
            memory[kind] += 1
        if base == "call":
            if TARGET.match(operands):
                calls["direct"] += 1
            elif (found := INDIRECT_ABSOLUTE.search(operands)) and \
                    int(found.group(1) or found.group(2), 16) in slots:
                calls["import"] += 1
                imported[slots[int(found.group(1) or found.group(2), 16)]] += 1
            elif "[" in operands:
                calls["memory"] += 1
            else:
                calls["register"] += 1
        elif base == "jmp" and "[" in operands and "*" in operands:
            calls["jump table"] += 1
    patterns = Counter()
    bases = [mnemonic.split()[-1] for _, mnemonic, _ in code]
    for name, sequence, _ in PATTERNS:
        for index in range(len(bases) - len(sequence) + 1):
            if all(bases[index + k] in choice for k, choice in enumerate(sequence)):
                patterns[name] += 1
    names = module.imports()
    imports = {dll: sorted(name for _, name in entries) for dll, entries in names.items()}
    types = rtti(module.data)
    return {
        "architecture": module.architecture,
        "kind": module.kind,
        "timestamp": module.timestamp,
        "size": len(module.data),
        "imports": imports,
        "exports": [name for _, name in module.exports()],
        "starts_threads": sorted(THREAD_IMPORTS & {n for v in imports.values() for n in v}),
        "instructions": sum(n for kind, n in mix.items() if kind != "padding"),
        "mix": dict(mix),
        "memory_operands": dict(memory),
        "calls": dict(calls),
        "imported_call_sites": dict(imported.most_common()),
        "patterns": {name: patterns[name] for name, _, _ in PATTERNS if patterns[name]},
        "shaders": shaders(module.data),
        "rtti_types": len(types),
        "rtti_names": types[:classes],
        "function_starts": len(function_starts(module, code)),
    }


def function_starts(module: Module, code: list[tuple[int, str, str]]) -> list[int]:
    """Sorted RVAs that begin a function: direct call targets, exports, entry."""
    sections = [(s["rva"], s["rva"] + s["size"]) for s in module.sections if s["flags"] & 0x20000000]
    starts = {rva for rva, _ in module.exports()}
    if module.entry:
        starts.add(module.entry)
    for _, mnemonic, operands in code:
        if mnemonic == "call" and (found := TARGET.match(operands)):
            rva = int(found.group(1), 16) - module.image_base
            if any(low <= rva < high for low, high in sections):
                starts.add(rva)
    return sorted(starts)


def shaders(data: bytes) -> dict[str, int]:
    """Embedded D3D9 shader objects by version: a version token, then a comment token."""
    found = Counter()
    for match in re.finditer(rb"[\x00-\x03][\x01-\x03](?:\xfe|\xff)\xff\xfe\xff", data):
        minor, major, kind = match.group()[0], match.group()[1], match.group()[2]
        found[f"{'ps' if kind == 0xFF else 'vs'}_{major}_{minor}"] += 1
    return dict(sorted(found.items()))


def rtti(data: bytes) -> list[str]:
    """MSVC RTTI type descriptor names (.?AV class, .?AU struct), sorted."""
    names = {m.group(1).decode(errors="replace")
             for m in re.finditer(rb"\.\?A[VU]([A-Za-z_$][\w$@?]*?)@@\0", data)}
    return sorted(names)


# ---------------------------------------------------------------- output

def share(part: int, whole: int) -> str:
    return f"{part * 100 / whole:.1f}%" if whole else "-"


def render(path: Path, figures: dict) -> None:
    total = figures["instructions"]
    print(f"== {path.name}: {figures['architecture']} {figures['kind']}, "
          f"{figures['size'] >> 10} KiB, linked {figures['timestamp']}")
    for dll, names in sorted(figures["imports"].items(), key=lambda kv: kv[0].lower()):
        print(f"   import {dll.lower():16} {len(names):4}")
    print(f"   exports {len(figures['exports'])}; threads: "
          f"{', '.join(figures['starts_threads']) or 'none of its own'}")
    print(f"   {total} instructions, {figures['function_starts']} function starts")
    memory = figures["memory_operands"]
    for kind, count in sorted(figures["mix"].items(), key=lambda kv: -kv[1]):
        if kind != "padding":
            print(f"     {kind:11} {count:8} {share(count, total):>6}   "
                  f"memory operand {share(memory.get(kind, 0), count):>6}")
    calls = figures["calls"]
    print("   calls: " + ", ".join(f"{k} {v}" for k, v in sorted(calls.items(), key=lambda kv: -kv[1])))
    for name, count in figures["patterns"].items():
        description = next(d for n, _, d in PATTERNS if n == name)
        print(f"   pattern {name:24} {count:6}   {description}")
    if figures["shaders"]:
        print("   embedded shaders: " + ", ".join(f"{k} {v}" for k, v in figures["shaders"].items()))
    print(f"   RTTI types: {figures['rtti_types']}")
    for name in figures["rtti_names"]:
        print(f"     {name}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("modules", nargs="+", type=Path)
    parser.add_argument("--json", type=Path, help="also write the figures")
    parser.add_argument("--classes", type=int, default=0, help="RTTI type names to list")
    parser.add_argument("--llvm", help="directory holding llvm-objdump")
    arguments = parser.parse_args(argv)
    result = {}
    for path in arguments.modules:
        module = Module(path.read_bytes())
        figures = profile(module, instructions(path, arguments.llvm), arguments.classes)
        result[path.name] = figures
        render(path, figures)
    if arguments.json:
        arguments.json.write_text(json.dumps(result, indent=1, ensure_ascii=False), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
