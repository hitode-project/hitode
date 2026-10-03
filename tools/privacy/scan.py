#!/usr/bin/env python3
"""Scan repository files (and optionally commits) for private data.

The repository must not carry the tester's machine: host paths, personal
email addresses, or anything listed in a local terms file. Guest paths the
emulator itself presents (C:\\Users\\Player, C:\\Program Files, ...) are fine.

  python3 tools/privacy/scan.py                  # tracked and unignored files
  python3 tools/privacy/scan.py --commits A..B   # also author/committer emails

Extra terms (game titles, user names, machine names) go one per line in a
file named by $ADV_PRIVACY_TERMS, or in `.privacy-terms` at the repository
root, which is ignored by git. They are matched case-insensitively against
file contents and paths. A line containing `privacy: allow` is skipped.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

NOREPLY = "@users.noreply.github.com"
# Third-party code keeps its authors' addresses.
EMAIL_EXEMPT = ("vendor/", "crates/adv-cpu/softfloat/", "tools/fixtures/vendor/")
EMAIL_ALLOWED_DOMAINS = {"example.com", "example.org", "example.net"}
# Guest-side profile names the emulator presents; any other C:\Users\<name> is a host path.
GUEST_USERS = {"player", "public", "default", "all users", "{}"}
SKIP = {"Cargo.lock"}
# The scanner and its tests hold synthetic examples; only local terms apply to them.
SELF = "tools/privacy/"
ALLOW_MARK = "privacy: allow"

EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,})")
# A drive-letter path, with / or (possibly escaped) \ separators.
DRIVE = re.compile(r"(?<![A-Za-z0-9])([A-Za-z]):(?:\\\\|\\|/)+([^\\/\"'`\s,;)]*)(?:(?:\\\\|\\|/)+([^\\/\"'`,;)]*))?")
UNIX_HOME = [
    re.compile(r"(?<![\w.])/[a-z]/Users/"),  # MSYS / Git Bash
    re.compile(r"(?<![\w.])/mnt/[a-z]/"),  # WSL
    re.compile(r"(?<![\w.:/])/home/(?!runner\b)[\w.-]+"),
    re.compile(r"(?<![\w.:/])/Users/(?!Shared\b)[A-Za-z][\w.-]*"),  # macOS
]


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True,
                          encoding="utf-8").stdout


def load_terms(root: Path) -> list[str]:
    path = os.environ.get("ADV_PRIVACY_TERMS")
    file = Path(path) if path else root / ".privacy-terms"
    if not file.is_file():
        return []
    terms = []
    for line in file.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            terms.append(line.casefold())
    return terms


def drive_finding(match: re.Match[str]) -> str | None:
    drive, first, second = match.group(1), match.group(2), match.group(3) or ""
    if drive.upper() != "C":
        return f"host drive path {match.group(0)!r}"
    if first.casefold() == "users" and second and second.casefold() not in GUEST_USERS:
        return f"host profile path {match.group(0)!r}"
    return None


def scan_line(path: str, line: str, terms: list[str]) -> list[str]:
    if ALLOW_MARK in line:
        return []
    found = []
    folded = line.casefold()
    found.extend(f"local term #{i + 1}" for i, term in enumerate(terms) if term in folded)
    if path.startswith(SELF):
        return found
    for match in DRIVE.finditer(line):
        if (problem := drive_finding(match)) is not None:
            found.append(problem)
    for pattern in UNIX_HOME:
        for match in pattern.finditer(line):
            found.append(f"host path {match.group(0)!r}")
    if not path.startswith(EMAIL_EXEMPT):
        for match in EMAIL.finditer(line):
            domain = match.group(1).lower()
            if match.group(0).lower().endswith(NOREPLY) or domain in EMAIL_ALLOWED_DOMAINS:
                continue
            found.append(f"email address {match.group(0)!r}")
    return found


def scan_files(root: Path, terms: list[str]) -> list[str]:
    problems = []
    for path in git("-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard").split("\0"):
        if not path or path in SKIP:
            continue
        folded = path.casefold()
        problems.extend(f"{path}: path contains local term #{i + 1}"
                        for i, term in enumerate(terms) if term in folded)
        try:
            data = (root / path).read_bytes()
        except OSError:
            continue  # deleted in the working tree
        if b"\0" in data[:8192]:
            continue  # binary
        text = data.decode("utf-8", errors="replace")
        for number, line in enumerate(text.splitlines(), 1):
            problems.extend(f"{path}:{number}: {p}" for p in scan_line(path, line, terms))
    return problems


def scan_commits(root: Path, commits: str) -> list[str]:
    problems = []
    log = git("-C", str(root), "log", "--format=%h%x00%ae%x00%ce", commits)
    for entry in log.splitlines():
        sha, author, committer = entry.split("\0")
        for role, email in (("author", author), ("committer", committer)):
            if not email.lower().endswith(NOREPLY) and email != "noreply@github.com":
                problems.append(f"commit {sha}: {role} email is not a GitHub noreply address")
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--commits", help="also check author/committer emails in this range")
    args = parser.parse_args(argv)
    root = Path(git("rev-parse", "--show-toplevel").strip())
    terms = load_terms(root)
    problems = scan_files(root, terms)
    if args.commits:
        problems += scan_commits(root, args.commits)
    for problem in problems:
        print(problem)
    if problems:
        print(f"{len(problems)} privacy finding(s); see tools/privacy/scan.py", file=sys.stderr)
        return 1
    print(f"privacy scan ok ({len(terms)} local terms)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
