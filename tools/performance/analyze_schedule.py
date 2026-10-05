#!/usr/bin/env python3
"""Summarize --schedule-profile boundaries (host wall intervals, not CPU time).

Usage: python tools/performance/analyze_schedule.py RUN_DIRECTORY
Reads schedule.tsv from the first reset-metrics mark to the final report event.
Only completed lock-wait attempts wholly inside that window enter the lock table.
A failed reacquire starts a new attempt; it is not attributed to the old owner.
End-PC attribution is a boundary observation, not instruction-level profiling.
Frame gaps use consecutive guest completions, not shell observations or scanout;
static reading and intentional sleeps can also produce long gaps.
"""

import argparse
from collections import Counter, defaultdict
import csv
import json
from pathlib import Path


def read_trace(path):
    with path.open(encoding="utf-8", newline="") as stream:
        return [
            {key: value if key == "event" else int(value) for key, value in row.items()}
            for row in csv.DictReader(stream, delimiter="\t")
        ]


def overlap(begin, end, lo, hi):
    return max(0, min(end, hi) - max(begin, lo))


def frame_gaps(rows, runs, waits, lo, hi):
    frames = [r for r in rows if r["event"] == "frame"]
    gaps = [
        (b["time_ns"] - a["time_ns"], a, b)
        for a, b in zip(frames, frames[1:])
        if lo <= a["time_ns"] < b["time_ns"] <= hi
        and b["a"] == ((a["a"] + 1) & 0xffffffff)
    ]
    result = []
    for duration, first, last in sorted(gaps, key=lambda gap: gap[0], reverse=True)[:10]:
        begin, end = first["time_ns"], last["time_ns"]
        active = defaultdict(lambda: {"runs": 0, "wall_ms": 0, "host_ms": 0})
        for start, stop, context, _, _, host in runs:
            ns = overlap(start, stop, begin, end)
            if ns:
                active[context]["runs"] += 1
                active[context]["wall_ms"] += ns / 1e6
                active[context]["host_ms"] += host * ns / max(stop - start, 1) / 1e6
        # Include waits crossing the frame boundary, but retain actual begin/end
        # so overlap is never mistaken for the full acquisition latency.
        relevant = [
            {**wait, "overlap_ms": overlap(wait["begin"], wait["end"], begin, end) / 1e6}
            for wait in waits if overlap(wait["begin"], wait["end"], begin, end)
        ]
        result.append({
            "frame_start": first["a"], "frame_end": last["a"],
            "begin_ns": begin, "end_ns": end, "wall_ms": duration / 1e6,
            "inline_runs": dict(active), "completed_lock_waits": relevant,
        })
    return result


