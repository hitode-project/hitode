#!/usr/bin/env python3
"""Disassemble a dumped native JIT region with its sampled hot instructions.

Usage:
  python tools/performance/native_hotspots.py RUN_DIRECTORY GUEST_START [--top 3]

Reads native-dumps.json and pc-samples.tsv written by pc_sample.py (with
--dump-regions GUEST_START), then prints the hottest stretches of the region's
machine code, each instruction prefixed by its sample count. Needs LLVM's
llvm-objcopy and llvm-objdump on PATH or in --llvm. Output stays local.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

LINE = re.compile(r"\s+([0-9a-f]+):\s+(.*)")
# objdump names branch targets after the temporary object; offsets suffice.
SYMBOL = re.compile(r" <[^>]*>")


def tool(llvm, name):
    found = shutil.which(name, path=llvm) if llvm else shutil.which(name)
    if not found:
        raise SystemExit(f"{name} not found; pass --llvm DIRECTORY")
    return found


def disassemble(binary, llvm, arch):
    with tempfile.TemporaryDirectory() as scratch:
        obj = Path(scratch) / "region.o"
        subprocess.run([tool(llvm, "llvm-objcopy"), "-I", "binary", "-O", arch,
                        "--rename-section=.data=.text,code", str(binary), str(obj)], check=True)
        text = subprocess.run([tool(llvm, "llvm-objdump"), "-d", "--no-show-raw-insn",
                               "-M", "intel", str(obj)],
                              check=True, capture_output=True, text=True).stdout
    return [(int(m.group(1), 16), SYMBOL.sub("", m.group(2)))
            for m in map(LINE.match, text.splitlines()) if m]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("guest_start", type=lambda text: int(text, 16))
    parser.add_argument("--top", type=int, default=3, help="hot stretches to print")
    parser.add_argument("--span", type=lambda text: int(text, 0), default=0x180,
                        help="bytes per stretch")
    parser.add_argument("--llvm", help="directory holding the LLVM tools")
    parser.add_argument("--arch", default="elf64-x86-64",
                        help="objcopy output target, e.g. elf64-littleaarch64")
    arguments = parser.parse_args()
    directory = arguments.run_directory
    dumps = json.loads((directory / "native-dumps.json").read_text())
    dump = next((d for d in dumps if d["guest"] == arguments.guest_start), None)
    if dump is None:
        raise SystemExit(f"no dump for {arguments.guest_start:#x}; sample with --dump-regions")
    rows = (directory / "pc-samples.tsv").read_text().splitlines()[1:]
    rips = [int(row.split("\t")[2]) for row in rows]
    hits = Counter(rip - dump["base"] for rip in rips
                   if dump["base"] <= rip < dump["base"] + dump["size"])
    print(f"{sum(hits.values()) * 100 / max(len(rips), 1):.1f}% of {len(rips)} samples "
          f"in {dump['mode']} region {arguments.guest_start:#x} ({dump['size']} bytes)")
    instructions = disassemble(directory / dump["file"], arguments.llvm, arguments.arch)
    spans = Counter()
    for offset, count in hits.items():
        spans[offset // arguments.span] += count
    for span, count in spans.most_common(arguments.top):
        start, end = span * arguments.span, (span + 1) * arguments.span
        print(f"\n== {start:#x}..{end:#x}: {count} samples")
        for offset, text in instructions:
            if start <= offset < end:
                print(f"{hits.get(offset, 0):5d} {offset:6x}  {text}")


if __name__ == "__main__":
    main()
