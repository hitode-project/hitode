#!/usr/bin/env python3
"""Aggregate execution-cliff and tier-verification counters from run reports.

Usage: python tools/survey/cliffs.py report1.json report2.json [--json]
Input: one final adv-run --report JSON per independent run, in argument order.
Both cpu_cliffs and tier_verification must have the current counter fields.
Keep normal and ADV_JIT_VERIFY=1 runs in separate invocations; cumulative
checkpoints from the same run must not be added together.
Output: Markdown tables (native fallback Top 20) or complete aggregate JSON on
stdout. Only counters, mnemonic/reason labels and ordinal report numbers leave
the tool; filenames, private metadata, run timings and verification reports are excluded.
Errors identify an input by ordinal, never by path or by its contents.

adv-run currently keeps at most 24 fallback and 24 x87 mnemonic/reason rows per
report. Rankings therefore cover retained rows only. Missing x87 exact calls
are counted as unattributed; omitted fallback counts cannot be reconstructed.
Interpreter details keep the top 24 rows by instruction units, with explicit
omitted counters. Older reports remain readable but their details are unknown.
These are execution counts, not time measurements. ADV_TIER1_CENSUS=1 adds
shared-form Tier 1 coverage for Uncovered/Rejected work, split into three
disjoint groups. Prefix histograms use decoded instructions; supported counts
use retirement units (REP elements). All-supported prefixes are right-censored.
Optional x87_control fields retain the complete PC/RC/mask distribution and
independent guard counts. Reports predating those fields are counted separately.
Optional tier1_bridge counts actual native retirements separately from residual
interpreter work; missing fields in old reports remain unmeasured.
Optional compilation_schedule contains bounded aggregate duration/size histograms,
waiting interpreter units and concurrency counters; old reports remain unmeasured.
Optional x87_pc64 records all nearest/masked PC=64 Control rejections by
opportunity class, including rejected operands and operations not evaluated.
"""
from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys
from typing import Iterable


REPORT_ROW_LIMIT = 24
X87_REASONS = ("Control", "Pending", "Operand", "NoFastForm")
INTERPRETER_REASONS = ("Uncovered", "Rejected", "Pending", "Interior", "Fallback", "Forced")
VERIFY_COUNTERS = ("verified", "inconclusive", "mismatches", "mxcsr_flags")
DETAIL_REASONS = {
    "AutoJitDisabled", "Tier1", "NonExecutable", "BelowThreshold",
    "PendingCompilation", "DeferredCompilation", "QueueCapacity", "RegionBoundary",
    "UnregisteredEntry", "NotDiscovered", "DiscoveryBoundary", "DiscoveryMemory",
    "DiscoverySize", "WorkBudget", "CompileFailure", "CachePolicy", "NoPublishedEntry",
}
DETAIL_BUCKETS = ("Uncovered", "Rejected")
COUNT_KEYS = ("entries", "instructions")


def detail_label(label: object) -> str:
    parts = label.split("/") if isinstance(label, str) else []
    if len(parts) < 2 or parts[0] not in DETAIL_BUCKETS:
        raise ValueError("invalid interpreter detail label")
    if len(parts) == 2 and parts[1] in DETAIL_REASONS:
        return label
    if (len(parts) == 4 and parts[1] == "Unsupported"
            and mnemonic(parts[2]) == parts[2]
            and re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,95}", parts[3])
            and not re.search(r"(?:^|_)(?:0[xX][0-9A-Fa-f]+|[0-9A-Fa-f]{8,})(?:_|$)", parts[3])
            and parts[3].split("_")[0].lower().startswith(parts[2].lower())):
        return label
    raise ValueError("invalid interpreter detail label")


def entry_count(value: object) -> dict:
    value = mapping(value)
    return {key: count(required(value, key)) for key in COUNT_KEYS}


def ranked_entries(values: dict) -> dict:
    return dict(sorted(values.items(), key=lambda row: (-row[1]["instructions"], row[0])))


def mapping(value: object) -> dict:
    if not isinstance(value, dict):
        raise ValueError("expected an object")
    return value


def count(value: object) -> int:
    if type(value) is not int or value < 0:
        raise ValueError("expected a nonnegative integer counter")
    return value


