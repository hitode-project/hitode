#!/usr/bin/env python3
"""Clippy as CI runs it, and for the mobile targets.

  python3 tools/lint.py [host|ios|android|all]...   (default: host)
  python3 tools/lint.py --dry-run all               print the commands only

host is the workspace-lint gate CI runs (.github/workflows/core-performance.yml
calls this script). ios and android lint adv-launcher and every workspace crate
it reaches, for aarch64-apple-ios and, through cargo-ndk, aarch64-linux-android.
Each target runs in the environment tools/env.py prepares; the mobile ones build
their static FFmpeg once per checkout first. Warnings fail the run.
Tests: python3 -m unittest tools/test_lint.py
"""
from __future__ import annotations

import argparse
from pathlib import Path
import shlex
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import env as toolchain  # noqa: E402

ROOT = toolchain.ROOT
# The features CI lints the workspace with.
WORKSPACE_LINT = ["--workspace", "--all-targets", "--locked",
                  "--features", "adv-shell/gpu,adv-shell/media,adv-device/slow-tests"]
TARGETS = ("host", "ios", "android")


def command(target: str) -> tuple[str, list[str]]:
    """The environment platform and the Clippy command of `target`."""
    if target == "host":
        return "mac", ["cargo", "clippy", *WORKSPACE_LINT, "--", "-D", "warnings"]
    if target == "ios":
        return "ios", ["cargo", "clippy", "--locked", "-p", "adv-launcher",
                       "--target", "aarch64-apple-ios", "--", "-D", "warnings"]
    return "android", ["cargo", "ndk", "-t", "arm64-v8a", "--platform", toolchain.android_api(),
                       "clippy", "--locked", "-p", "adv-launcher", "--", "-D", "warnings"]


def lint(target: str, dry_run: bool = False) -> bool:
    platform, args = command(target)
    if dry_run:
        print(f"{target}: {shlex.join(args)}")
        return True
    try:
        env = toolchain.cargo(platform)
    except toolchain.Missing as missing:
        print(f"FAIL  clippy {target}")
        print(missing)
        return False
    ok = subprocess.run(args, cwd=ROOT, env=env).returncode == 0
    print(f"{'ok   ' if ok else 'FAIL '} clippy {target}")
    return ok


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("targets", nargs="*", choices=[*TARGETS, "all"], default=["host"])
    parser.add_argument("--dry-run", action="store_true", help="print the commands only")
    args = parser.parse_args(argv)
    targets = TARGETS if "all" in args.targets else dict.fromkeys(args.targets)
    results = [lint(target, args.dry_run) for target in targets]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
