#!/usr/bin/env python3
"""Run behavior scenarios and judge a head build by its merge base.

  run.py run  --adv-run BIN --wine DIST [--tree DIR] [--out FILE] [NAME ...]
  run.py gate --base-tree DIR --base-bin BIN --head-tree DIR --head-bin BIN --wine DIST

A scenario is a directory `tests/scenarios/NAME/` holding `scenario.json` and
the guest C sources it builds (`Build:` lines, as in tools/wine/probes.py). The
guest judges its own result: exit status 0 means it holds. Files it writes and
named report counters are its observable outputs.

`gate` applies docs/spec/README.md#scenarios: the base tree's scenarios run on
both builds; they must still pass on head, and any output that differs must be
claimed by a scenario of the same spec ID that fails on base and passes on head.
Steady-state counter rates never rise. CI runs the base tree's copy of this
file, so a change cannot loosen the gate that judges it.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "wine"))
import probes  # noqa: E402

SCENARIOS = Path("tests/scenarios")
# Per-iteration growth allowed for a steady counter on head over base.
STEADY_SLACK = 0.01
COUNTER = re.compile(r"\b(\w+): (\d+)\b")


def load(tree: Path) -> dict[str, dict]:
    found = {}
    for spec in sorted((tree / SCENARIOS).glob("*/scenario.json")):
        scenario = json.loads(spec.read_text())
        scenario["dir"] = spec.parent
        scenario["digest"] = digest(spec.parent)
        found[spec.parent.name] = scenario
    return found


def digest(directory: Path) -> str:
    h = hashlib.sha256()
    for path in sorted(directory.rglob("*")):
        if path.is_file():
            h.update(path.relative_to(directory).as_posix().encode() + b"\0" + path.read_bytes())
    return h.hexdigest()


def build(scenario: dict, into: Path) -> None:
    for data in sorted(scenario["dir"].iterdir()):
        if data.is_file() and data.suffix not in (".c", ".h") and data.name != "scenario.json":
            shutil.copy(data, into / data.name)
    sources = sorted(scenario["dir"].glob("*.c"))
    for source in sources:
        outputs, errors = probes.commands(source)
        if errors:
            raise RuntimeError("\n".join(errors))
        for output, words in outputs:
            command = list(words)
            command[command.index("-o") + 1] = str(into / output)
            result = subprocess.run(command, cwd=source.parent, capture_output=True, text=True)
            if result.returncode != 0:
                raise RuntimeError(f"{source.name} -> {output}:\n{result.stderr}")


def counters(report: dict) -> dict[str, int]:
    found = {k: int(v) for k, v in COUNTER.findall(report.get("a64_diagnostics") or "")}
    for key in ("compilations", "host_calls", "interpreter_instructions"):
        if isinstance(report.get(key), int):
            found[key] = report[key]
    return found


def execute(scenario: dict, binary: Path, wine: Path, work: Path, iterations: int | None) -> dict:
    game = work / "game"
    game.mkdir(parents=True)
    build(scenario, game)
    if iterations is not None:
        (game / "scenario-input.txt").write_text(f"{iterations}\n")
    report = work / "report.json"
    command = [
        str(binary), "--headless", "--seconds", str(scenario.get("seconds", 60)),
        "--data-dir", str(work / "data"), "--wine", str(wine), "--report", str(report),
        *scenario.get("args", []), "--exe", str(game / scenario["exe"]),
    ]
    env = dict(os.environ, ADV_AUDIO_MUTE_DEVICE="1")
    log = work / "log.txt"
    with log.open("w") as sink:
        subprocess.run(command, stdout=sink, stderr=subprocess.STDOUT, env=env,
                       timeout=scenario.get("seconds", 60) + 60)
    data = json.loads(report.read_text()) if report.exists() else {}
    outputs = {}
    for name in scenario.get("outputs", []):
        path = game / name
        outputs[name] = hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None
    if scenario.get("frame"):
        outputs["frame"] = data.get("frame_fnv1a64")
    exit_code = data.get("guest_exit_code")
    return {
        "passed": data.get("stop_reason") == "guest_exit" and exit_code == 0,
        "exit": exit_code,
        "stop": data.get("stop_reason"),
        "outputs": outputs,
        "counters": counters(data),
        "log": log.read_text(errors="replace")[-2000:],
    }


def run_one(scenario: dict, binary: Path, wine: Path) -> dict:
    with tempfile.TemporaryDirectory(prefix="adv-scenario-") as scratch:
        steady = scenario.get("steady")
        try:
            if not steady:
                return execute(scenario, binary, wine, Path(scratch) / "run", None)
            low, high = steady["iterations"]
            first = execute(scenario, binary, wine, Path(scratch) / "low", low)
            result = execute(scenario, binary, wine, Path(scratch) / "high", high)
        except (RuntimeError, subprocess.TimeoutExpired) as error:
            return {"passed": False, "exit": None, "stop": "error", "outputs": {},
                    "counters": {}, "log": str(error)}
        result["passed"] = result["passed"] and first["passed"]
        result["steady"] = {
            name: (result["counters"].get(name, 0) - first["counters"].get(name, 0)) / (high - low)
            for name in steady["counters"]
        }
        return result


def run_all(scenarios: dict[str, dict], binary: Path, wine: Path) -> dict[str, dict]:
    return {name: run_one(scenario, binary, wine) for name, scenario in scenarios.items()}


def judge(base: dict[str, dict], head: dict[str, dict], results: dict) -> list[str]:
    """Return the gate's failures. `results` holds four runs keyed
    base_on_base, base_on_head, changed_on_base, changed_on_head, changed_again."""
    failures = []
    changed = {n: s for n, s in head.items() if n not in base or base[n]["digest"] != s["digest"]}
    claimed: set[str] = set()
    for name, scenario in changed.items():
        now, again, before = (results[k][name] for k in ("changed_on_head", "changed_again", "changed_on_base"))
        if not now["passed"]:
            failures.append(f"{name}: fails on head (exit {now['exit']}, {now['stop']})")
        elif now["outputs"] != again["outputs"]:
            failures.append(f"{name}: outputs differ between two head runs")
        elif not before["passed"]:
            claimed.update(base.get(name, scenario)["spec"])
    for name, scenario in base.items():
        old, new = results["base_on_base"][name], results["base_on_head"][name]
        mine = set(scenario["spec"]) & claimed
        if not old["passed"]:
            failures.append(f"{name}: fails on base; fix the base before judging changes")
            continue
        if not new["passed"] and not mine:
            failures.append(f"{name}: passed on base, fails on head (exit {new['exit']}, {new['stop']})")
        elif new["outputs"] != old["outputs"] and not mine:
            keys = sorted(k for k in old["outputs"] if old["outputs"][k] != new["outputs"].get(k))
            failures.append(f"{name}: outputs {keys} changed with no claiming scenario for {sorted(scenario['spec'])}")
        for counter, rate in new.get("steady", {}).items():
            if rate > old.get("steady", {}).get(counter, 0) + STEADY_SLACK:
                failures.append(f"{name}: steady {counter} rose {old['steady'][counter]:.3f} -> {rate:.3f} per iteration")
    return failures


def unknown_specs(tree: Path, scenarios: dict[str, dict]) -> list[str]:
    known = set()
    for spec in (tree / "docs/spec").glob("*.md"):
        known.update(re.findall(r"^### ([A-Z0-9]+(?:-[A-Z0-9]+)*-\d+) ", spec.read_text(), re.M))
    return [f"{name}: unknown spec {rule}" for name, scenario in scenarios.items()
            for rule in scenario.get("spec", []) if rule not in known] + [
        f"{name}: names no spec" for name, scenario in scenarios.items() if not scenario.get("spec")]


def gate(args: argparse.Namespace) -> int:
    base, head = load(args.base_tree), load(args.head_tree)
    if errors := unknown_specs(args.head_tree, head):
        for error in errors:
            print(f"FAIL  {error}")
        return 1
    changed = {n: s for n, s in head.items() if n not in base or base[n]["digest"] != s["digest"]}
    results = {
        "base_on_base": run_all(base, args.base_bin, args.wine),
        "base_on_head": run_all(base, args.head_bin, args.wine),
        "changed_on_head": run_all(changed, args.head_bin, args.wine),
        "changed_again": run_all(changed, args.head_bin, args.wine),
        "changed_on_base": run_all(changed, args.base_bin, args.wine),
    }
    if args.out:
        args.out.write_text(json.dumps(results, indent=1, default=str))
    failures = judge(base, head, results)
    print(f"scenarios: {len(base)} base, {len(changed)} new or changed")
    for failure in failures:
        print(f"FAIL  {failure}")
    if not failures:
        print("ok    every base scenario holds on head")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    one = commands.add_parser("run")
    one.add_argument("--adv-run", type=Path, required=True)
    one.add_argument("--wine", type=Path, required=True)
    one.add_argument("--tree", type=Path, default=Path("."))
    one.add_argument("--out", type=Path)
    one.add_argument("names", nargs="*")
    both = commands.add_parser("gate")
    for flag in ("--base-tree", "--base-bin", "--head-tree", "--head-bin", "--wine"):
        both.add_argument(flag, type=Path, required=True)
    both.add_argument("--out", type=Path)
    args = parser.parse_args(argv)
    if shutil.which(probes.COMPILER) is None:
        sys.exit(f"{probes.COMPILER} is not on PATH (docs/building.md)")
    if args.command == "gate":
        return gate(args)
    scenarios = load(args.tree)
    if args.names:
        scenarios = {n: scenarios[n] for n in args.names}
    results = run_all(scenarios, args.adv_run, args.wine)
    if args.out:
        args.out.write_text(json.dumps(results, indent=1))
    for name, result in results.items():
        print(f"{'ok  ' if result['passed'] else 'FAIL'}  {name} (exit {result['exit']}, {result['stop']})")
    return 0 if all(r["passed"] for r in results.values()) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