def required(value: dict, key: str) -> object:
    if key not in value:
        raise ValueError("missing a required counter field")
    return value[key]


def mnemonic(value: object) -> str:
    # Match the producer's iced-x86 debug labels, not operands or addresses.
    if (not isinstance(value, str)
            or not re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,31}", value)
            or re.fullmatch(r"[A-Fa-f0-9]{8,}", value)):
        raise ValueError("invalid mnemonic label")
    return value


def ranked(values: dict) -> dict:
    return dict(sorted(values.items(), key=lambda row: (-row[1], row[0])))


CENSUS_GROUPS = ("PendingCompilation", "Uncovered", "Rejected")
CENSUS_COUNTERS = ("entries", "instructions", "supported_units", "unsupported_units",
                   "prefix_units", "omitted_rows", "omitted_units")
CENSUS_PREFIXES = ("blocked_prefixes", "censored_prefixes")


def empty_census() -> dict:
    return {name: dict(dict.fromkeys(CENSUS_COUNTERS, 0), unsupported={},
                       blocked_prefixes=[0] * 8, censored_prefixes=[0] * 8)
            for name in CENSUS_GROUPS}


def merge_census(destination: dict, source: object, interpreter: dict) -> None:
    """Validate bounded anonymous form counters and reconcile observed work."""
    source = mapping(source)
    if set(source) != set(CENSUS_GROUPS):
        raise ValueError("invalid Tier 1 census groups")
    entries = units = 0
    for name in CENSUS_GROUPS:
        row = mapping(source[name])
        values = {key: count(required(row, key)) for key in CENSUS_COUNTERS}
        histograms = {}
        for key in CENSUS_PREFIXES:
            bins = required(row, key)
            if not isinstance(bins, list) or len(bins) != 8:
                raise ValueError("invalid Tier 1 prefix histogram")
            histograms[key] = [count(n) for n in bins]
        if sum(map(sum, histograms.values())) != values["entries"]:
            raise ValueError("Tier 1 prefix entries do not reconcile")
        forms = mapping(required(row, "unsupported"))
        if len(forms) > REPORT_ROW_LIMIT or (values["omitted_rows"] and len(forms) != REPORT_ROW_LIMIT):
            raise ValueError("inconsistent Tier 1 form row limit")
        retained = {}
        for label, n in forms.items():
            if (not isinstance(label, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,95}", label)
                    or re.search(r"(?:^|_)(?:0[xX][0-9A-Fa-f]+|[0-9A-Fa-f]{8,})(?:_|$)", label)):
                raise ValueError("invalid Tier 1 form label")
            retained[label] = count(n)
        if ((not values["omitted_rows"] and values["omitted_units"])
                or sum(retained.values()) + values["omitted_units"] != values["unsupported_units"]):
            raise ValueError("Tier 1 unsupported units do not reconcile")
        total = values["supported_units"] + values["unsupported_units"]
        if values["instructions"] > total or values["prefix_units"] > values["supported_units"]:
            raise ValueError("inconsistent Tier 1 instruction units")
        entries += values["entries"]
        units += total
        dest = destination[name]
        for key, n in values.items():
            dest[key] += n
        for key, bins in histograms.items():
            dest[key] = [a + b for a, b in zip(dest[key], bins)]
        for label, n in retained.items():
            dest["unsupported"][label] = dest["unsupported"].get(label, 0) + n
        dest["unsupported"] = ranked(dest["unsupported"])
    expected = [entry_count(interpreter.get(name, dict.fromkeys(COUNT_KEYS, 0))) for name in DETAIL_BUCKETS]
    if entries != sum(row["entries"] for row in expected) or units != sum(row["instructions"] for row in expected):
        raise ValueError("Tier 1 census does not reconcile with interpreter buckets")
SCHEDULE_HISTOGRAMS = ("deferred_us", "queue_us", "service_us", "publication_us", "age_us",
                       "instructions", "decoded_bytes", "window_bytes", "native_bytes", "waiting_units")
