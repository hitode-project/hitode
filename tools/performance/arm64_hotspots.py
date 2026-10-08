#!/usr/bin/env python3
"""Attribute simpleperf samples to anonymous JIT ranges and host CPU categories.

Usage: python tools/performance/arm64_hotspots.py --record RUN/perf.data
  --simpleperf-dir NDK/simpleperf --events RUN/perf-PID.events.tsv
  --output RUN/hotspots.json [--symfs RUN/symfs] [--markdown RUN/hotspots.md]

Input is a simpleperf record plus ADV_PERF_MAP_DIR's monotonic lifetime sidecar.
--perf-map accepts a static perf map instead, but rejects overlapping ranges
because it cannot resolve address reuse. JSON and Markdown contain statistics
and host function names only: no raw samples, addresses, process/thread IDs,
device identity, asset paths, guest symbols or code. CPU shares use event periods
and exclusive leaf PCs, not inclusive call-stack totals. Frame rate is not inferred.
"""
from __future__ import annotations

import argparse
from bisect import bisect_left, bisect_right, insort
from collections import Counter, defaultdict
from dataclasses import dataclass
import json
from pathlib import Path
import re
import sys

JIT_CATEGORIES = {
    "adv-jit-tier1": "Tier 1",
    "adv-jit-tier2": "Tier 2 Cranelift",
    "adv-jit-direct-memory": "Direct memory stubs",
}
CATEGORIES = (
    "Tier 0 interpreter", *JIT_CATEGORIES.values(), "JIT helper x87",
    "JIT helper SSE", "JIT helper memory", "JIT helper trap", "Compilation",
    "HLE GDI/blit", "HLE font", "HLE file", "HLE user32/messages", "HLE other",
    "Media/codecs", "Audio mixing", "Memory copy/set", "Kernel/syscall",
    "Runtime/scheduling", "Other host", "Anonymous/unresolved",
)


@dataclass(frozen=True)
class RangeEvent:
    time: int
    action: str
    base: int
    size: int
    category: str


def load_ranges(path: Path, *, static: bool = False) -> list[RangeEvent]:
    events = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split()
        if static:
            base, size, category = fields
            ns, action = 0, "load"
        else:
            time, action, base, size, category = fields
            ns = int(time)
        base, size = int(base, 16), int(size, 16)
        if (ns < 0 or base < 0 or size <= 0 or action not in ("load", "unload")
                or category not in JIT_CATEGORIES):
            raise ValueError("invalid anonymous range event")
        events.append(RangeEvent(ns, action, base, size, JIT_CATEGORIES[category]))
    events.sort(key=lambda event: event.time)
    if static:
        ordered = sorted(events, key=lambda event: event.base)
        if any(a.base + a.size > b.base for a, b in zip(ordered, ordered[1:])):
            raise ValueError("overlapping static perf map: use lifetime sidecar")
    return events


class Ranges:
    """Sweep monotonically ordered samples through exact executable lifetimes."""

    def __init__(self, events: list[RangeEvent]):
        self.events = events
        self.index = 0
        self.starts: list[int] = []
        self.active: dict[int, RangeEvent] = {}
        self.last_time = -1

    def lookup(self, ip: int, time: int) -> str | None:
        if time < self.last_time:
            raise ValueError("samples must be sorted by monotonic timestamp")
        self.last_time = time
        while self.index < len(self.events) and self.events[self.index].time <= time:
            event = self.events[self.index]
            self.index += 1
            old = self.active.get(event.base)
            if event.action == "unload":
                if old is None or (old.size, old.category) != (event.size, event.category):
                    raise ValueError("unmatched JIT retirement")
                del self.active[event.base]
                self.starts.pop(bisect_left(self.starts, event.base))
            else:
                pos = bisect_left(self.starts, event.base)
                if (old is not None
                        or pos and self.active[self.starts[pos - 1]].base
                        + self.active[self.starts[pos - 1]].size > event.base
                        or pos < len(self.starts) and event.base + event.size > self.starts[pos]):
                    raise ValueError("overlapping live JIT ranges")
                insort(self.starts, event.base)
                self.active[event.base] = event
        pos = bisect_right(self.starts, ip) - 1
        if pos >= 0:
            event = self.active[self.starts[pos]]
            if ip < event.base + event.size:
                return event.category
        return None


def host_symbol(name: str) -> str | None:
    """Keep recognizable host names; omit anonymous addresses and guest labels."""
    if (not name or re.fullmatch(r"(?:0x)?[0-9a-fA-F]+", name)
            or name.startswith(("[", "adv-jit-", "guest_", "region-"))
            or re.search(r"(?:guest|region)[_-][0-9a-fA-F]{6,}", name)):
        return None
    return re.sub(r"::h[0-9a-f]{16}$", "", name)


