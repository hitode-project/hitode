#!/usr/bin/env python3
"""Merge gate over repeated sample_matrix.py runs of two builds.

Run sample_matrix.py several times per build (one --output each, same manifest
and options), then compare. Guests are not deterministic: a sample may end in
different ways under one build, so the baseline is the set of outcomes seen in
all its runs, and a candidate only regresses when it shows something new.

An outcome is the status, the failure kind and, for a CPU or SEH failure, the
guest address it names; a new address catches a jump to garbage. A candidate
also regresses when its best frame count stays below the baseline's worst.
Outcomes the candidate no longer shows are reported as changes, not failures.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import statistics
import sys

FAILING = {"blocked", "diagnostic_error", "external_timeout", "launch_error"}
SITE = re.compile(r"\bat (0x[0-9a-fA-F]+)")


def outcome(result: dict) -> tuple[str, str, str]:
    kind = result.get("failure_kind") or "-"
    site = "-"
    if kind not in ("-", "called_stub") and result.get("failure"):
        match = SITE.search(result["failure"].splitlines()[0])
        if match:
            site = match.group(1).lower()
    return result.get("status") or "unknown", kind, site


def load(directories: list[Path]) -> dict[str, list[dict]]:
    """Results per sample id, one entry per run directory that has it."""
    runs: dict[str, list[dict]] = {}
    for directory in directories:
        results = json.loads((directory / "results.json").read_text(encoding="utf-8"))
        for result in results["samples"]:
            runs.setdefault(result["id"], []).append(result)
    return runs


def compare(baseline: list[dict], candidate: list[dict], tolerance: float) -> dict:
    before = {outcome(result) for result in baseline}
    after = {outcome(result) for result in candidate}
    frames = lambda results: [result.get("frames") or 0 for result in results]
    calls = lambda results: [result.get("host_calls") or 0 for result in results]
    reasons = []
    if not candidate:
        reasons.append("no candidate run")
    for status, kind, site in sorted(after - before):
        if status in FAILING:
            reasons.append(f"new {status}/{kind}" + (f" at {site}" if site != "-" else ""))
    least = min(frames(baseline), default=0)
    if candidate and least > 0 and max(frames(candidate)) < least * (1 - tolerance):
        reasons.append(f"frames {max(frames(candidate))} < {least}")
    changed = before != after
    return {
        "verdict": "regressed" if reasons else "changed" if changed else "same",
        "reasons": reasons,
        "new": sorted("/".join(o) for o in after - before),
        "gone": sorted("/".join(o) for o in before - after),
        "runs": [len(baseline), len(candidate)],
        "frames": [frames(baseline), frames(candidate)],
        "host_calls_median": [statistics.median(calls(side)) if side else None
                              for side in (baseline, candidate)],
    }


def gate(baseline: dict[str, list[dict]], candidate: dict[str, list[dict]],
         tolerance: float) -> dict[str, dict]:
    return {sample: compare(baseline.get(sample, []), candidate.get(sample, []), tolerance)
            for sample in sorted(set(baseline) | set(candidate))}


def table(verdicts: dict[str, dict]) -> str:
    rows = ["| Sample | Verdict | Runs | Frames (baseline / candidate) | Reasons and changes |",
            "|---|---|---|---|---|"]
    for sample, entry in verdicts.items():
        notes = entry["reasons"] + [f"+{o}" for o in entry["new"]] + [f"-{o}" for o in entry["gone"]]
        frames = " / ".join(",".join(map(str, side)) or "-" for side in entry["frames"])
        rows.append(f"| {sample} | {entry['verdict']} | {entry['runs'][0]}/{entry['runs'][1]} "
                    f"| {frames} | {'; '.join(notes) or '-'} |")
    return "\n".join(rows) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", nargs="+", required=True, type=Path)
    parser.add_argument("--candidate", nargs="+", required=True, type=Path)
    parser.add_argument("--frame-tolerance", type=float, default=0.25)
    parser.add_argument("--json", type=Path, help="also write the verdicts here")
    args = parser.parse_args(argv)
    verdicts = gate(load(args.baseline), load(args.candidate), args.frame_tolerance)
    sys.stdout.write(table(verdicts))
    if args.json:
        args.json.write_text(json.dumps(verdicts, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 1 if any(entry["verdict"] == "regressed" for entry in verdicts.values()) else 0


if __name__ == "__main__":
    sys.exit(main())
