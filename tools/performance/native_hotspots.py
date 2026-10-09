#!/usr/bin/env python3
"""Disassemble dumped native JIT regions: hot stretches, or what the code costs.

Usage:
  python tools/performance/native_hotspots.py RUN_DIRECTORY GUEST_START [--top 3]
  python tools/performance/native_hotspots.py RUN_DIRECTORY [GUEST_START] --summary
                                              [--json OUT.json]

Reads native-dumps.json and pc-samples.tsv written by pc_sample.py (with
--dump-regions GUEST_START). The default view prints the hottest stretches of
one region's machine code, each instruction prefixed by its sample count.

--summary splits every host instruction of the dumped regions (or of the one
given) by the JIT part that emitted it (check, body, pure, slow, entry, ...)
and by guest instruction class, and
counts host instructions per guest instruction. It needs the insn-*.tsv and
jit-*.tsv tables the runner writes under ADV_JIT_MAP_DIR. Each figure is given
twice: static (every instruction counts once) and sampled (weighted by
pc-samples.tsv hits). Host instructions are also sorted by kind: stack
(spill/reload through the frame), memory (other loads/stores), call and other.
--json writes the figures for comparing two builds.

Needs LLVM's llvm-objcopy and llvm-objdump on PATH or in --llvm. Output stays
local.
"""

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from attribute_pc import JitMap

LINE = re.compile(r"\s+([0-9a-f]+):\s+(.*)")
# objdump names branch targets after the temporary object; offsets suffice.
SYMBOL = re.compile(r" <[^>]*>")
# Frame-relative operands: Cranelift spill slots and the outgoing area.
STACK = {
    "elf64-x86-64": re.compile(r"\[(?:rsp|rbp)\b"),
    "elf64-littleaarch64": re.compile(r"\[(?:sp|x29|fp)\b"),
}
MEMORY = re.compile(r"\[")
CALL = re.compile(r"^(?:call|bl|blr)\b")
KINDS = ("stack", "memory", "call", "other")
# Parts that belong to one guest instruction, in display order.
GUEST_PARTS = ("check", "body", "pure", "slow")


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


def kind(text, arch):
    """stack, memory, call or other for one disassembled host instruction."""
    if CALL.match(text):
        return "call"
    if STACK.get(arch, STACK["elf64-x86-64"]).search(text):
        return "stack"
    if MEMORY.search(text):
        return "memory"
    return "other"


def compilation_time(directory, dump):
    """The jit-*.tsv timestamp of a dumped body (older dumps do not record it)."""
    if "ns" in dump:
        return dump["ns"]
    newest = None
    for path in sorted(directory.glob("jit-*.tsv")):
        for line in path.read_text(encoding="utf-8").splitlines():
            ns, base, _, guest = map(int, line.split("\t")[:4])
            if base == dump["base"] and guest == dump["guest"] and (newest is None or ns > newest):
                newest = ns
    return newest


def summarize(instructions, ends, label, hits, arch):
    """Static and sampled host-instruction counts of one region.

    instructions: [(offset, text)]; ends: offset one past the last byte;
    label(offset) -> {"part", "pc", "class"}; hits: Counter of sampled offsets.
    """
    parts = defaultdict(lambda: {"static": Counter(), "sampled": Counter()})
    classes = defaultdict(lambda: {"guest": set(), "static": Counter(), "sampled": Counter()})
    bounds = [offset for offset, _ in instructions[1:]] + [ends]
    for (offset, text), end in zip(instructions, bounds):
        found = label(offset)
        part, which = found["part"], kind(text, arch)
        weight = sum(hits.get(at, 0) for at in range(offset, end))
        for figures, key in ((parts[part], which), (parts[part], "all")):
            figures["static"][key] += 1
            figures["sampled"][key] += weight
        if part in GUEST_PARTS and found["class"] != "overhead":
            entry = classes[found["class"]]
            entry["guest"].add(found["pc"])
            entry["static"][part] += 1
            entry["sampled"][part] += weight
    return {
        "host": len(instructions),
        "samples": sum(hits.values()),
        "parts": {name: {view: dict(counts) for view, counts in figures.items()}
                  for name, figures in parts.items()},
        "classes": {name: {"guest": len(entry["guest"]), "static": dict(entry["static"]),
                           "sampled": dict(entry["sampled"])}
                    for name, entry in classes.items()},
    }


def merge(summaries):
    total = {"host": 0, "samples": 0, "parts": {}, "classes": {}}
    for summary in summaries:
        total["host"] += summary["host"]
        total["samples"] += summary["samples"]
        for name, figures in summary["parts"].items():
            into = total["parts"].setdefault(name, {"static": {}, "sampled": {}})
            for view in ("static", "sampled"):
                for key, value in figures[view].items():
                    into[view][key] = into[view].get(key, 0) + value
        for name, entry in summary["classes"].items():
            into = total["classes"].setdefault(name, {"guest": 0, "static": {}, "sampled": {}})
            into["guest"] += entry["guest"]
            for view in ("static", "sampled"):
                for key, value in entry[view].items():
                    into[view][key] = into[view].get(key, 0) + value
    return total


def share(part, whole):
    return f"{part * 100 / whole:5.1f}" if whole else "    -"