def classify(symbol: str, dso: str, jit: str | None) -> str:
    if jit:
        return jit
    name = symbol.lower()
    library = Path(dso).name.lower()
    if "kernel" in dso or name.startswith(("__arm64_sys_", "el0_")):
        return "Kernel/syscall"
    if re.search(r"(?:memcpy|memmove|memset|bzero|copy_nonoverlapping)", name):
        return "Memory copy/set"
    if ("cranelift_" in name or "regalloc2" in name
            or any(token in name for token in ("adv_cpu::compile::compile", "adv_cpu::tier1::emit",
                                               "adv_cpu::discovery", "iced_x86::decoder"))):
        return "Compilation"
    if any(token in name for token in ("adv_cpu::floating", "adv_cpu::compile::x87",
                                       "adv_cpu::tier1::x87", "softfloat", "extf80", "f128_")):
        return "JIT helper x87"
    if any(token in name for token in ("adv_cpu::compile::sse", "adv_cpu::tier1::sse",
                                       "adv_cpu::interpreter::sse")):
        return "JIT helper SSE"
    if any(token in name for token in ("adv_cpu::memory", "adv_cpu::direct_memory",
                                       "adv_cpu::tier1::native::load", "adv_cpu::tier1::native::store",
                                       "adv_cpu::tier1::simd", "adv_cpu::strings",
                                       "adv_cpu::atomic_memory", "adv_cpu::virtual_memory")):
        return "JIT helper memory"
    if "adv_cpu::trap" in name:
        return "JIT helper trap"
    if "adv_cpu::interpreter" in name:
        return "Tier 0 interpreter"
    if any(token in name for token in ("symphonia", "avcodec", "swscale", "ffmpeg",
                                       "adv_runtime::codec", "adv_device::media", "vorbis", "libav")):
        return "Media/codecs"
    if any(token in name for token in ("adv_device::audio", "adv_runtime::audio", "rubato",
                                       "adv_device::dsound", "adv_device::wave")):
        return "Audio mixing"
    if any(token in name for token in ("fontdue", "ttf_parser", "skrifa", "swash",
                                       "adv_device::text", "adv_win32::gdi::text", "font")):
        return "HLE font"
    if any(token in name for token in ("adv_device::gdi", "adv_device::surface",
                                       "adv_win32::gdi", "adv_device::blit", "adv_device::dib")):
        return "HLE GDI/blit"
    if any(token in name for token in ("adv_os::vfs", "adv_os::file", "adv_win32::file",
                                       "adv_win32::kernel32::file", "adv_device::file")):
        return "HLE file"
    if any(token in name for token in ("adv_win32::user32", "adv_device::window",
                                       "adv_runtime::input", "adv_device::message")):
        return "HLE user32/messages"
    if any(token in name for token in ("adv_win32::", "adv_device::", "adv_os::")):
        return "HLE other"
    if library == "libc.so" and re.search(r"(?:syscall|futex|ioctl|pread|pwrite|read|write|poll|wait|mmap|mprotect)", name):
        return "Kernel/syscall"
    if any(token in name for token in ("adv_cpu::executor", "adv_machine::", "adv_sync::",
                                       "adv_shell::", "adv_runtime::")):
        return "Runtime/scheduling"
    return "Other host" if host_symbol(symbol) else "Anonymous/unresolved"


def thread_role(comm: str, categories: Counter, *, is_main: bool) -> str:
    if "execution" in comm or is_main:
        return "Guest execution"
    # The shared background pool can compile, decode, or mix; classify its role
    # from exclusive leaf work instead of assuming every worker is a compiler.
    if any(token in comm.lower() for token in ("audio", "mixer", "cpal")):
        return "Audio"
    if "background" in comm or "compil" in comm:
        if categories["Compilation"] > sum(categories.values()) / 2:
            return "Compilation worker"
        if categories["Audio mixing"] + categories["Media/codecs"] > sum(categories.values()) / 2:
            return "Audio/media worker"
        return "Shared background worker"
    return "Other"


