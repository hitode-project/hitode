#!/usr/bin/env python3
"""Check workspace crate dependencies against the layering rules.

The rules mirror the code map in ARCHITECTURE.md. A crate may depend on what
its arrows reach, directly or through lower layers; dev-dependencies are free,
except that test-only crates must never appear anywhere else. Transitional
edges are allowed until the split that removes them; an entry that no longer
matches a real edge fails too, so the list only shrinks.
"""
from __future__ import annotations

import json
import subprocess
import sys

RULES: dict[str, set[str]] = {
    "adv-sync": set(),
    "adv-pe": set(),
    # The anonymous compatibility report format; no runtime knowledge.
    "adv-report": set(),
    # Engine-neutral core; each engine builds on it; the facade picks one.
    "adv-cpu-core": {"adv-sync"},
    "adv-cpu-x64": {"adv-cpu-core"},
    "adv-cpu-a64": {"adv-cpu-core"},
    "adv-cpu": {"adv-cpu-x64", "adv-cpu-a64"},
    "adv-machine": {"adv-cpu", "adv-sync"},
    "adv-abi": {"adv-machine", "adv-abi-macros"},
    "adv-abi-macros": set(),
    "adv-os": {"adv-abi", "adv-pe"},
    "adv-device": {"adv-sync"},
    "adv-win32": {"adv-os", "adv-device"},
    "adv-dx": {"adv-os", "adv-device"},
    # Wine's server, run in process for the Wine userland stack (issue #58).
    "adv-wineserver": set(),
    # The NT platform under Wine's userland; it talks to the server through
    # its protocol, so the server crate is a test dependency only.
    "adv-nt": set(),
    "adv-runtime": {"adv-win32", "adv-dx"},
    "adv-shell": {"adv-runtime", "adv-device", "adv-sync", "adv-report"},
    "adv-run": {"adv-shell"},
    "adv-jit-helper": set(),
    # Tauri player frontend: library and in-game overlay over the native game view.
    "adv-launcher": {"adv-shell", "tauri-plugin-storage"},
    "tauri-plugin-storage": set(),
    "adv-tune": {"adv-abi", "adv-device"},
    # Test support: reachable only from dev-dependencies.
    "adv-unicorn": {"adv-cpu"},
    "adv-samples": {"adv-pe"},
}
ALIASES: dict[str, str] = {}
TEST_ONLY = {"adv-unicorn", "adv-samples"}
TRANSITIONAL: dict[tuple[str, str], str] = {}


def role(name: str) -> str:
    if name.startswith("adv-tune-"):
        return "adv-tune"
    return ALIASES.get(name, name)


def reach(start: str) -> set[str]:
    seen: set[str] = set()
    stack = list(RULES[start])
    while stack:
        layer = stack.pop()
        if layer not in seen:
            seen.add(layer)
            stack.extend(RULES.get(layer, ()))
    return seen


def violations(crates: set[str], edges: list[tuple[str, str, str]]) -> list[str]:
    """edges: (crate, dependency, kind) between workspace crates; kind is
    'normal', 'build' or 'dev'."""
    errors = [f"{crate}: no layering rule in tools/layering/check_layers.py"
              for crate in crates if role(crate) not in RULES]
    used = set()
    for crate, dep, kind in edges:
        source, target = role(crate), role(dep)
        if kind == "dev" or source not in RULES:
            continue
        if target in TEST_ONLY and source not in TEST_ONLY:
            errors.append(f"{crate} -> {dep}: test-only crate outside dev-dependencies")
        elif (crate, dep) in TRANSITIONAL:
            used.add((crate, dep))
        elif target not in reach(source):
            errors.append(f"{crate} -> {dep} ({kind}): not allowed by the layering rules")
    for (crate, dep), reason in TRANSITIONAL.items():
        if (crate, dep) not in used:
            errors.append(f"{crate} -> {dep}: transitional edge is gone ({reason}); drop it")
    return sorted(set(errors))


def workspace(extra_args: list[str]) -> tuple[set[str], list[tuple[str, str, str]]]:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", *extra_args],
        check=True, capture_output=True, text=True,
    ).stdout
    packages = json.loads(out)["packages"]
    names = {p["name"] for p in packages}
    edges = [
        (package["name"], dep["name"], dep["kind"] or "normal")
        for package in packages
        for dep in package["dependencies"]
        if dep["name"] in names
    ]
    return names, edges


def main() -> int:
    errors = violations(*workspace(sys.argv[1:]))
    for error in errors:
        print(error)
    print("layering:", "FAILED" if errors else "ok")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
