#!/usr/bin/env python3
"""Attribute sampled host RIPs to JIT guest-instruction classes and host components.

Usage: python tools/performance/attribute_pc.py RUN_DIRECTORY [--long-ms 50]

Inputs in RUN_DIRECTORY (all local; none belong in the repository):
  pc-samples.tsv       perf_ns, unix_ns, rip, thread_cpu_100ns, suspend_ns per sample
                       of one runner thread (an external suspend/context sampler)
  sample-modules.json  {"modules": [{"base", "size", "path"}, ...]}, runner first
  jit-*.tsv, insn-*.tsv  written by the runner under ADV_JIT_MAP_DIR
  schedule.tsv         --schedule-profile; driver-clock.json + inputs.jsonl give
                       the reset-metrics control bracket used for alignment.
                       Optional: without it (a live, interactive run) every
                       sample lands in one "all" window with no run contexts.

Samples are wall-clock RIP observations of one thread, not CPU counters: waits
appear as system-module samples and suspension perturbs timing. JIT parts come
from IR source tags; Cranelift may move or merge instructions, so check/body/slow
splits are approximate, and untagged code between tags (egraph-rewritten pure
values) is charged to the next tagged instruction as part "pure". Each sample is also labelled with the inline guest run
active at that instant and the frame-gap window (cause from analyze_schedule).
"""

import argparse
import bisect
from collections import Counter, defaultdict
import csv
import json
from pathlib import Path
import re
import subprocess

from analyze_schedule import labelled_gaps, read_trace, trace_model

SYMBOLIZER = "llvm-symbolizer"
RUNTIME = ("std::", "core::", "alloc::", "hashbrown::", "<std::", "<core::", "<alloc::")


def read_samples(path):
    with path.open(encoding="utf-8", newline="") as stream:
        return [{key: int(value) for key, value in row.items()}
                for row in csv.DictReader(stream, delimiter="\t")]


class JitMap:
    """Timestamp-aware native body map: an address may be reused after retirement."""

    def __init__(self, directory):
        self.bodies = defaultdict(list)
        for path in sorted(directory.glob("jit-*.tsv")):
            for line in path.read_text(encoding="utf-8").splitlines():
                ns, base, size, guest, mode = line.split("\t")[:5]
                self.bodies[int(base)].append((int(ns), int(size), int(guest), mode))
        for rows in self.bodies.values():
            rows.sort()
        self.bases = sorted(self.bodies)
        self.ranges = defaultdict(list)
        for path in sorted(directory.glob("insn-*.tsv")):
            for line in path.read_text(encoding="utf-8").splitlines():
                ns, base, start, end, pc, part, kind = line.split("\t")
                self.ranges[(int(ns), int(base))].append((int(start), int(end), int(pc), part, kind))
        for rows in self.ranges.values():
            rows.sort()
        self.tagged = bool(self.ranges)

    def lookup(self, rip, unix_ns):
        index = bisect.bisect_right(self.bases, rip) - 1
        for base in self.bases[max(0, index - 3):index + 1][::-1]:
            # The newest compilation at this base before the sample owns it.
            live = [row for row in self.bodies[base] if row[0] <= unix_ns]
            if not live or rip >= base + live[-1][1]:
                continue
            ns, _, guest, mode = live[-1]
            offset = rip - base
            ranges = self.ranges.get((ns, base), [])
            at = bisect.bisect_right(ranges, (offset, float("inf"))) - 1
            found = {"region": guest, "mode": mode}
            if at >= 0 and ranges[at][0] <= offset < ranges[at][1]:
                _, _, pc, part, kind = ranges[at]
                return {**found, "pc": pc, "part": part, "class": kind}
            if not ranges or at + 1 == len(ranges):
                return {**found, "pc": 0, "part": "untagged", "class": "untagged"}
            if at < 0:
                return {**found, "pc": 0, "part": "prologue", "class": "overhead"}
            # Cranelift's egraph re-creates rewritten pure values without source
            # locations and places them before their first use: charge the next
            # tagged instruction, as a separate part.
            _, _, pc, _, kind = ranges[at + 1]
            return {**found, "pc": pc, "part": "pure", "class": kind}
        return None


