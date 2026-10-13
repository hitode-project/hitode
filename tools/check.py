#!/usr/bin/env python3
"""The one pre-commit check: formatting, whitespace, privacy, layering, specs and the launcher frontend.

  python tools/check.py                 # what is staged (the pre-commit hook runs this)
  python tools/check.py --all           # every workspace crate (cargo fmt --check)
  python tools/check.py --install-hook  # run the staged check before every commit

Formatting is checked on the staged content of each .rs file, not the working
copy, so partially staged files are judged by what will be committed. Generated
and third-party sources keep their own layout. The hook is shared by every
worktree of this repository and skips checkouts that predate this script.

When staged changes touch apps/adv-launcher (and always with --all), its
frontend is type-checked with svelte-check and built with Vite through pnpm.
These run on the working copy and need `pnpm install --frozen-lockfile` in
apps/adv-launcher once.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Generated (tools/abi) or third-party sources: never reformatted.
UNFORMATTED = ("data/abi/", "vendor/", "crates/adv-cpu-core/softfloat/", "tools/fixtures/vendor/")
LAUNCHER = "apps/adv-launcher/"
HOOK = """#!/bin/sh
# Installed by tools/check.py --install-hook; shared by every worktree.
if [ -f tools/check.py ]; then
    if command -v python3 >/dev/null 2>&1 && python3 -c "" >/dev/null 2>&1; then
        exec python3 tools/check.py
    fi
    exec python tools/check.py
fi
"""


def git(*args: str, stdin: bytes | None = None) -> bytes:
    return subprocess.run(["git", "-C", str(ROOT), *args], input=stdin, check=True,
                          capture_output=True).stdout


def staged_names(*filters: str) -> list[str]:
    out = git("diff", "--cached", "--name-only", *filters, "-z").decode()
    return [n for n in out.split("\0") if n]


def staged_rust() -> list[str]:
    names = staged_names("--diff-filter=ACMR")
    return [n for n in names if n.endswith(".rs") and not n.startswith(UNFORMATTED)]


def touches_launcher(names: list[str]) -> bool:
    return any(n.startswith(LAUNCHER) for n in names)


def formatted(path: str) -> bool:
    """Whether the staged blob of `path` is rustfmt clean."""
    source = git("show", f":{path}")
    result = subprocess.run(["rustfmt", "--edition", "2024", "--emit", "stdout", "--quiet"],
                            input=source, capture_output=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr.decode(errors="replace"))
        return False
    # rustfmt keeps the input's line endings; compare in the repository's LF form.
    return result.stdout.replace(b"\r\n", b"\n") == source.replace(b"\r\n", b"\n")


def run(name: str, command: list[str]) -> bool:
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True,
                            encoding="utf-8", errors="replace")
    if result.returncode == 0:
        print(f"ok    {name}")
        return True
    print(f"FAIL  {name}")
    sys.stdout.write(result.stdout + result.stderr)
    return False


def launcher(everything: bool) -> bool:
    """svelte-check and the Vite build of the launcher frontend, when it changed."""
    if not everything and not touches_launcher(staged_names()):
        return True
    pnpm = shutil.which("pnpm")
    if pnpm is None:
        print("FAIL  launcher: pnpm not found; install it (see apps/adv-launcher/package.json)")
        return False
    if not (ROOT / LAUNCHER / "node_modules").is_dir():
        print(f"FAIL  launcher: run `pnpm --dir {LAUNCHER} install --frozen-lockfile` first")
        return False
    ok = run("svelte-check (adv-launcher)", [pnpm, "--dir", LAUNCHER, "run", "check"])
    return run("vite build (adv-launcher)", [pnpm, "--dir", LAUNCHER, "run", "build"]) and ok


def check(everything: bool) -> bool:
    ok = True
    if everything:
        ok &= run("rustfmt (workspace)", ["cargo", "fmt", "--all", "--check"])
    else:
        bad = [path for path in staged_rust() if not formatted(path)]
        if bad:
            print("FAIL  rustfmt: run `rustfmt --edition 2024 <file>` (or `cargo fmt`) and restage")
            for path in bad:
                print(f"        {path}")
            ok = False
        else:
            print("ok    rustfmt (staged)")
        ok &= run("whitespace and conflict markers", ["git", "diff", "--cached", "--check"])
    ok &= run("privacy", [sys.executable, "tools/privacy/scan.py"])
    ok &= run("layering", [sys.executable, "tools/layering/check_layers.py"])
    ok &= run("specs", [sys.executable, "tools/spec/check_spec.py"])
    ok &= launcher(everything)
    return ok


def install_hook() -> None:
    common = Path(git("rev-parse", "--git-common-dir").decode().strip())
    if not common.is_absolute():
        common = ROOT / common
    hook = common / "hooks" / "pre-commit"
    if hook.exists() and hook.read_text(encoding="utf-8", errors="replace") != HOOK:
        sys.exit(f"{hook} exists with other content; merge it by hand")
    hook.parent.mkdir(parents=True, exist_ok=True)
    hook.write_text(HOOK, encoding="utf-8", newline="\n")
    os.chmod(hook, 0o755)
    print(f"installed {hook}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--all", action="store_true", help="check every workspace crate")
    group.add_argument("--install-hook", action="store_true", help="install the pre-commit hook")
    args = parser.parse_args(argv)
    if args.install_hook:
        install_hook()
        return 0
    return 0 if check(args.all) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
