"""Estimate scenario work from existing generated-code maps and entry counts."""
from __future__ import annotations

from collections import Counter, defaultdict
from functools import cache
import re
import subprocess
from pathlib import Path


def resident_trips(calls: int, trips: int, arrivals: int, tier1_hits: int | None) -> int:
    """Return loop iterations executed inside a resident form.

    A resident entry reached from a taken backedge executes `trips - 1`
    iterations after one Tier-1 pass, while a direct entry from outside
    executes all `trips` iterations with no Tier-1 pass. When Tier-1
    arrival counts are available, subtracting full-Tier-1 calls separates
    backedge entries from direct entries.
    """
    if arrivals <= 0 or trips <= 0:
        return 0
    if tier1_hits is None:
        return arrivals * (trips - 1)
    backedge = min(arrivals, max(0, tier1_hits - (calls - arrivals) * trips))
    direct = arrivals - backedge
    return direct * trips + backedge * (trips - 1)


def symbols(exe: Path, compiler: str) -> dict[str, int]:
    output = subprocess.check_output([compiler.removesuffix("gcc") + "nm", "-n", str(exe)], text=True)
    return {fields[2]: int(fields[0], 16) for line in output.splitlines()
            if len(fields := line.split()) == 3}


SITE = re.compile(r" (access|checked_fault|helper_exit|resident_cold_exit|interpreter|value) "
                  r"(0x[0-9a-f]+)(?: (\w+(?:\(\d+\))?))?=(\d+)")
METRICS = {
    "access": "accesses", "checked_fault": "checked_faults",
    "helper_exit": "helper_exits", "resident_cold_exit": "resident_cold_exits",
    "value": "value_calls",
}
INTERPRETER_METRICS = {"Fetch": "interprets", "HostFault": "faults", "OperandFault": "faults"}


def site_counts(diagnostics: str, limits: list[tuple[int, int]]) -> Counter:
    counts = Counter()
    for event, pc, cause, count in SITE.findall(diagnostics):
        if any(lo <= int(pc, 16) < hi for lo, hi in limits):
            metric = (METRICS[event] if event != "interpreter"
                      else INTERPRETER_METRICS.get(cause, "interpreted_exits"))
            counts[metric] += int(count)
    return counts


