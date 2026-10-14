#!/usr/bin/env python3
"""Summarize host call stacks sampled by pc_sample.py --stack.

Usage: python tools/performance/stack_report.py RUN_DIRECTORY [--focus TEXT] [--top 25]

Reads pc-stacks.tsv and sample-modules.json. Runner frames are named with
llvm-symbolizer and the runner's PDB (inlined frames included, as in
attribute_pc.py); system DLL frames by their nearest export; addresses outside
every module (JIT code) as "jit/unknown", where the walk usually stops.

Prints where samples land (leaf module), the functions with the most self and
inclusive samples, and, with --focus, only the stacks through a frame whose
name contains TEXT, followed by the immediate callers of those frames.
"""

import argparse
import bisect
from collections import Counter
import json
from pathlib import Path, PureWindowsPath
import re

from attribute_pc import SYMBOLIZER, exports, nearest_export, symbolize

UNKNOWN = "jit/unknown"
GENERIC = re.compile(r"<[^<>]*>")


def short(name, width=110):
    """Function name without generic arguments, which make Rust names unreadable."""
    previous = None
    while previous != name:
        previous, name = name, GENERIC.sub("", name)
    return name[:width]


class Namer:
    """Frame address -> names, innermost first (one frame may inline several)."""

    def __init__(self, modules, runner_symbols, export_tables):
        self.modules = sorted(modules, key=lambda m: m["base"])
        self.bases = [m["base"] for m in self.modules]
        self.runner = runner(modules)
        self.symbols = runner_symbols
        self.exports = export_tables

    def module(self, address):
        index = bisect.bisect_right(self.bases, address) - 1
        if index >= 0 and address < self.modules[index]["base"] + self.modules[index]["size"]:
            return self.modules[index]
        return None

    def __call__(self, address):
        module = self.module(address)
        if module is None:
            return [UNKNOWN]
        rva = address - module["base"]
        if module is self.runner:
            return [short(name) for name in self.symbols.get(rva, [])] or [f"adv-run+{rva:#x}"]
        # Split on either separator: module paths may come from a Windows host.
        name = PureWindowsPath(module["path"]).name
        export = nearest_export(self.exports.get(module["path"], []), rva).split("+")[0]
        return [f"{name}!{export}"]


def runner(modules):
    for module in modules:
        if module["path"].lower().endswith("adv-run.exe"):
            return module
    return modules[0] if modules else None


def aggregate(stacks, focus=None):
    """Self, inclusive and caller counts over named stacks (innermost first)."""
    leaf, own, inclusive, callers = Counter(), Counter(), Counter(), Counter()
    kept = 0
    for names in stacks:
        if not names:
            continue
        if focus and not any(focus in name for name in names):
            continue
        kept += 1
        first = names[0]
        leaf[first.split("!")[0] if "!" in first else first if first == UNKNOWN else "adv-run"] += 1
        own[names[0]] += 1
        for name in set(names):
            inclusive[name] += 1
        if focus:
            for index, name in enumerate(names[:-1]):
                if focus in name and focus not in names[index + 1]:
                    callers[names[index + 1]] += 1
    return {"stacks": kept, "leaf": leaf, "self": own, "inclusive": inclusive, "callers": callers}


def read_stacks(directory):
    rows = (directory / "pc-stacks.tsv").read_text(encoding="utf-8").splitlines()[1:]
    return [[int(x, 16) for x in row.split("\t")[1].split(",") if x] for row in rows]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("--focus", help="keep stacks through a frame containing this text")
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument("--symbolizer", default=SYMBOLIZER)
    arguments = parser.parse_args()
    directory = arguments.run_directory
    modules = json.loads((directory / "sample-modules.json").read_text(encoding="utf-8"))["modules"]
    stacks = read_stacks(directory)
    main_module = runner(modules)
    rvas = sorted({address - main_module["base"] for stack in stacks for address in stack
                   if main_module and 0 <= address - main_module["base"] < main_module["size"]})
    symbols = symbolize(main_module["path"], rvas, arguments.symbolizer) if rvas else {}
    tables = {m["path"]: exports(m["path"]) for m in modules if m is not main_module}
    name = Namer(modules, symbols, tables)
    named = [[n for address in stack for n in name(address)] for stack in stacks]
    result = aggregate(named, arguments.focus)
    total = result["stacks"] or 1
    print(f"{result['stacks']} of {len(stacks)} stacks"
          + (f" through '{arguments.focus}'" if arguments.focus else ""))
    for title, key in (("leaf module", "leaf"), ("self", "self"), ("inclusive", "inclusive"),
                       ("callers of the focus", "callers")):
        if not result[key]:
            continue
        print(f"\n{title}:")
        for entry, count in result[key].most_common(arguments.top):
            print(f"  {count * 100 / total:5.1f}%  {entry}")


if __name__ == "__main__":
    main()
