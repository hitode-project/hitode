#!/usr/bin/env python3
"""Compare existing loop profiles without running a game or changing its data.

Each directory can contain report.json, pc-attribution.json and native-dumps.json.
Percentages are shares of sampled wall-clock PCs, not execution time or speedup.
Reports and profiles may cover different intervals; they are never multiplied.
Keep generated output local: instruction addresses and scenario names are private.
"""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path


HISTOGRAMS = {
    "frame_wall": "warm_wall_time_between_frame_observations",
    "frame_active": "warm_active_runtime_between_frame_observations",
    "present_interval": "warm_host_present_intervals",
    "present_cost": "warm_host_present_cost",
}
GROUPS = ("groups_pct", "jit_parts_pct", "jit_classes_pct")
IDENTITY = ("backend", "compile_strategy", "region_linking", "codepage",
            "width", "height", "stop_reason")


def numeric(value, name):
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name}: expected a number")
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"{name}: expected a finite nonnegative number")
    return value


def read_optional(directory, name):
    path = directory / name
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8-sig"))


def load(directory, window):
    report = read_optional(directory, "report.json")
    attribution = read_optional(directory, "pc-attribution.json")
    dumps = read_optional(directory, "native-dumps.json")
    if report is None and attribution is None and dumps is None:
        raise ValueError(f"{directory}: no supported input files")
    selected = None
    if attribution is not None:
        if window not in attribution.get("windows", {}):
            raise ValueError(f"{directory}: attribution window {window!r} is absent")
        selected = attribution["windows"][window]
        count = selected.get("samples")
        if type(count) is not int or count < 0:
            raise ValueError("attribution samples must be a nonnegative integer")

    metrics = {}
    if report is not None:
        for name in ("frames", "elapsed_seconds", "first_frame_seconds",
                     "native_entries", "interpreter_entries", "compile_seconds"):
            metrics[name] = numeric(report.get(name), name)
        for name in ("device_errors", "lock_misses", "device_sample_rate"):
            metrics["audio." + name] = numeric(report.get("audio", {}).get(name), name)
        performance = report.get("performance", {})
        for label, name in HISTOGRAMS.items():
            histogram = performance.get(name, {})
            count = numeric(histogram.get("samples"), name + ".samples")
            metrics[label + ".samples"] = count
            for field in ("percentile_window_samples", "percentile_window_capacity"):
                metrics[label + "." + field] = numeric(histogram.get(field), field)
            for field in ("p50_ms", "p95_ms", "p99_ms", "max_ms"):
                metrics[label + "." + field] = (
                    numeric(histogram.get(field), name + "." + field)
                    if count is not None and count > 0 else None)

    code = {}
    for entry in dumps or []:
        guest = entry["guest"]
        if type(guest) is not int or not 0 <= guest <= 0xffffffff:
            raise ValueError("native dump guest must be a 32-bit integer address")
        key = f"{guest:#x}/{entry['mode']}"
        if key in code:
            raise ValueError(f"multiple code snapshots for {key}; select one snapshot first")
        code[key] = numeric(entry["size"], key)
    return {"report": report, "attribution": attribution, "window": selected,
            "metrics": metrics, "code": code}


def rows(left, right, missing_zero=False):
    def value(mapping, key):
        return numeric(mapping.get(key, 0 if missing_zero else None), key)
    result = []
    for key in sorted(left.keys() | right.keys()):
        a, b = value(left, key), value(right, key)
        result.append({"name": key, "baseline": a, "candidate": b,
                       "delta": b - a if a is not None and b is not None else None})
    return result


