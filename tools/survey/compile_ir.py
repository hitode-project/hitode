#!/usr/bin/env python3
"""Aggregate anonymous ADV_JIT_IR_COUNTS=1 lines, split by Fast/Speed.

Usage: python tools/survey/compile_ir.py <stderr.log> ...
Input: independent runs, each log supplied once; unrelated lines are ignored.
Output: JSON counts, log2 size/service histograms, IR/service Pearson r and
service totals per IR-size bucket. No paths, addresses or log text are emitted.
Only successful requests are observed, including completed but unpublished ones.
IR is counted before optimization across all generated functions; blocks belong
to their first instruction. Shared flags/exits are overhead, not charged to an
arbitrary mnemonic. Decoded counts include non-emitted instructions and clones
can emit an instruction more than once. This is static size, not execution heat.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
import math
from pathlib import Path
import sys

from cliffs import count, mnemonic

PREFIX = "compile-ir: "
CLASSES = {"int", "int.mem", "control", "control.mem", "string", "x87", "x87.mem",
           "sse", "sse.mem", "mmx", "mmx.mem", "overhead"}
PARTS = {"body", "check", "slow", "flags", "entry", "budget", "transfer", "exit",
         "x87-recovery", "untagged", "fault", "unknown"}
COUNTERS = ("guest", "ir", "blocks", "functions", "native_bytes", "service_us")


def validated(raw: object) -> dict:
    """Reject unknown fields and reconcile every request before aggregation."""
    if not isinstance(raw, dict) or set(raw) != set(COUNTERS) | {"version", "strategy", "rows", "decoded"}:
        raise ValueError("invalid request schema")
    if raw["version"] != 1 or raw["strategy"] not in ("fast", "speed"):
        raise ValueError("invalid request version or strategy")
    for key in COUNTERS:
        count(raw[key])
        if raw[key] >= 1 << 64:
            raise ValueError("counter out of range")
    for key, width in (("rows", 5), ("decoded", 3)):
        rows = raw[key]
        if not isinstance(rows, list):
            raise ValueError("invalid count rows")
        seen = set()
        for row in rows:
            if not isinstance(row, list) or len(row) != width:
                raise ValueError("invalid count row")
            mnemonic(row[0])
            if row[1] not in CLASSES or (width == 5 and row[2] not in PARTS):
                raise ValueError("invalid class or part")
            label = tuple(row[:width - 2] if width == 5 else row[:2])
            if label in seen:
                raise ValueError("duplicate count row")
            seen.add(label)
            for value in row[3:] if width == 5 else row[2:]:
                count(value)
    if (sum(r[3] for r in raw["rows"]) != raw["ir"]
            or sum(r[4] for r in raw["rows"]) != raw["blocks"]
            or sum(r[2] for r in raw["decoded"]) != raw["guest"]
            or any(r[4] > r[3] for r in raw["rows"])):
        raise ValueError("request counts do not reconcile")
    return raw


def histogram(target: dict, value: int) -> None:
    target["count"] = target.get("count", 0) + 1
    target["sum"] = target.get("sum", 0) + value
    target["max"] = max(target.get("max", 0), value)
    target.setdefault("log2_bins", [0] * 65)[value.bit_length()] += 1


def aggregate(logs) -> dict:
    """Stream logs; storage is bounded by mnemonic/class enums and 65 bins."""
    result = {}
    moments = {s: Counter() for s in ("fast", "speed")}
    for lines in logs:
        for line in lines:
            if not line.startswith(PREFIX):
                continue
            raw = validated(json.loads(line[len(PREFIX):]))
            strategy = raw["strategy"]
            dest = result.setdefault(strategy, {"requests": 0, "rows": {}, "decoded": {}, "service_by_ir": {}})
            dest["requests"] += 1
            for key in COUNTERS:
                histogram(dest.setdefault(key, {}), raw[key])
            for mnemonic_, class_, part, ir, blocks in raw["rows"]:
                row = dest["rows"].setdefault("/".join((mnemonic_, class_, part)), {"ir": 0, "blocks": 0})
                row["ir"] += ir
                row["blocks"] += blocks
            for mnemonic_, class_, n in raw["decoded"]:
                key = "/".join((mnemonic_, class_))
                dest["decoded"][key] = dest["decoded"].get(key, 0) + n
            bucket = raw["ir"].bit_length()
            group = dest["service_by_ir"].setdefault(bucket, {})
            histogram(group, raw["service_us"])
            x, y = raw["ir"], raw["service_us"]
            moments[strategy].update(n=1, x=x, y=y, xx=x*x, yy=y*y, xy=x*y)
    for strategy, dest in result.items():
        m = moments[strategy]
        vx, vy = m["n"]*m["xx"]-m["x"]**2, m["n"]*m["yy"]-m["y"]**2
        dest["ir_service_pearson_r"] = ((m["n"]*m["xy"]-m["x"]*m["y"]) / math.sqrt(vx*vy)
                                          if vx and vy else None)
        dest["rows"] = dict(sorted(dest["rows"].items(), key=lambda r: (-r[1]["ir"], r[0])))
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", type=Path, nargs="+")
    args = parser.parse_args()
    def lines(path):
        with path.open(encoding="utf-8", errors="replace") as source:
            yield from source
    try:
        result = aggregate(lines(path) for path in args.logs)
        if not result:
            raise ValueError("no compilation counts")
    except (OSError, ValueError, TypeError, KeyError):
        print("invalid or unreadable compilation counts", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=True, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