def component(frames):
    """Crate::module of the innermost adv_* frame, else the innermost frame's root."""
    for name in frames:
        found = re.search(r"\b(adv_[a-z0-9_]+)(?:::([a-z_][a-z0-9_]*))?", name)
        if found:
            return found.group(1) + (f"::{found.group(2)}" if found.group(2) else "")
    if not frames or not frames[0]:
        return "unsymbolized"
    leaf = frames[0]
    if leaf.startswith(RUNTIME):
        return "rust-runtime"
    if "cranelift" in leaf or "regalloc" in leaf:
        return "cranelift"
    return re.split(r"::|<|\s", leaf.lstrip("<"), maxsplit=1)[0] or "host-other"


def symbolize(binary, rvas, symbolizer=SYMBOLIZER):
    """RVA -> inline frame names, innermost first. Needs the runner's matching PDB."""
    if not rvas:
        return {}
    command = [symbolizer, f"--obj={binary}", "--relative-address", "--inlines",
               "--output-style=JSON"]
    output = subprocess.run(command, input="".join(f"{hex(rva)}\n" for rva in rvas),
                            encoding="utf-8", stdout=subprocess.PIPE, check=True).stdout
    symbols = {}
    for line in output.splitlines():
        entry = json.loads(line)
        symbols[int(entry["Address"], 16)] = [frame.get("FunctionName", "")
                                              for frame in entry.get("Symbol", [])]
    return symbols


def exports(path):
    """Sorted (RVA, name) of a PE's named exports; [] if unreadable."""
    try:
        data = Path(path).read_bytes()
        pe = int.from_bytes(data[0x3C:0x40], "little")
        sections = int.from_bytes(data[pe + 6:pe + 8], "little")
        optional = int.from_bytes(data[pe + 20:pe + 22], "little")
        magic = int.from_bytes(data[pe + 24:pe + 26], "little")
        directory = pe + 24 + (112 if magic == 0x20B else 96)
        export_rva = int.from_bytes(data[directory:directory + 4], "little")
        table = pe + 24 + optional
        spans = [(int.from_bytes(data[at + 12:at + 16], "little"),
                  int.from_bytes(data[at + 8:at + 12], "little"),
                  int.from_bytes(data[at + 20:at + 24], "little"))
                 for at in range(table, table + 40 * sections, 40)]

        def offset(rva):
            for start, size, raw in spans:
                if start <= rva < start + size:
                    return rva - start + raw
            raise ValueError("RVA outside sections")

        word = lambda rva, width=4: int.from_bytes(data[offset(rva):offset(rva) + width], "little")
        if not export_rva:
            return []
        functions, names, ordinals = word(export_rva + 28), word(export_rva + 32), word(export_rva + 36)
        result = []
        for index in range(word(export_rva + 24)):
            name_at = offset(word(names + 4 * index))
            name = data[name_at:data.index(b"\0", name_at)].decode("ascii", "replace")
            result.append((word(functions + 4 * word(ordinals + 2 * index, 2)), name))
        return sorted(result)
    except (OSError, ValueError, IndexError):
        return []


def nearest_export(table, rva):
    """Preceding named export; internal functions also land here, hence the offset."""
    index = bisect.bisect_right(table, (rva, "\U0010ffff")) - 1
    if index < 0:
        return "?"
    start, name = table[index]
    return f"{name}+{rva - start:#x}"


def align(directory, rows):
    """Offset adding to perf_ns to give schedule time, from the reset-metrics bracket."""
    clock = json.loads((directory / "driver-clock.json").read_text(encoding="utf-8"))
    inputs = [json.loads(line) for line in
              (directory / "inputs.jsonl").read_text(encoding="utf-8").splitlines() if line]
    reset = next(entry for entry in inputs if entry["command"] == "reset-metrics")
    mark = next(row["time_ns"] for row in rows if row["event"] == "mark")
    reset_perf = clock["perf_ns"] + (reset["at"] + reset["ack"]) / 2 * 1e9
    return round(mark - reset_perf), (reset["ack"] - reset["at"]) * 1e3 / 2


