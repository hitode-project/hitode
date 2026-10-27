#!/usr/bin/env python3
"""The one pre-commit check: formatting, whitespace, privacy, layering, specs, docs and the launcher frontend.

  python tools/check.py                 # what is staged (the pre-commit hook runs this)
  python tools/check.py --all           # every workspace crate (cargo fmt --check)
  python tools/check.py --ci            # --all plus the Clippy gates of CI; run before pushing
  python tools/check.py --fix           # apply formatting and regenerate generated tables
  python tools/check.py --install-hook  # run the staged check before every commit

Formatting is checked on the staged content of each .rs file, not the working
copy, so partially staged files are judged by what will be committed. Generated
and third-party sources keep their own layout. The hook is shared by every
worktree of this repository and skips checkouts that predate this script.

When staged changes touch apps/adv-launcher (and always with --all), its
frontend is type-checked with svelte-check and built with Vite through pnpm.
These run on the working copy and need `pnpm install --frozen-lockfile` in
apps/adv-launcher once.

--ci checks that every crate builds with default features (the launcher
needs media, so it is left out) and runs the workspace-lint Clippy gate as CI
does. Cargo commands get FFMPEG_DIR and LIBCLANG_PATH from Homebrew ffmpeg@8 and the Xcode clang when they are unset.

Committing code on master prints a note: the default branch accepts code only
through pull requests, so its checks run (CONTRIBUTING.md).
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
UNFORMATTED = ("data/abi/", "vendor/", "crates/adv-cpu-core/softfloat/")
LAUNCHER = "apps/adv-launcher/"
DEFAULT_BRANCHES = ("master", "main")
# Paths whose changes CI builds or lints; anything else is documentation.
CODE = (".rs", ".toml", ".lock", ".py", ".c", ".h", ".ps1", ".sh", ".svelte", ".ts", ".js", ".json", ".yml")
# The Clippy gate of .github/workflows/core-performance.yml.
WORKSPACE_LINT = ["--workspace", "--all-targets", "--locked",
                  "--features", "adv-shell/gpu,adv-shell/media,adv-device/slow-tests"]
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


def code_paths(names: list[str]) -> list[str]:
    return [n for n in names if n.endswith(CODE) or n.startswith(".github/")]


def branch() -> str:
    """The checked-out branch, or "" when HEAD is detached."""
    result = subprocess.run(["git", "-C", str(ROOT), "symbolic-ref", "--short", "-q", "HEAD"],
                            capture_output=True, text=True)
    return result.stdout.strip()


def output(command: list[str]) -> str:
    try:
        return subprocess.run(command, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return ""


def cargo_env() -> dict[str, str]:
    """The environment for Cargo, with the media build variables filled in on a Mac."""
    env = dict(os.environ)
    if sys.platform != "darwin":
        return env
    if not env.get("FFMPEG_DIR"):
        prefix = output(["brew", "--prefix", "ffmpeg@8"]) if shutil.which("brew") else ""
        if prefix and (Path(prefix) / "include").is_dir():
            env["FFMPEG_DIR"] = prefix
    if not env.get("LIBCLANG_PATH"):
        clang = output(["xcrun", "--find", "clang"])
        if clang:
            env["LIBCLANG_PATH"] = str(Path(clang).parent.parent / "lib")
    return env


def run(name: str, command: list[str], env: dict[str, str] | None = None) -> bool:
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True,
                            encoding="utf-8", errors="replace", env=env)
    if result.returncode == 0:
        print(f"ok    {name}")
        return True
    print(f"FAIL  {name}")
    sys.stdout.write(result.stdout + result.stderr)
    return False


def branch_names() -> list[str]:
    """Files changed since this branch left origin/master, committed or not."""
    try:
        base = git("merge-base", "HEAD", "origin/master").decode().strip()
        out = git("diff", "--name-only", "-z", base).decode()
    except subprocess.CalledProcessError:
        return []
    return [n for n in out.split("\0") if n]


def launcher(everything: bool, ci: bool = False) -> bool:
    """svelte-check and the Vite build of the launcher frontend, when it changed."""
    if not everything and not touches_launcher(staged_names()):
        return True
    installed = (ROOT / LAUNCHER / "node_modules").is_dir()
    if ci and not installed and not touches_launcher(branch_names()):
        print("skip  launcher: unchanged on this branch and not installed here")
        return True
    pnpm = shutil.which("pnpm")
    if pnpm is None:
        print("FAIL  launcher: pnpm not found; install it (see apps/adv-launcher/package.json)")
        return False
    if not installed:
        print(f"FAIL  launcher: run `pnpm --dir {LAUNCHER} install --frozen-lockfile` first")
        return False
    ok = run("svelte-check (adv-launcher)", [pnpm, "--dir", LAUNCHER, "run", "check"])
    return run("vite build (adv-launcher)", [pnpm, "--dir", LAUNCHER, "run", "build"]) and ok


def master_note() -> None:
    """Point at a pull request when code is committed straight to the default branch."""
    if branch() in DEFAULT_BRANCHES and code_paths(staged_names()):
        print(f"note  committing code on {branch()}: CI runs only on pull requests; "
              "consider `git switch -c <type>/<topic>`")


def clippy() -> bool:
    """The workspace-lint gate of .github/workflows/core-performance.yml."""
    return run("clippy workspace-lint",
               ["cargo", "clippy", *WORKSPACE_LINT, "--", "-D", "warnings"], cargo_env())


def default_features() -> bool:
    """Every crate builds without the optional features CI's lint gates turn on."""
    return run("cargo check (default features)",
               ["cargo", "check", "--workspace", "--exclude", "adv-launcher", "--all-targets", "--locked"],
               cargo_env())


