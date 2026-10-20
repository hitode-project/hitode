#!/usr/bin/env python3
"""Locate the host implementation of Windows APIs.

Usage: python tools/survey/where.py <dll!api | api> [...] [--root DIR]
Input: API names as reports print them (`winmm.dll!mixerGetLineInfoW`) or bare.
Output: for each name, whether docs/host-call-boundary.tsv registers it, the
non-test source lines that quote the name (registrations and calls), the
handler of a ("Name", argc, handler) table entry, the spec IDs cited in those
files and the test files that mention the name. Registration forms vary; a
registered name without a source line comes from a generated table or a COM
vtable.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TABLE = "docs/host-call-boundary.tsv"
SPEC = re.compile(r"// spec: ([A-Z0-9-]+)")


def registered(root: Path = ROOT) -> dict[str, set[str]]:
    """API name (case kept) to the lower-case DLLs that register it."""
    names: dict[str, set[str]] = {}
    for line in (root / TABLE).read_text(encoding="utf-8").splitlines()[1:]:
        api = line.split("\t", 1)[0]
        dll, sep, name = api.rpartition("!")
        if sep and not dll.startswith("<"):
            names.setdefault(name, set()).add(dll.lower())
    return names


def split(api: str) -> tuple[str, str]:
    dll, _, name = api.rpartition("!")
    return dll.lower(), name


@dataclass
class Place:
    name: str
    dlls: set[str] = field(default_factory=set)
    sources: list[str] = field(default_factory=list)
    handlers: list[str] = field(default_factory=list)
    specs: list[str] = field(default_factory=list)
    tests: list[str] = field(default_factory=list)


def is_test(path: Path) -> bool:
    return "tests" in path.parts or path.name == "tests.rs" or path.name.endswith("_tests.rs")


def locate(names: list[str], root: Path = ROOT) -> list[Place]:
    table = registered(root)
    places = [Place(n, table.get(n, set())) for n in names]
    files = sorted((root / "crates").glob("*/**/*.rs"))
    texts = {f: f.read_text(encoding="utf-8", errors="replace") for f in files}
    for place in places:
        quoted = re.compile(r'"' + re.escape(place.name) + r'"')
        # The common table form: ("Name", argc, handler)
        table_form = re.compile(r'\("' + re.escape(place.name) + r'",\s*\d+,\s*([a-z_]\w*)')
        mention = re.compile(r"\b" + re.escape(place.name) + r"\b")
        for path, text in texts.items():
            rel = path.relative_to(root).as_posix()
            if is_test(path):
                if mention.search(text):
                    place.tests.append(rel)
                continue
            found = False
            for m in quoted.finditer(text):
                found = True
                start = text.rfind("\n", 0, m.start()) + 1
                end = text.find("\n", m.end())
                line = text.count("\n", 0, m.start()) + 1
                place.sources.append(f"{rel}:{line}  {text[start:end].strip()[:90]}")
            for m in table_form.finditer(text):
                definition = re.search(r"\bfn " + re.escape(m.group(1)) + r"\b", text)
                if definition:
                    place.handlers.append(f"{rel}:{text.count(chr(10), 0, definition.start()) + 1}  fn {m.group(1)}")
            if found:
                place.specs.extend(s for s in dict.fromkeys(SPEC.findall(text)) if s not in place.specs)
    return places


def render(place: Place, asked_dll: str) -> str:
    if place.dlls:
        status = "registered in " + ", ".join(sorted(place.dlls))
        if asked_dll and asked_dll not in place.dlls:
            status += f" (not under {asked_dll})"
    else:
        status = "not registered"
    lines = [f"{place.name}: {status}"]
    for label, values in (("source", place.sources), ("handler", place.handlers),
                          ("specs", [" ".join(place.specs)] if place.specs else []), ("test", place.tests)):
        lines.extend(f"  {label:8}{value}" for value in values)
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("apis", nargs="+")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    asked = [split(a) for a in args.apis]
    for (dll, _), place in zip(asked, locate([n for _, n in asked], args.root)):
        print(render(place, dll))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
