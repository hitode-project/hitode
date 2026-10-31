#!/usr/bin/env python3
"""Keep the first-party Markdown set closed and short.

  python3 tools/docs/check_docs.py    # prints one line per problem; exit 1 if any

Every tracked .md outside third-party trees must be listed below or be a spec
under docs/spec/, and no file may exceed MAX_LINES. Reports, plans and status
belong in issues and commit messages, so a new document needs a maintainer to
add it here.

Guides, the documents in GUIDES, must stay true to the code they walk
through. Their relative links must name existing files. A link whose text is
one code symbol, such as [`Translator::body`](crates/.../ops.rs), must name
a symbol the linked file defines, segment by segment; a leading segment may be
the file's own module name. Every spec ID they cite must be a rule that
exists and is not withdrawn.
"""
from __future__ import annotations

import re
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
MAX_LINES = 1000
THIRD_PARTY = ("vendor/", "crates/adv-cpu-core/softfloat/")
SPECS = "docs/spec/"
ALLOWED = {
    "AGENTS.md",
    "ARCHITECTURE.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "README.md",
    "README.zh-CN.md",
    "crates/adv-cpu/README.md",
    "crates/adv-sync/README.md",
    "docs/building.md",
    "docs/configuration.md",
    "docs/glossary.md",
    "docs/investigation.md",
    "docs/ios.zh-CN.md",
    "docs/maintaining.md",
    "tools/abi/README.md",
    "tools/android/README.md",
    "tools/cpu/README.md",
}

GUIDES = ("ARCHITECTURE.md",)

LINK = re.compile(r"\[([^\]]+)\]\(([^)\s]+)\)")
SYMBOL = re.compile(r"`([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`")
RULE = re.compile(r"^### ([A-Z0-9]+-[A-Z0-9]+-[0-9]+) ", re.M)
WITHDRAWN = re.compile(r"^Withdrawn: (.*)$", re.M)
PREFIX = re.compile(r"^ID prefix: ([A-Z0-9]+)$", re.M)


def defines(source: str, name: str) -> bool:
    """Whether Rust `source` defines `name`: an item, macro, field, variant
    (capitalized) or the type of an `impl`."""
    n = re.escape(name)
    return any(re.search(pattern, source, re.M) for pattern in (
        rf"\b(?:fn|struct|enum|trait|type|const|static|mod|union)\s+{n}\b",
        rf"\bmacro_rules!\s*{n}\b",
        rf"^\s*(?:pub(?:\([^)]*\))?\s+)?{n}\s*:",
        rf"^\s*impl\b[^{{]*\b{n}\b",
    )) or (name[0].isupper() and re.search(rf"^\s*{n}\s*(?:[({{,]|$)", source, re.M) is not None)


def module(path: str) -> str:
    file = Path(path)
    return file.parent.name if file.stem in ("mod", "lib") else file.stem


def check_guide(name: str, text: str, read: Callable[[str], str | None],
                rules: set[str], withdrawn: set[str], prefixes: set[str]) -> list[str]:
    """Problems in guide `name`; `read` returns a repository file or None."""
    problems = []
    for label, target in LINK.findall(text):
        if re.match(r"[a-z]+:", target) or target.startswith("#"):
            continue
        path = target.split("#")[0].rstrip("/")
        source = read(path)
        if source is None:
            problems.append(f"{name}: link to {path}, which does not exist")
            continue
        symbol = SYMBOL.fullmatch(label)
        if symbol is None or not path.endswith(".rs"):
            continue
        segments = symbol.group(1).split("::")
        if len(segments) > 1 and segments[0] == module(path):
            segments = segments[1:]
        for segment in segments:
            if not defines(source, segment):
                problems.append(f"{name}: {path} does not define `{segment}` of `{label[1:-1]}`")
    if prefixes:
        cited = re.compile(rf"\b(?:{'|'.join(sorted(prefixes))})-[A-Z0-9]+-[0-9]+\b")
        for rule in sorted(set(cited.findall(text))):
            if rule in withdrawn:
                problems.append(f"{name}: cites {rule}, which is withdrawn")
            elif rule not in rules:
                problems.append(f"{name}: cites {rule}, which no spec defines")
    return problems


def read(path: str) -> str | None:
    file = ROOT / path
    if file.is_file():
        return file.read_text(encoding="utf-8")
    return "" if file.is_dir() else None


def guides() -> list[str]:
    rules: set[str] = set()
    withdrawn: set[str] = set()
    prefixes: set[str] = set()
    for spec in sorted((ROOT / SPECS).glob("*.md")):
        text = spec.read_text(encoding="utf-8")
        rules.update(RULE.findall(text))
        prefixes.update(PREFIX.findall(text))
        for line in WITHDRAWN.findall(text):
            withdrawn.update(id.strip() for id in line.split(","))
    problems = []
    for name in GUIDES:
        problems += check_guide(name, (ROOT / name).read_text(encoding="utf-8"), read,
                                rules, withdrawn, prefixes)
    return problems


def tracked() -> list[str]:
    out = subprocess.run(["git", "ls-files", "--cached", "*.md"], cwd=ROOT, check=True,
                         capture_output=True, text=True).stdout
    return [p for p in out.splitlines() if not p.startswith(THIRD_PARTY)]


def main() -> int:
    problems = []
    for path in tracked():
        file = ROOT / path
        if not file.exists():
            continue
        if path not in ALLOWED and not (path.startswith(SPECS) and path.count("/") == 2):
            problems.append(f"{path}: not an allowed document; update an existing one instead")
        lines = len(file.read_text(encoding="utf-8").splitlines())
        if lines > MAX_LINES:
            problems.append(f"{path}: {lines} lines, over {MAX_LINES}; split or cut it")
    problems += guides()
    for problem in problems:
        print(problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