def in_run_share(times, runs, shift):
    begins = [run[0] for run in runs]
    inside = 0
    for time in times:
        index = bisect.bisect_right(begins, time + shift) - 1
        inside += index >= 0 and time + shift < runs[index][1]
    return inside / len(times) if times else 0.0


def refine(times, runs, span_ns=3_000_000, step_ns=25_000):
    """Shift maximizing the share of JIT samples inside inline runs.

    Generated code executes only inside a run, so the true alignment puts nearly
    every JIT sample in one. Runs are often shorter than the control bracket's
    real error, so the bracket alone can misplace whole runs. Returns (shift,
    share at zero, share at shift); ties keep the shift closest to zero.
    """
    runs = sorted((begin, end) for begin, end, *_ in runs)
    candidates = range(-span_ns, span_ns + 1, step_ns)
    best = max(candidates, key=lambda shift: (in_run_share(times, runs, shift), -abs(shift)))
    return best, in_run_share(times, runs, 0), in_run_share(times, runs, best)


def bucket(label):
    if label["kind"] == "jit":
        return f"jit/{label['class']}/{label['part']}"
    return f"{label['kind']}/{label['name']}"


def host_function(frames):
    """Innermost frame, plus the innermost project frame when it is inlined further out."""
    if not frames:
        return ""
    project = next((name for name in frames if "adv_" in name), None)
    return frames[0] if project in (None, frames[0]) else f"{frames[0]} @ {project}"


def label_samples(samples, modules, jit, symbols, export_tables=None):
    runner = modules[0]
    export_tables = {} if export_tables is None else export_tables
    labels = []
    for sample in samples:
        rip = sample["rip"]
        if runner["base"] <= rip < runner["base"] + runner["size"]:
            frames = symbols.get(rip - runner["base"], [])
            labels.append({"kind": "host", "name": component(frames),
                           "function": host_function(frames)})
            continue
        found = jit.lookup(rip, sample["unix_ns"])
        if found:
            labels.append({"kind": "jit", **found})
            continue
        module = next((m for m in modules[1:] if m["base"] <= rip < m["base"] + m["size"]), None)
        if module is None:
            labels.append({"kind": "unknown", "name": "unknown", "function": ""})
            continue
        name = Path(module["path"]).name.lower()
        if module["path"] not in export_tables:
            export_tables[module["path"]] = exports(module["path"])
        labels.append({"kind": "module", "name": name, "function":
                       f"{name}!{nearest_export(export_tables[module['path']], rip - module['base'])}"})
    return labels


def attribute(samples, labels, model, gaps, long_ms):
    runs = sorted((begin, end, context) for begin, end, context, *_ in model["runs"])
    run_begins = [run[0] for run in runs]
    gap_begins = [gap["begin_ns"] for gap in gaps]

    def context_at(time):
        index = bisect.bisect_right(run_begins, time) - 1
        if index >= 0 and time < runs[index][1]:
            return f"ctx{runs[index][2]}"
        return "outside-run"

    def window_at(time):
        index = bisect.bisect_right(gap_begins, time) - 1
        if index >= 0 and time < gaps[index]["end_ns"]:
            gap = gaps[index]
            return gap["cause"] if gap["wall_ms"] >= long_ms else "short"
        return "unframed"

    windows = defaultdict(lambda: {"samples": 0, "buckets": Counter(), "contexts": Counter(),
                                   "guest": Counter(), "functions": Counter()})
    for sample, label in zip(samples, labels):
        time = sample["trace_ns"]
        if not model["lo"] <= time <= model["hi"]:
            continue
        for name in ("all", window_at(time)):
            window = windows[name]
            window["samples"] += 1
            window["buckets"][bucket(label)] += 1
            window["contexts"][context_at(time)] += 1
            if label["kind"] == "jit" and label["pc"]:
                window["guest"][(label["pc"], label["class"], label["part"])] += 1
            elif label["kind"] in ("host", "module"):
                window["functions"][label["function"] or "?"] += 1
    return windows


