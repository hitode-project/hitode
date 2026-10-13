#!/usr/bin/env python3
"""Check docs/spec against the writing rules in docs/spec/README.md.

Each spec file declares an ID prefix and lists rules as `### ID title` headings
followed by fields. Rules verified by a test need a `spec: ID` marker in a
tracked source file; every mention of an ID with a known prefix anywhere in
the repository must name a live rule.
"""
from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SPEC = "docs/spec"
MAX_LINES = 400
MAX_SENTENCE = 80
FIELDS = ("规定", "适用", "验证", "依据")
REQUIRED = ("规定", "验证")
MODALS = ("必须", "不得", "应当", "可以", "只能")
METHODS = ("测试", "检查", "审查")
BANNED = ("只可以", "目前", "现状", "已删除", "尚未", "计划", "客体", "客户机", "生成代码")
SCANNED = (".rs", ".py", ".md", ".ts", ".svelte", ".toml")

PREFIX_LINE = re.compile(r"^编号前缀：([A-Z][A-Z0-9]*)$")
ENTRY = re.compile(r"^### (\S+) (\S.*)$")
FIELD = re.compile(r"^\*\*(.+?)\*\*：(.*)$")
WITHDRAWN = re.compile(r"^已撤销：(.*)$")
MARKER = re.compile(r"\bspec:\s*((?:[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d+[,\s]*)+)")
DATE = re.compile(r"\b20\d\d-\d\d-\d\d\b")
CODE = re.compile(r"`[^`]*`")
LINK = re.compile(r"\]\([^)]*\)")


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
    return LINK.sub("]", CODE.sub("", line))


def sentences(text: str) -> list[str]:
    return [s for s in re.split(r"[。；！？]", text) if s.strip()]


def parse(path: str, text: str, spec: Spec) -> None:
    lines = text.splitlines()
    err = lambda n, msg: spec.errors.append(f"{path}:{n}: {msg}")
    if len(lines) > MAX_LINES:
        err(len(lines), f"{len(lines)} lines; split the file (limit {MAX_LINES})")
    prefix = next((m.group(1) for l in lines if (m := PREFIX_LINE.match(l))), None)
    if prefix is None:
        err(1, "missing the 编号前缀 line")
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
        words = prose(line)
        for word in BANNED:
            if word in words:
                err(n, f"banned word {word!r}")
        if re.search(r"(?<!宿)主机", words):
            err(n, "write 宿主, not 主机")
        if DATE.search(words):
            err(n, "dates belong in git history")
        if not line.startswith(("|", "#")):
            for s in sentences(words):
                if len(s.strip()) > MAX_SENTENCE:
                    err(n, f"sentence over {MAX_SENTENCE} characters: {s.strip()[:20]}…")
        if m := WITHDRAWN.match(line):
            ids = [i for i in re.split(r"[、,\s]+", m.group(1)) if i and i != "无"]
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
            order = [f for f in FIELDS if f in rule.fields]
            if list(rule.fields) != order:
                err(n, f"fields of {rule.id} out of order; use {'、'.join(FIELDS)}")
    for r in spec.rules.values():
        if r.path != path:
            continue
        for name in REQUIRED:
            if name not in r.fields:
                spec.errors.append(f"{path}:{r.line}: {r.id} lacks {name}")
        body = r.fields.get("规定", "")
        if body and not any(w in prose(body) for w in MODALS):
            spec.errors.append(f"{path}:{r.line}: {r.id} 规定 has no 必须／不得／应当／可以／只能")
        if body and len(sentences(prose(body))) > 3:
            spec.errors.append(f"{path}:{r.line}: {r.id} 规定 has more than three sentences")
        method = r.fields.get("验证", "")
        kind = method.split(" — ", 1)[0]
        if method and kind not in METHODS:
            spec.errors.append(f"{path}:{r.line}: {r.id} 验证 must be one of {'、'.join(METHODS)}")
        if kind in ("检查", "审查") and " — " not in method:
            spec.errors.append(f"{path}:{r.line}: {r.id} {kind} needs ' — reason'")


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
                    if not rule.fields.get("验证", "").startswith("测试"):
                        spec.errors.append(f"{path}:{n}: {i} is tested here; mark it 测试")
    for r in spec.rules.values():
        if r.fields.get("验证", "").startswith("测试") and r.id not in tested:
            spec.errors.append(f"{r.path}:{r.line}: {r.id} is 测试 but no test has `spec: {r.id}`")


def check(files: dict[str, str]) -> list[str]:
    spec = Spec()
    for path in sorted(files):
        if path.startswith(SPEC + "/") and path.endswith(".md") and path != f"{SPEC}/README.md":
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
