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

A structural warning (a fixed personality contract the run observed broken)
that no baseline run shows regresses, as does a new shader/declaration
warning. --require-frames, the startup smoke threshold, also regresses every
candidate run without a frame. New personality gaps, rejected discovery probes
and reviewed-export calls, are listed as changes: they show what a personality
change made the guest see, not a failure by themselves.
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


def structural_warnings(results: list[dict]) -> set[tuple[str, str]]:
    """(api, reason) of every process's structural warnings."""
    found = set()
    for result in results:
        api = result.get("api") or {}
        for process in api.get("processes") or []:
            for warning in process.get("structural_warnings") or []:
                found.add((warning.get("api", ""), warning.get("reason", "")))
        if api.get("structural_mismatches") and not api.get("processes"):
            found.add(("?", "structural mismatch without process detail"))
    return found


def personality_gaps(results: list[dict]) -> set[str]:
    """Rejected probe and reviewed-call names, labelled by kind."""
    found = set()
    for result in results:
        personality = (result.get("api") or {}).get("personality") or {}
        for probe in personality.get("rejected_probes") or []:
            found.add(f"rejected {probe.get('api')} ({probe.get('class') or 'undeclared'})")
        for api in personality.get("reviewed_calls") or {}:
            found.add(f"reviewed {api}")
    return found


def compare(baseline: list[dict], candidate: list[dict], tolerance: float,
            require_frames: bool = False) -> dict:
    before = {outcome(result) for result in baseline}
    after = {outcome(result) for result in candidate}
    frames = lambda results: [result.get("frames") or 0 for result in results]
    calls = lambda results: [result.get("host_calls") or 0 for result in results]
    reasons = []
    def shader_warnings(results):
        return {(warning.get("api", ""), warning.get("reason", ""))
                for result in results for warning in (result.get("api") or {}).get("warnings", [])
                if re.search(r"shader|declaration|programmable", warning.get("reason", ""), re.I)
                or re.search(r"Create(?:Vertex|Pixel)Shader", warning.get("api", ""), re.I)}
    for api, reason in sorted(shader_warnings(candidate) - shader_warnings(baseline)):
        reasons.append(f"new shader/declaration warning: {api}: {reason}")
    for api, reason in sorted(structural_warnings(candidate) - structural_warnings(baseline)):
        reasons.append(f"new structural warning: {api}: {reason}")
    if not candidate:
        reasons.append("no candidate run")
    if require_frames and any(not result.get("frames") for result in candidate):
        reasons.append("a candidate run produced no frame")
    for status, kind, site in sorted(after - before):
        if status in FAILING:
            reasons.append(f"new {status}/{kind}" + (f" at {site}" if site != "-" else ""))
    least = min(frames(baseline), default=0)
    if candidate and least > 0 and max(frames(candidate)) < least * (1 - tolerance):
        reasons.append(f"frames {max(frames(candidate))} < {least}")
    gaps = sorted(personality_gaps(candidate) - personality_gaps(baseline))
    changed = before != after or bool(gaps)
    return {
        "verdict": "regressed" if reasons else "changed" if changed else "same",
        "reasons": reasons,
        "new": sorted("/".join(o) for o in after - before),
        "gone": sorted("/".join(o) for o in before - after),
        "new_personality_gaps": gaps,
        "runs": [len(baseline), len(candidate)],
        "frames": [frames(baseline), frames(candidate)],
        "host_calls_median": [statistics.median(calls(side)) if side else None
                              for side in (baseline, candidate)],
    }


def gate(baseline: dict[str, list[dict]], candidate: dict[str, list[dict]],
         tolerance: float, require_frames: bool = False) -> dict[str, dict]:
    return {sample: compare(baseline.get(sample, []), candidate.get(sample, []), tolerance,
                            require_frames)
            for sample in sorted(set(baseline) | set(candidate))}


def table(verdicts: dict[str, dict]) -> str:
    rows = ["| Sample | Verdict | Runs | Frames (baseline / candidate) | Reasons and changes |",
            "|---|---|---|---|---|"]
    for sample, entry in verdicts.items():
        notes = (entry["reasons"] + [f"+{o}" for o in entry["new"]] + [f"-{o}" for o in entry["gone"]]
                 + [f"+{gap}" for gap in entry["new_personality_gaps"]])
        frames = " / ".join(",".join(map(str, side)) or "-" for side in entry["frames"])
        rows.append(f"| {sample} | {entry['verdict']} | {entry['runs'][0]}/{entry['runs'][1]} "
                    f"| {frames} | {'; '.join(notes) or '-'} |")
    return "\n".join(rows) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", nargs="+", required=True, type=Path)
    parser.add_argument("--candidate", nargs="+", required=True, type=Path)
    parser.add_argument("--frame-tolerance", type=float, default=0.25)
    parser.add_argument("--require-frames", action="store_true",
                        help="regress any candidate run without a frame (startup smoke)")
    parser.add_argument("--json", type=Path, help="also write the verdicts here")
    args = parser.parse_args(argv)
    verdicts = gate(load(args.baseline), load(args.candidate), args.frame_tolerance,
                    args.require_frames)
    sys.stdout.write(table(verdicts))
    if args.json:
        args.json.write_text(json.dumps(verdicts, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 1 if any(entry["verdict"] == "regressed" for entry in verdicts.values()) else 0


if __name__ == "__main__":
    sys.exit(main())