def summarize(samples: list[dict], events: list[RangeEvent]) -> dict:
    if not samples:
        raise ValueError("record has no samples")
    event_names = {sample["event"] for sample in samples}
    if len(event_names) != 1:
        raise ValueError("select exactly one sampling event; mixed periods are not comparable")
    ranges = Ranges(events)
    categories = Counter()
    symbols = Counter()
    threads = defaultdict(Counter)
    identities = {}
    matched = 0
    for sample in sorted(samples, key=lambda row: row["time"]):
        period = sample["period"]
        if period <= 0:
            raise ValueError("sample period must be positive")
        jit = ranges.lookup(sample["ip"], sample["time"])
        category = classify(sample["symbol"], sample["dso"], jit)
        categories[category] += period
        key = (sample["pid"], sample["tid"])
        threads[key][category] += period
        identities[key] = (sample["comm"], sample["pid"] == sample["tid"])
        if jit:
            matched += 1
        elif (name := host_symbol(sample["symbol"])):
            symbols[name] += period
    total = sum(categories.values())

    def rows(counts: Counter, *, complete: bool = False) -> list[dict]:
        names = CATEGORIES if complete else counts
        return [{"name": name, "period": counts[name], "percent": round(100 * counts[name] / total, 3)}
                for name in sorted(names, key=lambda name: (-counts[name], name))]

    roles = defaultdict(Counter)
    role_counts = Counter()
    for key, counts in threads.items():
        comm, main = identities[key]
        role = thread_role(comm, counts, is_main=main)
        roles[role].update(counts)
        role_counts[role] += 1
    return {
        "schema": "adv-arm64-hotspots-v1", "event": next(iter(event_names)),
        "samples": len(samples), "total_period": total,
        "jit_matched_samples": matched,
        "categories": rows(categories, complete=True),
        "threads": [{"role": role, "thread_count": role_counts[role],
                     "period": sum(counts.values()), "percent": round(100 * sum(counts.values()) / total, 3),
                     "categories": rows(counts)} for role, counts in sorted(roles.items())],
        "top_host_symbols": rows(Counter(dict(symbols.most_common(20)))),
        "notes": ["Exclusive sampled user CPU when event ends in :u; kernel time is unmeasured.",
                  "Thread category percentages use the whole-process denominator.",
                  "Shared workers are not assumed to be dedicated compilation threads.",
                  "Guest script-VM work remains anonymous JIT/interpreter; no guest attribution."],
    }


def read_record(record: Path, simpleperf_dir: Path, symfs: Path | None, pid: int | None) -> list[dict]:
    sys.path.insert(0, str(simpleperf_dir))
    from simpleperf_report_lib import ReportLib
    lib = ReportLib()
    try:
        lib.SetRecordFile(str(record))
        if symfs:
            lib.SetSymfs(str(symfs))
        samples = []
        while (sample := lib.GetNextSample()) is not None:
            if pid is not None and sample.pid != pid:
                continue
            symbol = lib.GetSymbolOfCurrentSample()
            event = lib.GetEventOfCurrentSample()
            samples.append({"ip": sample.ip, "time": sample.time, "period": sample.period,
                            "pid": sample.pid, "tid": sample.tid, "comm": sample.thread_comm,
                            "event": event.name, "symbol": symbol.symbol_name, "dso": symbol.dso_name})
        return samples
    finally:
        lib.Close()


def markdown(report: dict) -> str:
    lines = [f"Samples: {report['samples']}; event: {report['event']}", "",
             "| Category | CPU share |", "|---|---:|"]
    lines += [f"| {row['name']} | {row['percent']:.3f}% |" for row in report["categories"]]
    lines += ["", "| Thread role | Threads | CPU share |", "|---|---:|---:|"]
    lines += [f"| {row['role']} | {row['thread_count']} | {row['percent']:.3f}% |" for row in report["threads"]]
    lines += ["", "| Host function | CPU share |", "|---|---:|"]
    lines += [f"| `{row['name'].replace('|', '/').replace('`', '')}` | {row['percent']:.3f}% |"
              for row in report["top_host_symbols"]]
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", required=True, type=Path)
    parser.add_argument("--simpleperf-dir", required=True, type=Path)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--events", type=Path)
    source.add_argument("--perf-map", type=Path)
    parser.add_argument("--symfs", type=Path)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--markdown", type=Path)
    args = parser.parse_args()
    events = load_ranges(args.events or args.perf_map, static=args.perf_map is not None)
    report = summarize(read_record(args.record, args.simpleperf_dir, args.symfs, args.pid), events)
    report["range_source"] = "static map" if args.perf_map else "monotonic lifetimes"
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if args.markdown:
        args.markdown.write_text(markdown(report), encoding="utf-8")
    print(f"samples={report['samples']} jit_matched={report['jit_matched_samples']}")


if __name__ == "__main__":
    main()
