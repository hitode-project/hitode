#!/usr/bin/env python3
"""Check docs/spec against the writing rules in docs/spec/README.md.

  python3 tools/spec/check_spec.py    # prints one line per problem; exit 1 if any

Each spec file declares an ID prefix and lists rules as `### ID Title`
headings followed by fields. Rules verified by a test need a `spec: ID`
marker in a tracked source file; every mention of an ID with a known prefix
anywhere in the repository must name a live rule.
"""
from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SPEC = "docs/spec"
RULES_DOC = f"{SPEC}/README.md"
MAX_LINES = 400
MAX_WORDS = 25
MAX_SENTENCES = 3
FIELDS = ("Rule", "Applies to", "Verification", "Basis")
REQUIRED = ("Rule", "Verification")
KEYWORDS = re.compile(r"\b(MUST|SHOULD|MAY)\b")
METHODS = ("Test", "Check", "Review")
# Status, history and plans belong in git and issues; abbreviations are ambiguous.
BANNED = ("currently", "at present", "planned", "not yet", "TODO", "no longer",
          "has been removed", "have been removed", "was removed", "were removed",
          "e.g.", "i.e.", "etc.")
# RFC 8174: only the uppercase keywords carry meaning, so the lowercase forms
# are ambiguous; `shall` is not used at all.
LOWERCASE = re.compile(r"\b(must|should|shall|SHALL)\b")
# Controlled terms: what the glossary calls the thing instead.
TERMS = {"client": "guest", "emulated program": "guest"}
SCANNED = (".rs", ".py", ".md", ".ts", ".svelte", ".toml", ".ps1", ".c", ".kt")

PREFIX_LINE = re.compile(r"^ID prefix: ([A-Z][A-Z0-9]*)$")
ENTRY = re.compile(r"^### (\S+) (\S.*)$")
FIELD = re.compile(r"^\*\*(.+?)\*\*: (.*)$")
WITHDRAWN = re.compile(r"^Withdrawn: (.*)$")
MARKER = re.compile(r"\bspec:\s*((?:[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d+[,\s]*)+)")
DATE = re.compile(r"\b20\d\d-\d\d-\d\d\b")
CODE = re.compile(r"`[^`]*`")
LINK = re.compile(r"\]\([^)]*\)")
SENTENCE_END = re.compile(r"(?<=[.;!?])\s+")


@dataclass
class Rule:
    id: str
    path: str
    line: int
    fields: dict[str, str] = field(default_factory=dict)


@dataclass
class Spec:
    rules: dict[str, Rule] = field(default_factory=dict)
    withdrawn: set[str] = field(default_factory=set)
    prefixes: set[str] = field(default_factory=set)
    errors: list[str] = field(default_factory=list)


def prose(line: str) -> str:
    return LINK.sub("]", CODE.sub("CODE", line))


def sentences(text: str) -> list[str]:
    return [s.strip() for s in SENTENCE_END.split(text.strip()) if s.strip()]


def check_words(line: str, err) -> None:
    words = prose(line)
    lower = words.lower()
    for phrase in BANNED:
        if re.search(rf"(?<!\w){re.escape(phrase.lower())}(?!\w)", lower):
            err(f"banned phrase {phrase!r}")
    if m := LOWERCASE.search(words):
        err(f"{m.group(1)!r}: write MUST, SHOULD or MAY, or rephrase")
    for term, use in TERMS.items():
        if re.search(rf"\b{term}\b", lower):
            err(f"write {use!r}, not {term!r}")
    if DATE.search(words):
        err("dates belong in git history")
    if not line.startswith(("|", "#")):
        body = FIELD.sub(r"\2", words.lstrip("-> "))
        for s in sentences(body):
            if len(s.split()) > MAX_WORDS:
                err(f"sentence over {MAX_WORDS} words: {' '.join(s.split()[:5])}…")