def fix() -> bool:
    """Apply rustfmt."""
    ok = run("cargo fmt", ["cargo", "fmt", "--all"])
    changed = git("status", "--short").decode().rstrip()
    if changed:
        print("changed:\n" + changed)
    return ok


def check(everything: bool, ci: bool = False) -> bool:
    ok = True
    if not everything:
        master_note()
    if everything:
        ok &= run("rustfmt (workspace)", ["cargo", "fmt", "--all", "--check"])
    else:
        bad = [path for path in staged_rust() if not formatted(path)]
        if bad:
            print("FAIL  rustfmt: run `python3 tools/check.py --fix` (or `rustfmt --edition 2024 <file>`) and restage")
            for path in bad:
                print(f"        {path}")
            ok = False
        else:
            print("ok    rustfmt (staged)")
        ok &= run("whitespace and conflict markers", ["git", "diff", "--cached", "--check"])
    ok &= run("privacy", [sys.executable, "tools/privacy/scan.py"])
    ok &= run("layering", [sys.executable, "tools/layering/check_layers.py"])
    ok &= run("vfs namespace", [sys.executable, "tools/vfs/check_namespace.py"])
    ok &= run("specs", [sys.executable, "tools/spec/check_spec.py"])
    ok &= run("wine protocol", [sys.executable, "tools/wine/protocol.py", "--check"])
    ok &= run("wine syscalls", [sys.executable, "tools/wine/syscalls.py", "--check"])
    ok &= run("docs", [sys.executable, "tools/docs/check_docs.py"])
    ok &= launcher(everything, ci)
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
    group.add_argument("--ci", action="store_true", help="--all plus the Clippy gates of CI")
    group.add_argument("--fix", action="store_true", help="apply formatting and regenerate tables")
    group.add_argument("--install-hook", action="store_true", help="install the pre-commit hook")
    args = parser.parse_args(argv)
    if args.install_hook:
        install_hook()
        return 0
    if args.fix:
        return 0 if fix() else 1
    ok = check(args.all or args.ci, args.ci)
    if args.ci:
        ok &= default_features()
        ok &= clippy()
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
