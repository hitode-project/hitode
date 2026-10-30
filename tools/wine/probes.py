#!/usr/bin/env python3
"""Build the authored Wine test probes, and check that tests reach them only here.

  python3 tools/wine/probes.py          # build every probe; print their directory
  python3 tools/wine/probes.py --check  # exit 1 on a malformed probe or a direct read

Each `crates/*/tests/probe/*.c` names its outputs in its leading comment, one
`Build: i686-w64-mingw32-gcc ... -o NAME ...` line per output, run in the
source's directory. Outputs are cached by their command, source and compiler, so
a changed source is always rebuilt and an unchanged one never is. The printed
directory holds exactly the current outputs.

Tests take the installation and this directory from `common/wine.rs` in
`adv-nt`, the only code that reads `ADV_WINE_DIST`.
"""
from __future__ import annotations

import hashlib
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
COMPILER = "i686-w64-mingw32-gcc"
BUILD = re.compile(r"^[\s/*]*Build:\s*(.*?)\s*(?:\*/)?\s*$")
HELPER = "crates/adv-nt/tests/common/wine.rs"
READ = re.compile(r'var(?:_os)?\(\s*"ADV_WINE_(?:DIST|PROBES)"')


def shown(path: Path) -> str:
    return str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path)


def sources() -> list[Path]:
    return sorted(ROOT.glob("crates/*/tests/probe/*.c"))


def commands(source: Path) -> tuple[list[tuple[str, list[str]]], list[str]]:
    """The (output, command) pairs of `source`, and what is wrong with them."""
    text = source.read_text(errors="replace")
    end = text.find("*/")
    header = text[: end + 2] if text.startswith("/*") and end >= 0 else ""
    found, errors = [], []
    for line in header.splitlines():
        match = BUILD.match(line)
        if not match:
            continue
        words = shlex.split(match.group(1))
        name = shown(source)
        if not words or words[0] != COMPILER:
            errors.append(f"{name}: a Build line must start with {COMPILER}")
        elif "-o" not in words[:-1] or source.name not in words:
            errors.append(f"{name}: a Build line must name -o OUTPUT and {source.name}")
        elif "/" in words[words.index("-o") + 1]:
            errors.append(f"{name}: a Build output is a file name, not a path")
        else:
            found.append((words[words.index("-o") + 1], words))
    if not found and not errors:
        errors.append(f"{shown(source)}: no `Build: {COMPILER} ...` line")
    return found, errors


def probes() -> tuple[list[tuple[Path, str, list[str]]], list[str]]:
    found, errors, seen = [], [], {}
    for source in sources():
        outputs, problems = commands(source)
        errors += problems
        for output, words in outputs:
            if output in seen:
                errors.append(f"{output}: built by both {seen[output]} and {source.name}")
            seen[output] = source.name
            found.append((source, output, words))
    return found, errors


def direct_reads(root: Path = ROOT) -> list[str]:
    errors = []
    for path in sorted(root.glob("crates/**/*.rs")):
        relative = path.relative_to(root).as_posix()
        if relative == HELPER:
            continue
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            if READ.search(line):
                errors.append(f"{relative}:{number}: read the Wine installation through {HELPER}")
    return errors


def build() -> Path:
    found, errors = probes()
    if errors:
        sys.exit("\n".join(errors))
    compiler = shutil.which(COMPILER)
    if not compiler:
        sys.exit(f"{COMPILER} is not on PATH; the Wine probes need it (docs/building.md)")
    version = subprocess.run([compiler, "--version"], capture_output=True, check=True).stdout
    cache = Path(tempfile.gettempdir()) / "adv-wine-probes"

    def one(probe: tuple[Path, str, list[str]]) -> Path:
        source, output, words = probe
        key = hashlib.sha256(version + "\0".join(words).encode() + source.read_bytes())
        target = cache / "objects" / key.hexdigest()[:24] / output
        if not target.exists():
            target.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=target.parent) as scratch:
                built = Path(scratch) / output
                command = list(words)
                command[command.index("-o") + 1] = str(built)
                result = subprocess.run(command, cwd=source.parent, capture_output=True, text=True)
                if result.returncode != 0:
                    raise RuntimeError(f"{source.name} -> {output}:\n{result.stderr}")
                os.replace(built, target)
        return target

    with ThreadPoolExecutor() as pool:
        try:
            targets = list(pool.map(one, found))
        except RuntimeError as error:
            sys.exit(str(error))
    key = hashlib.sha256("\0".join(str(t) for t in targets).encode()).hexdigest()[:24]
    directory = cache / "sets" / key
    if not directory.exists():
        directory.parent.mkdir(parents=True, exist_ok=True)
        scratch = Path(tempfile.mkdtemp(dir=directory.parent))
        for target in targets:
            os.link(target, scratch / target.name)
        try:
            os.rename(scratch, directory)
        except OSError:
            shutil.rmtree(scratch)  # Another build made the same set first.
    return directory


def main(argv: list[str]) -> int:
    if argv == ["--check"]:
        errors = probes()[1] + direct_reads()
        for error in errors:
            print(error, file=sys.stderr)
        return 1 if errors else 0
    if argv:
        sys.exit(__doc__)
    print(build())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
