#!/usr/bin/env python3
"""Run alternating same-binary x87 A/B trials and reject invalid comparisons.

Timings are diagnostics, not a shared-runner speed gate. Correctness, coverage,
mode, work counts and checksums are hard gates. Each benchmark invocation also
checks every timed result against its transactional interpreter reference.
"""
from __future__ import annotations

import argparse
import csv
import io
import json
import math
import os
from pathlib import Path
import re
import statistics
import subprocess

WORKLOADS = {"load_store": 2, "add_sub": 4, "mul_div": 4,
             "compare_exchange": 6, "pcm": 3, "interpolation": 5}
FIELDS = ["mode", "workload", "samples", "iterations", "x87_ops",
          "median_ns_per_op", "p95_ns_per_op", "max_ns_per_op",
          "compile_ms", "native_bytes", "checksum"]
INTEGERS = ("samples", "iterations", "x87_ops", "native_bytes")
TIMINGS = ("median_ns_per_op", "p95_ns_per_op", "max_ns_per_op", "compile_ms")


def parse(text: str, mode: str) -> dict[str, dict]:
    reader = csv.DictReader(io.StringIO(text.lstrip("\ufeff")))
    if reader.fieldnames != FIELDS:
        raise ValueError("unexpected x87 benchmark CSV header")
    rows = {}
    for original in reader:
        row = dict(original)
        name = row.get("workload")
        if None in row or any(value is None for value in row.values()):
            raise ValueError("malformed benchmark row")
        if name not in WORKLOADS or name in rows or row["mode"] != mode:
            raise ValueError(f"unexpected/duplicate workload or mode: {name}")
        for field in INTEGERS:
            row[field] = int(row[field])
            if row[field] <= 0:
                raise ValueError(f"nonpositive {field}: {name}")
        for field in TIMINGS:
            row[field] = float(row[field])
            if not math.isfinite(row[field]) or row[field] <= 0:
                raise ValueError(f"invalid {field}: {name}")
        if not row["median_ns_per_op"] <= row["p95_ns_per_op"] <= row["max_ns_per_op"]:
            raise ValueError(f"unordered quantiles: {name}")
        if row["x87_ops"] != WORKLOADS[name]:
            raise ValueError(f"incorrect operation count: {name}")
        if re.fullmatch(r"[0-9a-f]{16}", row["checksum"]) is None:
            raise ValueError(f"invalid checksum: {name}")
        rows[name] = row
    if set(rows) != set(WORKLOADS):
        raise ValueError("missing benchmark workloads")
    return rows


def compare(helper: dict, native: dict) -> None:
    if set(helper) != set(WORKLOADS) or set(native) != set(WORKLOADS):
        raise ValueError("incomplete comparison")
    for name in WORKLOADS:
        for field in ("samples", "iterations", "x87_ops", "checksum"):
            if helper[name][field] != native[name][field]:
                raise ValueError(f"{name}: {field} differs between trials")


def summarize(pairs: list[tuple[dict, dict]]) -> dict:
    if not pairs:
        raise ValueError("no benchmark pairs")
    rows = []
    reference = pairs[0][0]
    for helper, native in pairs:
        compare(helper, native)
        compare(reference, helper)
    for name in WORKLOADS:
        h = [pair[0][name] for pair in pairs]
        n = [pair[1][name] for pair in pairs]
        median = lambda group, key: statistics.median(row[key] for row in group)
        ratios = [a["median_ns_per_op"] / b["median_ns_per_op"] for a, b in zip(h, n)]
        rows.append({
            "workload": name, "rounds": len(pairs),
            "samples_per_round": h[0]["samples"], "iterations": h[0]["iterations"],
            "x87_ops": WORKLOADS[name], "checksum": h[0]["checksum"],
            "helper_median_ns_per_op": median(h, "median_ns_per_op"),
            "native_median_ns_per_op": median(n, "median_ns_per_op"),
            "speedup_median": statistics.median(ratios),
            "speedup_min": min(ratios), "speedup_max": max(ratios),
            "helper_median_of_p95_ns_per_op": median(h, "p95_ns_per_op"),
            "native_median_of_p95_ns_per_op": median(n, "p95_ns_per_op"),
            "helper_max_ns_per_op": max(row["max_ns_per_op"] for row in h),
            "native_max_ns_per_op": max(row["max_ns_per_op"] for row in n),
            "helper_compile_ms": median(h, "compile_ms"),
            "native_compile_ms": median(n, "compile_ms"),
            "helper_native_bytes": median(h, "native_bytes"),
            "native_native_bytes": median(n, "native_bytes"),
        })
    return {"schema": 1, "acceptance": "synthetic-only",
            "aggregation": "median of paired-round medians; p95 columns are not pooled quantiles",
            "workloads": rows}


def run(binary: Path, output: Path, rounds: int = 5, samples: int = 31) -> dict:
    if rounds < 2 or samples < 1:
        raise ValueError("at least two alternating rounds and positive samples are required")
    output.mkdir(parents=True, exist_ok=True)
    # Remove a stale success summary before doing any work that can fail.
    (output / "summary.json").unlink(missing_ok=True)
    binary = binary.resolve(strict=True)
    pairs = []
    for index in range(rounds):
        order = ("helper", "native") if index % 2 == 0 else ("native", "helper")
        trial = {}
        for mode in order:
            env = os.environ.copy()
            env.pop("ADV_JIT_X87_HELPER_ONLY", None)
            if mode == "helper":
                env["ADV_JIT_X87_HELPER_ONLY"] = "1"
            result = subprocess.run([str(binary), str(samples)], env=env,
                                    capture_output=True, text=True, timeout=180, check=False)
            stem = output / f"round-{index + 1:02d}-{mode}"
            stem.with_suffix(".csv").write_text(result.stdout, encoding="utf-8")
            stem.with_suffix(".stderr.txt").write_text(result.stderr, encoding="utf-8")
            if result.returncode:
                raise RuntimeError(f"{mode} round {index + 1} failed: {result.stderr[-2000:]}")
            trial[mode] = parse(result.stdout, mode)
            if any(row["samples"] != samples for row in trial[mode].values()):
                raise ValueError("benchmark did not run the requested sample count")
        compare(trial["helper"], trial["native"])
        pairs.append((trial["helper"], trial["native"]))
    summary = summarize(pairs)
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--samples", type=int, default=31)
    args = parser.parse_args()
    summary = run(args.binary, args.output, args.rounds, args.samples)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