def compare(baseline, candidate, window="all", top=12):
    warnings = [
        "Descriptive comparison only: scenario, input replay, warm-up and build identity "
        "must be matched externally. No speedup or correctness verdict is inferred.",
        "Profile shares use each window's total samples (including non-JIT samples). "
        "A smaller share does not prove that the corresponding work became faster.",
        "Report timings and PC samples may cover different intervals; no sampled "
        "percentage is converted into milliseconds.",
    ]
    for label, source in (("baseline", baseline), ("candidate", candidate)):
        report, attribution = source["report"], source["attribution"]
        if report is None:
            warnings.append(f"{label}: report.json absent; frame/audio metrics unavailable.")
        elif report.get("failure") is not None:
            warnings.append(f"{label}: guest failure reported; not a successful run.")
        if attribution is None:
            warnings.append(f"{label}: pc-attribution.json absent; sample shares unavailable.")
        elif not attribution.get("instruction_map"):
            warnings.append(f"{label}: instruction map absent; detailed JIT attribution is incomplete.")
        if source["window"] is not None and source["window"]["samples"] == 0:
            warnings.append(f"{label}: empty attribution window; shares are unavailable, not zero.")

    a, b = baseline["report"], candidate["report"]
    if a is not None and b is not None:
        for key in ("exe", *IDENTITY):
            if a.get(key) != b.get(key):
                warnings.append(f"Report identity differs: {key}.")
        if a.get("frames") != b.get("frames"):
            warnings.append("Frame counts differ; total runtime is not equal-work evidence.")
        if a.get("frame_fnv1a64") != b.get("frame_fnv1a64"):
            warnings.append("Final frame checksums differ; do not infer equivalent work.")
    pa, pb = baseline["attribution"], candidate["attribution"]
    if pa is not None and pb is not None and pa.get("long_ms") != pb.get("long_ms"):
        warnings.append("Long-frame thresholds differ between attribution files.")

    sections = {"report_metrics": rows(baseline["metrics"], candidate["metrics"]),
                "native_bytes": rows(baseline["code"], candidate["code"])}
    windows = [source["window"] for source in (baseline, candidate)]
    for group in GROUPS:
        maps = [w.get(group, {}) if w and w["samples"] else {} for w in windows]
        complete = all(w and w["samples"] and group in w for w in windows)
        sections[group] = rows(*maps, missing_zero=complete)

    # guest_instructions is a top-N list. Absence is UNKNOWN, never zero.
    maps = []
    for w in windows:
        entries = w.get("guest_instructions", []) if w and w["samples"] else []
        maps.append({f"{int(e['pc'], 16):#x}/{e['class']}/{e['part']}": e["pct"]
                     for e in entries})
    sections["guest_top_pct"] = sorted(
        rows(*maps), key=lambda r: max(r["baseline"] or 0, r["candidate"] or 0),
        reverse=True)[:top]
    return {"schema_version": 1, "window": window,
            "samples": {label: source["window"]["samples"] if source["window"] else None
                        for label, source in (("baseline", baseline), ("candidate", candidate))},
            "warnings": warnings, "sections": sections}


def markdown(result):
    lines = ["# Offline loop profile comparison", "",
             f"Window: `{result['window']}`. Samples: {result['samples']}", ""]
    lines += [f"- {warning}" for warning in result["warnings"]]
    for name, values in result["sections"].items():
        if not values:
            continue
        unit = "percentage points" if name.endswith("_pct") else "same unit as metric"
        lines += ["", f"## {name}", "", f"Delta = candidate − baseline ({unit}).",
                  "", "| Metric | Baseline | Candidate | Delta |",
                  "|---|---:|---:|---:|"]
        for row in values:
            cells = ["—" if row[k] is None else f"{row[k]:.4f}".rstrip("0").rstrip(".")
                     for k in ("baseline", "candidate", "delta")]
            label = row['name'].replace('|', '\\|').replace('\n', ' ')
            lines.append(f"| {label} | {' | '.join(cells)} |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--window", default="all")
    parser.add_argument("--top", type=int, default=12)
    parser.add_argument("--json", type=Path, help="optional new local JSON output file")
    args = parser.parse_args()
    if args.top <= 0:
        parser.error("--top must be positive")
    try:
        result = compare(load(args.baseline, args.window), load(args.candidate, args.window),
                         args.window, args.top)
        if args.json:
            args.json.parent.mkdir(parents=True, exist_ok=True)
            with args.json.open("x", encoding="utf-8") as stream:
                json.dump(result, stream, indent=2, allow_nan=False)
                stream.write("\n")
        print(markdown(result), end="")
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"loop profile comparison: {error}\n")


if __name__ == "__main__":
    main()