SCHEDULE_COUNTERS = ("duplicate_requests", "capacity_rejections", "window_deferrals", "worker_full")
SCHEDULE_GLOBALS = ("in_flight_at_submit", "active_at_start", "unfinished_age_us", "unfinished_waiting_units")


def merge_histogram(destination: dict, raw: object) -> None:
    """Validate and merge a fixed log2 histogram; never retain unknown fields."""
    raw = mapping(raw)
    values = {key: count(required(raw, key)) for key in ("count", "sum", "max")}
    bins = required(raw, "log2_bins")
    if not isinstance(bins, list) or len(bins) != 65:
        raise ValueError("invalid compilation histogram bins")
    bins = [count(n) for n in bins]
    if sum(bins) != values["count"] or values["max"] > values["sum"]:
        raise ValueError("inconsistent compilation histogram totals")
    occupied = [i for i, n in enumerate(bins) if n]
    if occupied and occupied[-1] != values["max"].bit_length():
        raise ValueError("inconsistent compilation histogram maximum")
    if not occupied and (values["sum"] or values["max"]):
        raise ValueError("nonempty compilation histogram without samples")
    destination.setdefault("log2_bins", [0] * 65)
    for key in ("count", "sum"):
        destination[key] = destination.get(key, 0) + values[key]
    destination["max"] = max(destination.get("max", 0), values["max"])
    destination["log2_bins"] = [a + b for a, b in zip(destination["log2_bins"], bins)]


def merge_schedule(destination: dict, raw: object) -> None:
    """Consume only anonymous scheduling counters from one final report."""
    raw = mapping(raw)
    if count(required(raw, "version")) != 1:
        raise ValueError("unsupported compilation schedule version")
    configuration = tuple(count(required(raw, key)) for key in ("compile_threads", "available_cores"))
    configurations = destination.setdefault("configurations", [])
    row = next((c for c in configurations if (c["compile_threads"], c["available_cores"]) == configuration), None)
    if row is None:
        row = dict(zip(("compile_threads", "available_cores"), configuration), reports=0)
        configurations.append(row)
    row["reports"] += 1
    destination["report_count"] += 1
    for key in SCHEDULE_COUNTERS:
        destination[key] = destination.get(key, 0) + count(required(raw, key))
    for key in SCHEDULE_GLOBALS:
        merge_histogram(destination.setdefault(key, {}), required(raw, key))
    for strategy in ("fast", "speed"):
        source = mapping(required(raw, strategy))
        target = destination.setdefault(strategy, {})
        for key in SCHEDULE_HISTOGRAMS:
            merge_histogram(target.setdefault(key, {}), required(source, key))
        units = required(source, "interpreter_units_by_stage")
        if not isinstance(units, list) or len(units) != 4:
            raise ValueError("invalid compilation interpreter stages")
        target["interpreter_units_by_stage"] = [a + count(b) for a, b in zip(
            target.get("interpreter_units_by_stage", [0] * 4), units)]
        if any(source[key]["count"] != source["service_us"]["count"]
               for key in ("queue_us", "publication_us")):
            raise ValueError("inconsistent compilation completion counts")