def render(title, summary):
    host, samples = summary["host"], summary["samples"]
    guest = sum(entry["guest"] for entry in summary["classes"].values())
    print(f"\n== {title}: {host} host instructions, {guest} guest instructions, "
          f"{samples} samples")
    print(f"{'part':10} {'static%':>7} {'sampled%':>8}  {'stack':>6} {'memory':>6} {'call':>5}"
          "   (host instructions of each kind)")
    parts = summary["parts"]
    for name in sorted(parts, key=lambda n: -parts[n]["static"].get("all", 0)):
        static, sampled = parts[name]["static"], parts[name]["sampled"]
        kinds = " ".join(f"{static.get(k, 0):>{w}}" for k, w in zip(KINDS[:3], (6, 6, 5)))
        print(f"{name:10} {share(static.get('all', 0), host):>7} "
              f"{share(sampled.get('all', 0), samples):>8}  {kinds}")
    stack = sum(figures["static"].get("stack", 0) for figures in parts.values())
    stack_hits = sum(figures["sampled"].get("stack", 0) for figures in parts.values())
    print(f"stack traffic: {share(stack, host).strip()}% of host instructions, "
          f"{share(stack_hits, samples).strip()}% of samples")
    header = " ".join(f"{p:>5}" for p in GUEST_PARTS)
    print(f"\n{'class':12} {'guest':>5} {'host/guest':>10}  {header}  {'samples':>7}")
    classes = summary["classes"]
    for name in sorted(classes, key=lambda n: -sum(classes[n]["sampled"].values())):
        entry = classes[name]
        count = entry["guest"] or 1
        per = " ".join(f"{entry['static'].get(p, 0) / count:5.1f}" for p in GUEST_PARTS)
        print(f"{name:12} {entry['guest']:5} {sum(entry['static'].values()) / count:10.1f}  "
              f"{per}  {sum(entry['sampled'].values()):7}")


def hot_stretches(instructions, hits, dump, total, guest_start, top, span):
    print(f"{sum(hits.values()) * 100 / max(total, 1):.1f}% of {total} samples "
          f"in {dump['mode']} region {guest_start:#x} ({dump['size']} bytes)")
    spans = Counter()
    for offset, count in hits.items():
        spans[offset // span] += count
    for index, count in spans.most_common(top):
        start, end = index * span, (index + 1) * span
        print(f"\n== {start:#x}..{end:#x}: {count} samples")
        for offset, text in instructions:
            if start <= offset < end:
                print(f"{hits.get(offset, 0):5d} {offset:6x}  {text}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("guest_start", nargs="?", type=lambda text: int(text, 16))
    parser.add_argument("--top", type=int, default=3, help="hot stretches to print")
    parser.add_argument("--span", type=lambda text: int(text, 0), default=0x180,
                        help="bytes per stretch")
    parser.add_argument("--summary", action="store_true",
                        help="cost split by JIT part and guest class instead of code")
    parser.add_argument("--json", type=Path, help="with --summary: also write the figures")
    parser.add_argument("--llvm", help="directory holding the LLVM tools")
    parser.add_argument("--arch", default="elf64-x86-64",
                        help="objcopy output target, e.g. elf64-littleaarch64")
    arguments = parser.parse_args()
    directory = arguments.run_directory
    dumps = json.loads((directory / "native-dumps.json").read_text())
    if arguments.guest_start is not None:
        dumps = [d for d in dumps if d["guest"] == arguments.guest_start]
        if not dumps:
            raise SystemExit(f"no dump for {arguments.guest_start:#x}; "
                             "sample with --dump-regions")
    elif not arguments.summary:
        raise SystemExit("give GUEST_START, or --summary for every dumped region")
    rows = (directory / "pc-samples.tsv").read_text().splitlines()[1:]
    rips = [int(row.split("\t")[2]) for row in rows]

    def sampled(dump):
        return Counter(rip - dump["base"] for rip in rips
                       if dump["base"] <= rip < dump["base"] + dump["size"])

    if not arguments.summary:
        dump = dumps[0]
        instructions = disassemble(directory / dump["file"], arguments.llvm, arguments.arch)
        hot_stretches(instructions, sampled(dump), dump, len(rips), dump["guest"],
                      arguments.top, arguments.span)
        return
    jit = JitMap(directory)
    if not jit.tagged:
        raise SystemExit("no insn-*.tsv: run with ADV_JIT_MAP_DIR set to this directory")
    regions = {}
    for dump in dumps:
        ns = compilation_time(directory, dump)
        if ns is None:
            print(f"skipped {dump['guest']:#x}: not in jit-*.tsv")
            continue
        instructions = disassemble(directory / dump["file"], arguments.llvm, arguments.arch)
        summary = summarize(instructions, dump["size"],
                            lambda offset: jit.lookup(dump["base"] + offset, ns),
                            sampled(dump), arguments.arch)
        regions[f"{dump['guest']:08x}-{dump['mode']}"] = summary
        render(f"region {dump['guest']:#x} ({dump['mode']})", summary)
    total = merge(regions.values())
    if len(regions) > 1:
        render(f"all {len(regions)} regions", total)
    if arguments.json:
        arguments.json.write_text(json.dumps({"samples": len(rips), "total": total,
                                              "regions": regions}, indent=1))


if __name__ == "__main__":
    main()