def analyze(rows):
    if not rows:
        raise ValueError("empty scheduling trace")
    if any(row["event"] == "truncated" for row in rows):
        raise ValueError("truncated trace: no complete interval analysis")
    if any(a["time_ns"] > b["time_ns"] for a, b in zip(rows, rows[1:])):
        raise ValueError("non-monotonic trace")
    lo = next((r["time_ns"] for r in rows if r["event"] == "mark"), rows[0]["time_ns"])
    hi = max((r["time_ns"] for r in rows if r["event"] == "observe"), default=rows[-1]["time_ns"])
    if hi <= lo:
        raise ValueError("no observation after measurement start")

    state, since, owners, waits, starts = {}, {}, {}, {}, {}
    segments = defaultdict(list)
    records, runs = [], []

    def change(context, time, new_state):
        if context in state:
            segments[context].append((since[context], time, state[context]))
        state[context], since[context] = new_state, time

    def finish_wait(key, time, outcome):
        wait = waits.pop(key, None)
        if wait is not None:
            records.append({**wait, "end": time, "outcome": outcome})

    for row in rows:
        time, event, context, a, b, c = (row[k] for k in ("time_ns", "event", "context", "a", "b", "c"))
        if event in ("pick", "run_start"):
            change(context, time, "owner")
        if event == "run_start":
            starts[context] = time
        elif event == "run_end":
            begin = starts.pop(context, time - b)
            runs.append((begin, time, context, a, b, c))
            change(context, time, "ready")
        elif event == "lease":
            change(context, time, "leased")
        elif event in ("wake", "collect"):
            change(context, time, "ready")
        elif event == "block":
            change(context, time, "timer" if a == 1 else "object")
        elif event == "yield":
            change(context, time, "polling")

        if event == "cs_wait":
            finish_wait((context, a), time, "retry")
            waits[(context, a)] = {
                "ctx": context, "lock": a, "owner": owners.get(a),
                "owner_tid": b, "begin": time, "release": None, "wake": None,
            }
        elif event == "wake":
            for wait in waits.values():
                if wait["ctx"] == context and wait["wake"] is None:
                    wait["wake"] = time
        elif event == "cs_take":
            owners[a] = context
            finish_wait((context, a), time, "acquired")
        elif event == "cs_release" and b == 0:
            owners.pop(a, None)
            for wait in waits.values():
                if wait["lock"] == a and wait["release"] is None:
                    wait["release"] = time
    for context in state:
        change(context, rows[-1]["time_ns"], state[context])

    active, pcs = Counter(), Counter()
    states = defaultdict(Counter)
    for begin, end, context, end_pc, wall, host in runs:
        duration = overlap(begin, end, lo, hi)
        if not duration:
            continue
        fraction = duration / max(end - begin, 1)
        active[f"ctx{context}_wall_ms"] += duration / 1e6
        active[f"ctx{context}_host_ms"] += host * fraction / 1e6
        pcs[(context, end_pc)] += max(0, wall - host) * fraction / 1e6
    for context, intervals in segments.items():
        for begin, end, state_name in intervals:
            states[context][state_name] += overlap(begin, end, lo, hi) / 1e6

    locks = defaultdict(lambda: {
        "count": 0, "acquired_count": 0, "retry_count": 0,
        "wait_ms": 0, "to_release_ms": 0, "handoff_ms": 0,
        "max_wait_ms": 0, "missing_release_count": 0, "owner_state_ms": Counter(),
    })
    selected = [w for w in records if w["begin"] >= lo and w["end"] <= hi]
    for wait in selected:
        entry = locks[(wait["lock"], wait["ctx"], wait["owner"])]
        entry["count"] += 1
        entry[f"{wait['outcome']}_count"] += 1
        duration = (wait["end"] - wait["begin"]) / 1e6
        entry["wait_ms"] += duration
        entry["max_wait_ms"] = max(duration, entry["max_wait_ms"])
        if wait["release"] is None:
            entry["missing_release_count"] += 1
            continue
        entry["to_release_ms"] += (wait["release"] - wait["begin"]) / 1e6
        # Includes another owner's work if the lock was stolen before reacquire.
        entry["handoff_ms"] += (wait["end"] - wait["release"]) / 1e6
        for begin, end, state_name in segments.get(wait["owner"], ()):
            duration = overlap(begin, end, wait["begin"], wait["release"])
            if duration:
                entry["owner_state_ms"][state_name] += duration / 1e6

    summary = {
        "interval_kind": "host wall; leased includes queueing and completion delivery",
        "boundary_note": "host/end-PC time is prorated for runs crossing the measurement boundary",
        "lock_note": "count is completed attempts; handoff includes competing acquisition, not only scheduler delay",
        "incomplete_lock_waits": len(waits),
        "excluded_boundary_lock_waits": len(records) - len(selected),
        "measured_ms": (hi - lo) / 1e6,
        "inline_runs": dict(active),
        "states_ms": dict(states),
        "locks": [
            {"lock": hex(lock), "waiter": waiter, "owner": owner, **value}
            for (lock, waiter, owner), value in sorted(locks.items(), key=lambda item: item[1]["wait_ms"], reverse=True)
        ],
        "end_pc_attribution_ms": [
            {"context": context, "pc": hex(pc), "ms": ms}
            for (context, pc), ms in pcs.most_common(20)
        ],
        "frame_gap_note": "guest completions; inline host time is prorated at boundaries; completed waits only",
        "longest_frame_gaps": frame_gaps(rows, runs, records, lo, hi),
    }
    return summary, selected


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    args = parser.parse_args()
    try:
        summary, waits = analyze(read_trace(args.run_directory / "schedule.tsv"))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"schedule analysis: {error}\n")
    for filename, value in (("causal-summary.json", summary), ("waits.json", waits)):
        (args.run_directory / filename).write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
