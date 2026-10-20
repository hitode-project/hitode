#!/usr/bin/env python3
"""Reject host namespace changes that bypass `adv_os::vfs::host`.

The VFS directory index is authoritative and never revalidated against the
host (WIN32-OBJECT-8), so every guest-visible create, remove or rename must
go through the wrappers that invalidate it. Host-only files outside the guest
trees (diagnostics, indexes in the data directory) carry a `// host-only:`
comment on the same or the previous line. Test modules are not checked.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOTS = [Path("crates/adv-os/src/vfs.rs"), Path("crates/adv-os/src/vfs"), Path("crates/adv-win32/src")]
WRAPPERS = Path("crates/adv-os/src/vfs/host.rs")
CHANGE = re.compile(
    r"\bfs::(create_dir|create_dir_all|remove_file|remove_dir|remove_dir_all|rename|copy|write|hard_link)\("
    r"|\bFile::create\(|\.create(_new)?\(true\)|\bsymlink\("
)
TEST_FILE = re.compile(r"(^|/)(tests?|.*_tests|test_support)\.rs$|/tests/")


def sources() -> list[Path]:
    files: list[Path] = []
    for root in ROOTS:
        files.extend([root] if root.is_file() else sorted(root.rglob("*.rs")))
    return [f for f in files if f != WRAPPERS and not TEST_FILE.search(f.as_posix())]


def production(lines: list[str]):
    """Yield (number, line), skipping inline `#[cfg(test)] mod name { ... }` bodies."""
    skip = False
    pending = False
    for number, line in enumerate(lines, 1):
        if skip:
            if line.startswith("}"):
                skip = False
            continue
        if line.startswith("#[cfg(") and "test" in line:
            pending = True
            continue
        if pending and line.startswith("#["):
            continue
        if pending and re.match(r"(pub(\([^)]*\))? )?mod \w+ \{", line):
            skip = True
            pending = False
            continue
        pending = False
        yield number, line


def main() -> int:
    bad = []
    for path in sources():
        lines = path.read_text().splitlines()
        for number, line in production(lines):
            code = line.split("//", 1)[0]
            if not CHANGE.search(code):
                continue
            previous = lines[number - 2] if number > 1 else ""
            if "// host-only:" in line or "// host-only:" in previous:
                continue
            # Options built as the argument of a wrapped open.
            if ".create" in code and any("host::open(" in l for l in lines[max(0, number - 8) : number]):
                continue
            bad.append(f"{path}:{number}: {line.strip()}")
    if bad:
        print("host namespace changes must use adv_os::vfs::host (or mark `// host-only: why`):")
        print("\n".join(f"  {entry}" for entry in bad))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
