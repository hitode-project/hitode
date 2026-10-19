#!/usr/bin/env python3
"""Keep the first-party Markdown set closed and short.

  python3 tools/docs/check_docs.py    # prints one line per problem; exit 1 if any

Every tracked .md outside third-party trees must be listed below or be a spec
under docs/spec/, and no file may exceed MAX_LINES. Reports, plans and status
belong in issues and commit messages, so a new document needs a maintainer to
add it here.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
MAX_LINES = 400
THIRD_PARTY = ("vendor/", "tools/fixtures/vendor/", "crates/adv-cpu-core/softfloat/")
SPECS = "docs/spec/"
ALLOWED = {
    "AGENTS.md",
    "ARCHITECTURE.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "README.md",
    "README.zh-CN.md",
    "crates/adv-cpu/README.md",
    "crates/adv-os/tests/fixtures/README.md",
    "crates/adv-sync/README.md",
    "docs/building.md",
    "docs/configuration.md",
    "docs/glossary.md",
    "docs/investigation.md",
    "docs/ios.md",
    "docs/ios.zh-CN.md",
    "docs/maintaining.md",
    "tools/abi/README.md",
    "tools/android/README.md",
    "tools/compat/README.md",
    "tools/cpu/README.md",
    "tools/fixtures/README.md",
    "tools/fixtures/dll-driver.md",
}


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
    for problem in problems:
        print(problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
