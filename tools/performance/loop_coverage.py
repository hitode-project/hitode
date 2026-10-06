#!/usr/bin/env python3
"""Rank active loop coverage gaps without confusing static loop counts with cost.

Input is adv-run's *.loops.tsv. Work is an approximate instruction weight, NOT
CPU time: nested loops can double count, and startup/unpacking can dominate.
Each input report contributes equal weight to the aggregate. Keep API-blocked
and playable samples distinct when selecting inputs for a follow-up run.
"""
from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path


def read_report(path: Path) -> dict:
    rows = []
    for line in path.read_text(encoding="utf-8-sig").splitlines():
        if not line.startswith("0x"):
            continue
        fields = line.split("\t")
        if len(fields) != 13:
            raise ValueError(f"{path}: expected 13 loop columns, got {len(fields)}")
        region, header, latch, length, outcome, detail = fields[:6]
        checked, kernel, calls, failures, work = map(int, fields[6:11])
        if min(checked, kernel, calls, failures, work) < 0:
            raise ValueError(f"{path}: negative counter")
        if work:
            rows.append(dict(region=region, header=header, latch=latch,
                             instructions=int(length), outcome=outcome, detail=detail,
                             checked=checked, kernel=kernel, calls=calls,
                             failures=failures, work=work, kernel_work=kernel * int(length),
                             shape=fields[12]))
    total = sum(row["work"] for row in rows)
    for row in rows:
        row["share"] = row["work"] / total
    rows.sort(key=lambda row: (-row["work"], row["header"]))
    report = path.with_name(path.name.removesuffix(".loops.tsv") + ".json")
    status = {}
    if report.exists():
        data = json.loads(report.read_text(encoding="utf-8-sig"))
        status = {key: data.get(key) for key in
                  ("stop_reason", "frames", "elapsed_seconds", "guest_exit_code")}
    return dict(path=str(path), status=status, total_work=total,
                executed_kernel_share=sum(row["kernel_work"] for row in rows) / total if total else 0.0,
                loops=rows)


def summarize(paths: list[Path]) -> dict:
    reports = [read_report(path) for path in paths]
    active = [report for report in reports if report["total_work"]]
    categories = defaultdict(lambda: dict(share=0.0, samples=set(), active_loops=0))
    for report in active:
        for row in report["loops"]:
            category = categories[row["outcome"]]
            category["share"] += row["share"] / len(active)
            category["samples"].add(report["path"])
            category["active_loops"] += 1
    ranked = [dict(outcome=key, **{**value, "samples": sorted(value["samples"])})
              for key, value in categories.items()]
    ranked.sort(key=lambda row: (-row["share"], row["outcome"]))
    return dict(metric="equal-report mean of approximate loop instruction weight",
                warning="Not CPU time or speedup; nesting and startup can dominate.",
                executed_kernel_share=sum(r["executed_kernel_share"] for r in active) / len(active) if active else 0.0,
                reports=reports, categories=ranked)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--limit", type=int, default=15)
    args = parser.parse_args()
    paths = sorted({path.resolve() for entry in args.inputs
                    for path in (entry.rglob("*.loops.tsv") if entry.is_dir() else [entry])})
    if not paths:
        parser.error("no *.loops.tsv reports found")
    result = summarize(paths)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n",
                               encoding="utf-8")
    print(result["warning"])
    print(f"Reports: {len(paths)}; active: {sum(bool(r['total_work']) for r in result['reports'])}")
    print(f"Actually executed in kernels (same approximate weight): {result['executed_kernel_share']:.2%}")
    for row in result["categories"][:max(0, args.limit)]:
        print(f"{row['share']:7.2%}  {len(row['samples']):2} samples  "
              f"{row['active_loops']:5} loops  {row['outcome']}")


if __name__ == "__main__":
    main()