def parse(path: str, text: str, spec: Spec) -> None:
    lines = text.splitlines()

    def err(n: int, msg: str) -> None:
        spec.errors.append(f"{path}:{n}: {msg}")

    if len(lines) > MAX_LINES:
        err(len(lines), f"{len(lines)} lines; split the file (limit {MAX_LINES})")
    prefix = next((m.group(1) for l in lines if (m := PREFIX_LINE.match(l))), None)
    if prefix is None:
        err(1, "missing the 'ID prefix:' line")
        return
    if prefix in spec.prefixes:
        err(1, f"prefix {prefix} is declared by another file")
    spec.prefixes.add(prefix)
    id_form = re.compile(rf"^{prefix}-[A-Z0-9]+-[1-9]\d*$")
    rule: Rule | None = None
    fence = False
    for n, line in enumerate(lines, 1):
        if line.startswith("```"):
            fence = not fence
            continue
        if fence:
            continue
        check_words(line, lambda msg, n=n: err(n, msg))
        if m := WITHDRAWN.match(line):
            ids = [i for i in re.split(r"[,\s]+", m.group(1)) if i and i != "none"]
            for i in ids:
                if not id_form.match(i):
                    err(n, f"withdrawn ID {i} does not match {prefix}-GROUP-N")
                spec.withdrawn.add(i)
            continue
        if m := ENTRY.match(line):
            rule = Rule(m.group(1), path, n)
            if not id_form.match(rule.id):
                err(n, f"rule ID {rule.id} does not match {prefix}-GROUP-N")
            if rule.id in spec.rules:
                other = spec.rules[rule.id]
                err(n, f"rule ID {rule.id} already used at {other.path}:{other.line}")
            spec.rules[rule.id] = rule
            continue
        if line.startswith("#"):
            rule = None
            continue
        if rule is not None and (m := FIELD.match(line)):
            name, value = m.groups()
            if name not in FIELDS:
                err(n, f"unknown field {name}")
            elif name in rule.fields:
                err(n, f"field {name} repeated")
            rule.fields[name] = value.strip()
            if list(rule.fields) != [f for f in FIELDS if f in rule.fields]:
                err(n, f"fields of {rule.id} out of order; use {', '.join(FIELDS)}")
    for r in spec.rules.values():
        if r.path != path:
            continue

        def rule_err(msg: str, r: Rule = r) -> None:
            spec.errors.append(f"{path}:{r.line}: {r.id} {msg}")

        for name in REQUIRED:
            if name not in r.fields:
                rule_err(f"lacks {name}")
        body = prose(r.fields.get("Rule", ""))
        if body and not KEYWORDS.search(body):
            rule_err("Rule has no MUST, SHOULD or MAY")
        if len(sentences(body)) > MAX_SENTENCES:
            rule_err(f"Rule has more than {MAX_SENTENCES} sentences")
        method = r.fields.get("Verification", "")
        kind = method.split(" — ", 1)[0]
        if method and kind not in METHODS:
            rule_err(f"Verification must be one of {', '.join(METHODS)}")
        if kind in ("Check", "Review") and " — " not in method:
            rule_err(f"{kind} needs ' — reason'")


def tracked_files() -> list[str]:
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-co", "--exclude-standard", "-z"],
                         check=True, capture_output=True).stdout.decode()
    return [n for n in out.split("\0") if n.endswith(SCANNED) and (ROOT / n).is_file()]


def check_references(spec: Spec, files: dict[str, str]) -> None:
    if not spec.prefixes:
        return
    mention = re.compile(rf"\b(?:{'|'.join(sorted(spec.prefixes))})-[A-Z0-9]+-\d+\b")
    tested: set[str] = set()
    for path, text in files.items():
        is_spec = path.startswith(SPEC + "/")
        for n, line in enumerate(text.splitlines(), 1):
            for m in mention.finditer(line):
                i = m.group(0)
                if i in spec.withdrawn and not is_spec:
                    spec.errors.append(f"{path}:{n}: {i} is withdrawn")
                elif i not in spec.rules and i not in spec.withdrawn:
                    spec.errors.append(f"{path}:{n}: {i} names no rule")
            if is_spec:
                continue
            for m in MARKER.finditer(line):
                for i in re.findall(r"[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d+", m.group(1)):
                    rule = spec.rules.get(i)
                    if rule is None:
                        continue
                    tested.add(i)
                    if not rule.fields.get("Verification", "").startswith("Test"):
                        spec.errors.append(f"{path}:{n}: {i} is tested here; mark it Test")
    for r in spec.rules.values():
        if r.fields.get("Verification", "").startswith("Test") and r.id not in tested:
            spec.errors.append(f"{r.path}:{r.line}: {r.id} is Test but no test has `spec: {r.id}`")


def check(files: dict[str, str]) -> list[str]:
    spec = Spec()
    for path in sorted(files):
        if path.startswith(SPEC + "/") and path.endswith(".md") and path != RULES_DOC:
            parse(path, files[path], spec)
    check_references(spec, files)
    return spec.errors


def main() -> int:
    files = {p: (ROOT / p).read_text(encoding="utf-8", errors="replace") for p in tracked_files()}
    errors = check(files)
    for e in errors:
        print(e)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
