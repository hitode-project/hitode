#!/usr/bin/env python3
"""Turn a user report or a run report into a list of gaps and next commands.

Usage: python tools/survey/triage_report.py <report.json> [--root DIR]
Input: the launcher's anonymous report (schema hitode.report/1), which names
only the first missing API, or an `adv-run --report` run report, whose
`missing_apis` lists every gap the run reached.
Output: the gaps grouped by DLL with where.py's registration status and
source, then the next step for the failure kind: rerun in discovery mode for
an anonymous API failure, the backend checks for a CPU fault, the debugger for
performance issues. Nothing in the report identifies a game, so commands name
the executable as a placeholder.
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import where  # noqa: E402

ANONYMOUS = "hitode.report/1"
RUN = ("cargo run -p adv-run --release --features adv-shell/gpu,adv-shell/media -- --audio "
       "--codepage {codepage} --api-mode discovery "
       "--data-dir target/investigations/<topic>/data --report target/investigations/<topic>/report.json "
       '"$ADV_SAMPLES/<game>/<entry>.exe"')


def gaps(report: dict) -> list[str]:
    if report.get("schema") == ANONYMOUS:
        failure = report.get("failure") or {}
        return [failure["api"]] if failure.get("kind") == "api" and failure.get("api") else []
    return [api for api in report.get("missing_apis", []) if isinstance(api, str)]


def next_steps(report: dict) -> list[str]:
    steps = []
    failure = report.get("failure") or {}
    kind = failure.get("kind") if isinstance(failure, dict) else None
    codepage = (report.get("game") or {}).get("codepage", "auto")
    if report.get("schema") == ANONYMOUS:
        if kind == "api":
            steps.append("List every gap at once (the anonymous report names only the first):\n  "
                         + RUN.format(codepage=codepage))
        elif kind == "cpu":
            steps.append(f"CPU fault {failure.get('cpu_fault') or ''}: reproduce, then compare backends "
                         "(docs/investigation.md, CPU and JIT): --backend interpreter, or ADV_JIT_VERIFY=1 "
                         "when the interpreter cannot reach it.")
        elif kind == "runtime":
            steps.append("Runtime error: reproduce with --report and read its `failure` message.")
        if report.get("perf_issues"):
            steps.append("Performance (" + ", ".join(report["perf_issues"]) + "): attach the live debugger "
                         "(ADV_DEBUG=1, tools/debug/hitode_debug.py watch) while a person plays.")
    elif report.get("failure"):
        steps.append("Run failure: " + json.dumps(report["failure"], ensure_ascii=False)[:300])
    return steps


def triage(report: dict, root: Path = where.ROOT) -> str:
    apis = gaps(report)
    out = []
    if apis:
        by_dll: dict[str, list[str]] = defaultdict(list)
        for api in apis:
            by_dll[where.split(api)[0] or "?"].append(api)
        places = dict(zip(apis, where.locate([where.split(a)[1] for a in apis], root)))
        for dll in sorted(by_dll):
            out.append(f"## {dll} ({len(by_dll[dll])})")
            out.extend(where.render(places[api], dll) for api in sorted(by_dll[dll]))
    else:
        out.append("No missing APIs in this report.")
    steps = next_steps(report)
    if steps:
        out.append("## Next")
        out.extend(f"- {step}" for step in steps)
    return "\n".join(out)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("report", type=Path)
    parser.add_argument("--root", type=Path, default=where.ROOT)
    args = parser.parse_args(argv)
    print(triage(json.loads(args.report.read_text(encoding="utf-8")), args.root))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