def aggregate(reports: Iterable[dict]) -> dict:
    """Sum retained counters; reject incomplete or inconsistent input reports."""
    fallbacks = Counter()
    exact = Counter()
    by_mnemonic = Counter()
    by_reason = Counter(dict.fromkeys(X87_REASONS, 0))
    controls = Counter()
    control_guards = Counter(dict.fromkeys(("PC", "RC", "Masks"), 0))
    control_reports = 0
    pc64 = Counter()
    pc64_reports = 0
    interpreter = {reason: {"entries": 0, "instructions": 0} for reason in INTERPRETER_REASONS}
    details = {}
    omitted = {reason: dict.fromkeys(COUNT_KEYS, 0) for reason in DETAIL_BUCKETS}
    unknown = {reason: dict.fromkeys(COUNT_KEYS, 0) for reason in DETAIL_BUCKETS}
    omitted_rows = detail_reports = 0
    census = empty_census()
    census_reports = 0
    bridge_reports = 0
    bridge_totals = dict.fromkeys(COUNT_KEYS, 0)
    bridge_details = {}
    verification = dict.fromkeys(VERIFY_COUNTERS, 0)
    per_report = []
    schedule = {"report_count": 0, "unmeasured_reports": 0}
    helpers = fast = unattributed = fallback_limits = exact_limits = 0
    for index, report in enumerate(reports, 1):
        try:
            report = mapping(report)
            if "compilation_schedule" in report:
                merge_schedule(schedule, report["compilation_schedule"])
            else:
                schedule["unmeasured_reports"] += 1
            cliffs = mapping(required(report, "cpu_cliffs"))
            verify = mapping(required(report, "tier_verification"))
            run_helpers = count(required(cliffs, "x87_helper_calls"))
            run_fast = count(required(cliffs, "x87_fast"))
            run_fallbacks = mapping(required(cliffs, "fallbacks"))
            run_exact = mapping(required(cliffs, "x87_exact"))
            run_interpreter = mapping(required(cliffs, "interpreter"))
            retained_exact = 0
            retained_control = 0
            for name, value in run_fallbacks.items():
                fallbacks[mnemonic(name)] += count(value)
            for label, value in run_exact.items():
                parts = label.split("/") if isinstance(label, str) else []
                if len(parts) != 2 or parts[1] not in X87_REASONS:
                    raise ValueError("invalid x87 reason label")
                name, reason = mnemonic(parts[0]), parts[1]
                value = count(value)
                exact[f"{name}/{reason}"] += value
                by_mnemonic[name] += value
                by_reason[reason] += value
                retained_exact += value
                if reason == "Control":
                    retained_control += value
            if run_fast + retained_exact > run_helpers:
                raise ValueError("x87 counters exceed helper calls")
            if "x87_control" in cliffs or "x87_control_rejections" in cliffs:
                distribution = mapping(required(cliffs, "x87_control"))
                guards = mapping(required(cliffs, "x87_control_rejections"))
                expected = dict.fromkeys(("PC", "RC", "Masks"), 0)
                total = 0
                for label, value in distribution.items():
                    if not isinstance(label, str):
                        raise ValueError("invalid x87 control label")
                    match = re.fullmatch(r"PC=(24|53|64|reserved)/RC=(nearest|down|up|zero)/masks=([0-3][0-9a-f])", label)
                    if match is None:
                        raise ValueError("invalid x87 control label")
                    pc, rc, masks = match.groups()
                    value = count(value)
                    expected["PC"] += value * (pc not in ("24", "53"))
                    expected["RC"] += value * (rc != "nearest")
                    expected["Masks"] += value * (masks != "3f")
                    total += value
                    controls[label] += value
                if set(guards) != set(expected) or any(count(guards[k]) != n for k, n in expected.items()):
                    raise ValueError("inconsistent x87 control guards")
                if not retained_control <= total <= run_helpers - run_fast:
                    raise ValueError("x87 control distribution exceeds exact counts")
                control_guards.update(expected)
                control_reports += 1
            if "x87_pc64" in cliffs:
                opportunities = mapping(cliffs["x87_pc64"])
                observed = Counter()
                for label, value in opportunities.items():
                    parts = label.split("/") if isinstance(label, str) else []
                    if len(parts) != 2 or parts[1] not in ("Other", "Operand", "Range", "Inexact", "Exact", "Zero"):
                        raise ValueError("invalid x87 PC64 category")
                    mnemonic(parts[0])
                    observed[label] = count(value)
                distribution = mapping(required(cliffs, "x87_control"))
                if sum(observed.values()) != count(distribution.get("PC=64/RC=nearest/masks=3f", 0)):
                    raise ValueError("inconsistent x87 PC64 coverage")
                pc64.update(observed)
                pc64_reports += 1
            for reason, values in run_interpreter.items():
                if reason not in INTERPRETER_REASONS:
                    raise ValueError("invalid interpreter reason label")
                values = mapping(values)
                for key in ("entries", "instructions"):
                    interpreter[reason][key] += count(required(values, key))
            detail_fields = ("interpreter_details", "interpreter_details_omitted",
                             "interpreter_detail_rows_omitted")
            if any(key in cliffs for key in detail_fields):
                run_details = mapping(required(cliffs, detail_fields[0]))
                run_omitted = mapping(required(cliffs, detail_fields[1]))
                missing_rows = count(required(cliffs, detail_fields[2]))
                if len(run_details) > REPORT_ROW_LIMIT or (missing_rows and len(run_details) != REPORT_ROW_LIMIT):
                    raise ValueError("inconsistent interpreter detail row limit")
                if set(run_omitted) != set(DETAIL_BUCKETS):
                    raise ValueError("invalid omitted interpreter buckets")
                totals = {reason: entry_count(run_omitted[reason]) for reason in DETAIL_BUCKETS}
                if not missing_rows and any(sum(row.values()) for row in totals.values()):
                    raise ValueError("omitted counters without omitted rows")
                for reason in DETAIL_BUCKETS:
                    for key in COUNT_KEYS:
                        omitted[reason][key] += totals[reason][key]
                for label, values in run_details.items():
                    label = detail_label(label)
                    values = entry_count(values)
                    destination = details.setdefault(label, dict.fromkeys(COUNT_KEYS, 0))
                    for key in COUNT_KEYS:
                        destination[key] += values[key]
                        totals[label.split("/")[0]][key] += values[key]
                for reason in DETAIL_BUCKETS:
                    if totals[reason] != entry_count(run_interpreter.get(reason, dict.fromkeys(COUNT_KEYS, 0))):
                        raise ValueError("interpreter details do not reconcile with bucket totals")
                omitted_rows += missing_rows
                detail_reports += 1
            else:
                for reason in DETAIL_BUCKETS:
                    for key, value in entry_count(run_interpreter.get(reason, dict.fromkeys(COUNT_KEYS, 0))).items():
                        unknown[reason][key] += value
            if "tier1_bridge" in cliffs:
                raw_bridge = mapping(cliffs["tier1_bridge"])
                run_bridge = entry_count(raw_bridge)
                bridge_sum = dict.fromkeys(COUNT_KEYS, 0)
                for label, raw in mapping(required(raw_bridge, "details")).items():
                    if label not in ("Pending", "Interior"):
                        detail_label(label)
                    values = entry_count(raw)
                    dest = bridge_details.setdefault(label, dict.fromkeys(COUNT_KEYS, 0))
                    for key in COUNT_KEYS:
                        bridge_sum[key] += values[key]
                        dest[key] += values[key]
                if bridge_sum != run_bridge:
                    raise ValueError("Tier 1 bridge details do not reconcile")
                for key in COUNT_KEYS:
                    bridge_totals[key] += run_bridge[key]
                bridge_reports += 1
            if cliffs.get("tier1_census") is not None:
                merge_census(census, cliffs["tier1_census"], run_interpreter)
                census_reports += 1
            run_verification = {key: count(required(verify, key)) for key in VERIFY_COUNTERS}
            for key, value in run_verification.items():
                verification[key] += value
            per_report.append({"report": index, "tier_verification": run_verification})
            helpers += run_helpers
            fast += run_fast
            unattributed += run_helpers - run_fast - retained_exact
            fallback_limits += len(run_fallbacks) >= REPORT_ROW_LIMIT
            exact_limits += len(run_exact) >= REPORT_ROW_LIMIT
        except ValueError as error:
            raise ValueError(f"input {index}: {error}") from None
    if not per_report:
        raise ValueError("at least one report is required")
    return {
        "report_count": len(per_report),
        "compilation_schedule": schedule,
        "cpu_cliffs": {
            "tier1_bridge": dict(bridge_totals, details=ranked_entries(bridge_details)),
            "tier1_bridge_reports": bridge_reports,
            "tier1_bridge_unmeasured_reports": len(per_report) - bridge_reports,
            "tier1_census": census,
            "tier1_census_reports": census_reports,
            "fallbacks": ranked(fallbacks),
            "fallbacks_retained_total": sum(fallbacks.values()),
            "interpreter": dict(sorted(interpreter.items(),
                                       key=lambda row: (-row[1]["instructions"], row[0]))),
            "interpreter_total": {key: sum(row[key] for row in interpreter.values())
                                  for key in ("entries", "instructions")},
            "interpreter_details": ranked_entries(details),
            "interpreter_details_omitted": omitted,
            "interpreter_details_unknown": unknown,
            "x87_helper_calls": helpers,
            "x87_fast": fast,
            "x87_exact_total": helpers - fast,
            "x87_exact": ranked(exact),
            "x87_exact_by_mnemonic": ranked(by_mnemonic),
            "x87_exact_by_reason": ranked(by_reason),
            "x87_exact_unattributed": unattributed,
            "x87_control_reports": control_reports,
            "x87_control": ranked(controls),
            "x87_control_rejections": dict(control_guards),
            "x87_pc64_reports": pc64_reports,
            "x87_pc64": ranked(pc64),
        },
        "report_limits": {
            "producer_row_limit": REPORT_ROW_LIMIT,
            "fallback_reports_at_limit": fallback_limits,
            "x87_exact_reports_at_limit": exact_limits,
            "interpreter_detail_reports": detail_reports,
            "interpreter_detail_rows_omitted": omitted_rows,
        },
        "tier_verification": verification,
        "per_report": per_report,
    }