def split_choice(iterations: int, retired: int, ns: float, wake: int, work: int,
                 last: int, saved: int, limit: int, idle: int) -> tuple[int, int]:
    """Replay the block policy from its price, frame pace and available helpers."""
    serial = iterations * ns
    need = min(int(serial), max(0, max(last, work + int(serial)) - limit - saved))
    worth = max(0, int((serial - wake) / (4 * wake) - 1))
    most = min(idle, worth)
    if not need or not most:
        return 0, 0
    slice_iterations = max(1, 65536 // retired)
    helpers = most
    for count in range(1, most + 1):
        part = (serial - wake) / (count + 1)
        if wake + part + min(part / 4, slice_iterations * ns) + 10000 <= serial - need:
            helpers = count
            break
    blocks = 4 * (helpers + 1)
    return helpers, min(slice_iterations, (iterations + blocks - 1) // blocks)


def measure(mapping: Path, exe: Path, compiler: str, steady: dict) -> tuple[Counter, list[dict]]:
    rows = [line.split("\t", 3) for file in mapping.glob("opt-*.tsv")
            for line in file.read_text().splitlines()]
    marks = {int(row[3].split("\t", 1)[0]): row[3].split("\t", 1)[1]
             for row in rows if len(row) == 4 and row[2] == "mark"}
    layout = steady.get("kernel_work")
    expected = 2 * len(layout["kernels"]) if layout else 2
    if marks.keys() != set(range(expected)):
        raise RuntimeError(f"expected {expected} measurement marks, received {sorted(marks)}")
    names = symbols(exe, compiler)
    def limits(regions):
        return [(names[name], names[name + "_end"]) for name in regions]

    low, high = steady["iterations"]
    totals = Counter()
    if layout:
        totals.update(estimate(mapping, exe, compiler, layout, names, (low, high), marks))
        for index, kernel in enumerate(layout["kernels"]):
            regions = [kernel[0], *(name for caller, name, _ in layout.get("callees", [])
                                    if caller == kernel[0])]
            totals.update(site_counts(marks[2 * index + 1], limits(regions)))
            totals.subtract(site_counts(marks[2 * index], limits(regions)))
    else:
        regions = limits(steady["regions"])
        totals.update(site_counts(marks[1], regions))
        totals.subtract(site_counts(marks[0], regions))
    decisions = []
    if name := steady.get("split_policy"):
        lo, hi = limits([name])[0]
        for row in rows:
            if len(row) != 4 or row[2] != "split_price" or not lo <= int(row[1]) < hi:
                continue
            trips, retired, ns, wake, work, last, saved, limit, idle, helpers, size = row[3].split("\t")
            inputs = (int(trips), int(retired), float(ns), int(wake), int(work),
                      int(last), int(saved), int(limit), int(idle))
            actual = (int(helpers), int(size))
            decisions.append({
                "inputs": dict(zip(("iterations", "retired_per_iteration", "ns_per_iteration",
                                    "wake_ns", "work_ns", "last_ns", "saved_ns", "limit_ns",
                                    "idle_helpers"), inputs)),
                "choice": actual,
                "expected": split_choice(*inputs),
            })
        if not decisions:
            raise RuntimeError(f"no priced split decisions for {name}")
    return totals, decisions


def estimate(mapping: Path, exe: Path, compiler: str, layout: dict,
             names: dict[str, int], iterations: tuple[int, int], marks: dict[int, str]) -> Counter:
    """Price straight-line kernel phases with deterministic trip counts.

    Resident entries at each loop head are priced from direct entries
    when Tier-1 arrival counts show the first pass bypassed Tier-1, or
    from the first taken backedge otherwise. Words describe inline
    emission. The runner's existing counters account for work outside it.
    """
    maps = list(mapping.glob("insn-*.tsv"))
    programs = {}
    for path in maps:
        for line in path.with_name(path.name.replace("insn-", "jit-")).read_text().splitlines():
            gen, base, _, head, mode, *_ = line.split("\t")
            programs[gen, base] = int(head), mode, len(programs)

    resident_cost = {}
    for path in mapping.glob("opt-*.tsv"):
        for line in path.read_text().splitlines():
            fields = line.split("\t")
            if len(fields) == 5 and fields[2] == "work" and int(fields[3]):
                resident_cost[int(fields[1])] = (int(fields[3]), int(fields[4]))

    prefix = compiler.removesuffix("gcc")
    limits = [(names[name], names[name + "_end"]) for name, _ in layout["kernels"]]
    for _, name, _ in layout.get("callees", []):
        limits.append((names[name], names[name + "_end"]))

    stages = defaultdict(lambda: defaultdict(Counter))
    cold = set()
    previous = None
    repeated = set()
    for path in maps:
        for line in path.read_text().splitlines():
            gen, base, start, end, pc, part, mode = line.split("\t")
            unit = gen, base
            if part == "Cold":
                cold.add(unit)
            if mode not in ("a64", "a64-resident") or unit in cold or part == "Rep":
                continue
            pc = int(pc)
            if not any(lo <= pc < hi for lo, hi in limits):
                previous = None
                continue
            if previous != (unit, pc) and pc in stages[unit]:
                repeated.add((unit, pc))
            previous = unit, pc
            if (unit, pc) not in repeated and int(end) > int(start):
                stages[unit][pc][part] += (int(end) - int(start)) // 4
    ops = subprocess.check_output([prefix + "objdump", "-d", str(exe)], text=True)
    control = {int(pc, 16) for pc in re.findall(r"^\s*([0-9a-f]+):.*\bfldcw\b", ops, re.M)}

    @cache
    def price(lo: int, hi: int, mode: str, head: int | None = None) -> Counter:
        forms = [((unit, pc), parts) for unit, pcs in stages.items()
                 if programs[unit][1] == mode and (head is None or programs[unit][0] == head)
                 for pc, parts in pcs.items() if lo <= pc < hi]
        coverage = Counter(unit for (unit, _), _ in forms)
        seen = set()
        cost = Counter()
        for (unit, pc), parts in sorted(forms, key=lambda item: (
                coverage[item[0][0]], programs[item[0][0]][2]), reverse=True):
            if pc in seen:
                continue
            seen.add(pc)
            cost["kernel_host_words"] += parts.total()
            cost["kernel_x87_entries"] += int("X87Entry" in parts)
            cost["kernel_x87_publications"] += int("X87Publish" in parts)
            cost["kernel_control_updates"] += int(mode == "a64" and pc in control)
        return cost

    def count(iterations: int, endpoint: int) -> Counter:
        calls = iterations * layout["calls"]
        rows = calls * layout["rows"]
        totals = Counter()
        for index, (name, columns) in enumerate(layout["kernels"]):
            diagnostics = marks[2 * index + endpoint]
            entries = {int(pc, 16): int(count) for pc, count in re.findall(
                r"span (0x[0-9a-f]+)=\[entries (\d+)", diagnostics)}
            tier1 = {
                int(pc, 16): sum(int(n) for n in re.findall(r"\d+", counts))
                for pc, counts in re.findall(
                    r"(0x[0-9a-f]+)=\[builds \d+, misses \d+, joins \d+, versions \d+, arrivals \[([^\]]*)\]",
                    diagnostics,
                )
            }
            row, col, col_end, row_end, end = (names[name + suffix] for suffix in
                                              ("_row", "_col", "_col_end", "_row_end", "_end"))
            row_arrivals = min(calls, entries.get(row, 0))
            row_tier1 = max(
                (v for pc, v in tier1.items() if row <= pc < col or col_end <= pc < row_end),
                default=None,
            )
            resident_rows = resident_trips(calls, layout["rows"], row_arrivals, row_tier1)
            tier1_rows = rows - resident_rows
            col_arrivals = min(tier1_rows, entries.get(col, 0))
            col_tier1 = max(
                (v for pc, v in tier1.items() if col <= pc < col_end),
                default=None,
            )
            resident_columns = resident_trips(tier1_rows, columns, col_arrivals, col_tier1)
            resident_cells = resident_rows * columns + resident_columns
            tier1_cells = rows * columns - resident_cells
            totals["kernel_resident_cells"] += resident_cells
            totals["kernel_tier1_cells"] += tier1_cells
            phases = [(names[name], row, calls), (row, col, tier1_rows),
                      (col, col_end, tier1_cells), (col_end, row_end, tier1_rows),
                      (row_end, end, calls)]
            for lo, hi, visits in phases:
                for counter, value in price(lo, hi, "a64").items():
                    totals[counter] += value * visits

            column_words = price(col, col_end, "a64-resident", col)["kernel_host_words"]
            nested_words = price(col, col_end, "a64-resident", row)["kernel_host_words"]
            for caller, callee, repeats in layout.get("callees", []):
                if caller != name:
                    continue
                lo, hi = names[callee], names[callee + "_end"]
                for counter, value in price(lo, hi, "a64").items():
                    totals[counter] += value * tier1_cells * repeats
                column_words += price(lo, hi, "a64-resident", col)["kernel_host_words"] * repeats
                nested_words += price(lo, hi, "a64-resident", row)["kernel_host_words"] * repeats

            loop_words, boundary = resident_cost.get(col, (column_words, 0))
            totals["kernel_host_words"] += loop_words * resident_columns + boundary * col_arrivals
            # The resident price includes one pass through its nested
            # loop. Price the remaining columns with that form's emission.
            row_words = (price(row, col, "a64-resident", row)["kernel_host_words"] +
                         nested_words +
                         price(col_end, row_end, "a64-resident", row)["kernel_host_words"])
            loop_words, boundary = resident_cost.get(row, (row_words, 0))
            totals["kernel_host_words"] += ((loop_words + nested_words * (columns - 1)) *
                                            resident_rows + boundary * row_arrivals)
            for counter in ("kernel_x87_entries", "kernel_x87_publications", "kernel_control_updates"):
                totals[counter] += row_arrivals + col_arrivals
        return totals

    low, high = iterations
    totals = count(high, 1)
    totals.subtract(count(low, 0))
    return totals