def summarize(window, top):
    total = max(window["samples"], 1)
    share = lambda count: round(100 * count / total, 2)
    parts, classes = Counter(), Counter()
    for name, count in window["buckets"].items():
        if name.startswith("jit/"):
            _, kind, part = name.split("/")
            parts[part] += count
            classes[kind] += count
    groups = Counter()
    for name, count in window["buckets"].items():
        groups[name.split("/")[0]] += count
    return {
        "samples": window["samples"],
        "groups_pct": {name: share(count) for name, count in groups.most_common()},
        "jit_parts_pct": {name: share(count) for name, count in parts.most_common()},
        "jit_classes_pct": {name: share(count) for name, count in classes.most_common()},
        "contexts_pct": {name: share(count) for name, count in window["contexts"].most_common()},
        "buckets_pct": {name: share(count) for name, count in window["buckets"].most_common(top)},
        "guest_instructions": [
            {"pc": hex(pc), "class": kind, "part": part, "pct": share(count)}
            for (pc, kind, part), count in window["guest"].most_common(top)
        ],
        "functions": [
            {"function": name, "pct": share(count)}
            for name, count in window["functions"].most_common(top)
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("--long-ms", type=float, default=50.0,
                        help="frame gaps at least this long are labelled by cause")
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument("--symbolizer", default=SYMBOLIZER)
    args = parser.parse_args()
    directory = args.run_directory
    live = not (directory / "schedule.tsv").exists()
    try:
        samples = read_samples(directory / "pc-samples.tsv")
        modules = json.loads((directory / "sample-modules.json").read_text(encoding="utf-8"))["modules"]
        if live:
            model = {"lo": float("-inf"), "hi": float("inf"), "runs": []}
            gaps, offset, uncertainty_ms = [], 0, None
        else:
            rows = read_trace(directory / "schedule.tsv")
            model = trace_model(rows)
            gaps = labelled_gaps(rows, model)
            offset, uncertainty_ms = align(directory, rows)
    except (OSError, ValueError, KeyError, StopIteration) as error:
        parser.exit(1, f"pc attribution: {error!r}\n")
    jit = JitMap(directory)
    runner = modules[0]
    rvas = sorted({s["rip"] - runner["base"] for s in samples
                   if runner["base"] <= s["rip"] < runner["base"] + runner["size"]})
    symbols = symbolize(runner["path"], rvas, args.symbolizer)
    labels = label_samples(samples, modules, jit, symbols)
    times = [sample["perf_ns"] + offset for sample, label in zip(samples, labels)
             if label["kind"] == "jit" and model["lo"] <= sample["perf_ns"] + offset < model["hi"]]
    shift, share_before, share_after = (0, 0.0, 0.0) if live else refine(times, model["runs"])
    for sample in samples:
        sample["trace_ns"] = sample["perf_ns"] + offset + shift
    windows = attribute(samples, labels, model, gaps, args.long_ms)
    summary = {
        "note": "wall-clock RIP samples of one thread; JIT parts approximate; suspension perturbs timing",
        "alignment_uncertainty_ms": uncertainty_ms,
        "alignment_refinement": {"shift_ms": shift / 1e6, "jit_in_run_before": round(share_before, 3),
                                 "jit_in_run_after": round(share_after, 3)},
        "instruction_map": jit.tagged,
        "long_ms": args.long_ms,
        "windows": {name: summarize(window, args.top)
                    for name, window in sorted(windows.items(), key=lambda kv: -kv[1]["samples"])},
    }
    (directory / "pc-attribution.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({name: {k: v for k, v in window.items()
                             if k in ("samples", "groups_pct", "jit_parts_pct", "jit_classes_pct")}
                      for name, window in summary["windows"].items()}, indent=2))


if __name__ == "__main__":
    main()