def read_reports(paths: Iterable[Path]) -> Iterable[dict]:
    """Read UTF-8 reports, including a BOM from Windows redirection."""
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result

    for index, path in enumerate(paths, 1):
        try:
            yield json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=unique_object)
        except (OSError, UnicodeError, ValueError):
            raise ValueError(f"input {index}: cannot read a unique-key UTF-8 JSON report") from None


def markdown(summary: dict) -> str:
    """Render only aggregate counters and ordinal verification rows."""
    cliffs = summary["cpu_cliffs"]
    limits = summary["report_limits"]
    lines = [f"输入报告数：{summary['report_count']}", "",
             "计数只覆盖各报告保留的行；普通运行和 verify 运行应分别汇总。",
             f"达到 {REPORT_ROW_LIMIT} 行上限的报告：native {limits['fallback_reports_at_limit']}，"
             f"x87 {limits['x87_exact_reports_at_limit']}。", "",
             "Native 回退 Top 20（按保留行合计；同次数按助记符排序）", "",
             "| 助记符 | 次数 |", "|---|---:|"]
    lines.extend(f"| {name} | {value} |" for name, value in list(cliffs["fallbacks"].items())[:20])
    lines.extend(["", f"Native 保留行合计：{cliffs['fallbacks_retained_total']}；"
                  "被源报告截去的回退次数未知。", "", "解释器入口", "",
                  "| 原因 | 入口数 | 指令单位数 |", "|---|---:|---:|"])
    lines.extend(f"| {reason} | {values['entries']} | {values['instructions']} |"
                 for reason, values in cliffs["interpreter"].items())
    lines.extend(["", "解释器入口细分 Top 24（整批指令单位归于入口状态；不是逐条指令热度）", "",
                  "| 入口原因 / 细分 / 形式 | 入口数 | 指令单位数 |", "|---|---:|---:|"])
    lines.extend(f"| {label} | {values['entries']} | {values['instructions']} |"
                 for label, values in list(cliffs["interpreter_details"].items())[:REPORT_ROW_LIMIT])
    lines.extend(["", f"含细分的报告：{limits['interpreter_detail_reports']}；"
                  f"源报告截断行数：{limits['interpreter_detail_rows_omitted']}。",
                  "汇总 JSON 保留所有源报告已保留行；下表单列截断和旧报告缺失，不推断具体原因。", "",
                  "| 大桶 | 源报告截断指令单位 | 旧报告未细分指令单位 |", "|---|---:|---:|"])
    lines.extend(f"| {reason} | {cliffs['interpreter_details_omitted'][reason]['instructions']} | "
                 f"{cliffs['interpreter_details_unknown'][reason]['instructions']} |" for reason in DETAIL_BUCKETS)
    lines.extend(["", f"Tier 1 Bridge 覆盖 {cliffs['tier1_bridge_reports']} 份报告；"
                  f"未测 {cliffs['tier1_bridge_unmeasured_reports']} 份；"
                  f"接管 {cliffs['tier1_bridge']['instructions']} 单位。",
                  "", "| 原入口状态 | Bridge 入口 | 接管单位 |", "|---|---:|---:|"])
    lines.extend(f"| {label} | {n['entries']} | {n['instructions']} |"
                 for label, n in cliffs["tier1_bridge"]["details"].items())
    lines.extend(["", f"Tier 1 普查覆盖 {cliffs['tier1_census_reports']} 份报告；缺失或关闭不视为零。",
                  "只统计 Uncovered/Rejected 的实际退休单位，PendingCompilation 单列且不重复。",
                  "形式可发射不保证访存、代码验证、预算或 CFG 准入；全支持前缀为截尾下界。", "",
                  "| 入口组 | 支持单位 | 不支持单位 | 连续前缀单位 | 截去的不支持单位 |", "|---|---:|---:|---:|---:|"])
    for group, row in cliffs["tier1_census"].items():
        lines.append(f"| {group} | {row['supported_units']} | {row['unsupported_units']} | {row['prefix_units']} | {row['omitted_units']} |")
        lines.extend(["", f"{group} 不支持 Top 24（各报告保留行之和）", "", "| iced Code | 单位 |", "|---|---:|"])
        lines.extend(f"| {label} | {n} |" for label, n in list(row["unsupported"].items())[:REPORT_ROW_LIMIT])
        lines.extend(["", "前缀长度桶：0、1、2–3、4–7、8–15、16–31、32–63、64+（解码指令条数）。",
                      f"遇阻：{row['blocked_prefixes']}；仍全支持：{row['censored_prefixes']}。"])
    lines.extend(["", f"x87 helper：{cliffs['x87_helper_calls']}；fast：{cliffs['x87_fast']}；"
                  f"exact：{cliffs['x87_exact_total']}；未归因 exact：{cliffs['x87_exact_unattributed']}。",
                  "", "| x87 原因 | 次数（保留行） |", "|---|---:|"])
    lines.extend(f"| {reason} | {value} |" for reason, value in cliffs["x87_exact_by_reason"].items())
    lines.extend(["", "| x87 助记符 / 原因 | 次数（保留行） |", "|---|---:|"])
    lines.extend(f"| {label} | {value} |" for label, value in cliffs["x87_exact"].items())
    lines.extend(["", f"Control 细分覆盖 {cliffs['x87_control_reports']} 份报告；"
                  "PC／RC／Masks 为独立计数，不相加。非算术指令仍可能因操作数拒绝。",
                  "", "| 控制字分类 | 次数 |", "|---|---:|"])
    lines.extend(f"| {label} | {value} |" for label, value in cliffs["x87_control"].items())
    lines.extend(f"| {label} 拒绝 | {value} |" for label, value in cliffs["x87_control_rejections"].items())
    lines.extend(["", f"PC=64 候选细分覆盖 {cliffs['x87_pc64_reports']} 份报告；"
                  "Exact 为正常结果精确，Zero 单列，Range 不作精确性判断。",
                  "", "| 助记符 / 候选分类 | 次数 |", "|---|---:|"])
    lines.extend(f"| {label} | {value} |" for label, value in cliffs["x87_pc64"].items())
    lines.extend(["", "Tier verification（报告序号按输入顺序；全零不证明启用了校验）", "",
                  "| 报告 | verified | inconclusive | mismatches | mxcsr_flags |",
                  "|---|---:|---:|---:|---:|"])
    for row in summary["per_report"] + [{"report": "合计", "tier_verification": summary["tier_verification"]}]:
        values = " | ".join(str(row["tier_verification"][key]) for key in VERIFY_COUNTERS)
        lines.append(f"| {row['report']} | {values} |")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("reports", nargs="+", type=Path, help="one final JSON per independent run")
    parser.add_argument("--json", action="store_true", help="write all aggregate counters as JSON")
    args = parser.parse_args(argv)
    try:
        summary = aggregate(read_reports(args.reports))
    except ValueError as error:
        print(f"cliffs: {error}", file=sys.stderr)
        return 2
    print(json.dumps(summary, ensure_ascii=True, indent=2) if args.json else markdown(summary),
          end="\n" if args.json else "")
    return 0


if __name__ == "__main__":
    sys.exit(main())
