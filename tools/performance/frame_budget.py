#!/usr/bin/env python3
"""Summarize saved frame-budget runs.

Usage: frame_budget.py summarize DIR
Output: summary.json/summary.md contain statistics only.
UX summaries use a complete ordered sequence for continuous presentation windows,
estimated loss relative to authored cadence_ms or an explicitly labelled p20
proxy, Ctrl commits/second, and cold title-gate time with inclusive cost subsets.
Possible loading gaps and observer freezes are kept separate; a presentation
candidate is not proof of animation, native cadence, or title interactivity.
"""
from __future__ import annotations

import argparse
from collections import Counter
import importlib.util
import json
import math
from pathlib import Path
import re
import sys

from sample_matrix import write_json

REPO = Path(__file__).resolve().parents[2]
SCENARIOS = Path(__file__).with_name("scenarios")
sys.path.insert(0, str(REPO / "tools" / "android"))
from scene_schedule import valid_acknowledgement


def import_tool(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


baseline = import_tool("budget_baseline", REPO / "tools/android/summarize-baseline.py")
BUDGET_NS = 33_300_000
SCENES = ("title", "normal", "ctrl", "transition")
REASONS = ("uncovered", "rejected", "pending", "interior", "fallback", "forced")
HLE_GROUPS = ("graphics", "file", "audio", "other")
CPU_FIELDS = ("tier0_units", "tier2_blocks", "native_entries", "interpreter_entries",
              "sync_compile_ns", "compile_wait_execution_ns", "compile_wait_units", "invalidations",
              "recompilations", "compilations", "x87_helpers", "softfloat_calls", "retired_tier0_instructions")


def numeric_fields(value: dict, names) -> dict:
    return {name: value[name] for name in names if type(value.get(name)) in (int, float)
            and math.isfinite(value[name]) and value[name] >= 0}


def clean_row(row: dict) -> dict:
    """Use an allowlist so future raw diagnostics cannot leak into shareable output."""
    result = numeric_fields(row, ("serial", "wall_ns", "active_ns", "guest_exec_ns", "present_call_ns",
                                 "wait_ns", "wait_overshoot_ns", "wait_timeouts", "core", "frequency_khz",
                                 "scheduler_ns", "frontend_ns", "control_pause_ns", "accounting_excess_ns",
                                 "service_audio_ns", "service_graphics_ns", "scene_check_ns",
                                 "present_interval_ns", "guest_delta_us", "file_read_bytes"))
    result.update(startup=row.get("startup") is True,
                  hle_ns=numeric_fields(row.get("hle_ns", {}), HLE_GROUPS),
                  native_reference="unmeasured")
    result["operation_ns"] = numeric_fields(row.get("operation_ns", {}), ("text", "image_decode", "audio_decode"))
    cpu = row.get("cpu")
    result["cpu"] = None if cpu is None else numeric_fields(cpu, CPU_FIELDS) | {
        "reasons": numeric_fields(cpu.get("reasons", {}), REASONS),
        "reason_ns": numeric_fields(cpu.get("reason_ns", {}), REASONS)}
    result["top_apis"] = [dict(api=item["api"], **numeric_fields(item, ("ns", "calls")))
                          for item in row.get("top_apis", [])[:5]
                          if isinstance(item, dict) and isinstance(item.get("api"), str)
                          and re.fullmatch(r"[A-Za-z0-9_.-]+![A-Za-z0-9_?@$.:<>-]+", item["api"])]
    return result


def execution_metrics(report: dict) -> dict:
    raw = baseline.execution_metrics(report)
    schedule = raw.get("compilation_schedule") or {}
    cache = report.get("code_cache") or {}
    aot = report.get("aot") or {}
    return {"counts": numeric_fields(report, ("native_entries", "interpreter_entries", "compilations", "compile_requests", "compile_seconds")),
        "code_cache": {"enabled": cache.get("enabled") is True} | numeric_fields(cache,
            ("hits", "misses", "rejected", "corrupt", "stored", "unsupported", "restore_ns", "evicted", "entries", "bytes")),
        "aot": numeric_fields(aot, ("images", "image_rejections", "candidates", "decoded_instructions", "budget_exhaustions",
            "compile_requests", "private_completions", "publications", "stale_candidates", "ready_regions", "ready_bytes")),
        "compilation_schedule": numeric_fields(schedule, ("compile_threads", "available_cores", "duplicate_requests",
            "capacity_rejections", "window_deferrals", "worker_full"))}


def wait_metrics(report: dict) -> dict | None:
    """Keep only fixed source labels and numeric distributions in shared summaries."""
    raw = report.get("wait_diagnostics")
    if not isinstance(raw, dict) or raw.get("schema") != "adv-wait-diagnostics-v1":
        return None
    sources = raw.get("sources") or {}
    result = {}
    for name in ("sleep", "sleep_ex", "object", "message", "multimedia_timer", "waitable_timer", "present", "yield", "other"):
        row = sources.get(name) or {}
        if not isinstance(row, dict):
            continue
        clean = numeric_fields(row, ("segments", "timeouts", "signaled", "skipped_segments", "wall_total_us", "retained",
            "host_waits", "host_requested_ns", "host_elapsed_ns", "host_overshoot_ns"))
        for key in ("requested_us", "wall_us", "guest_us", "lateness_us", "dispatch_queue_us",
                    "host_requested_us", "host_elapsed_us", "host_overshoot_us",
                    "timed_requested_us", "timed_wall_us", "timed_lateness_us"):
            values = row.get(key)
            clean[key] = numeric_fields(values, ("count", "p50", "p95", "p99", "max")) if isinstance(values, dict) else None
        result[name] = clean
    return {"sources": result, **numeric_fields(raw, ("capacity_per_source",))}


def valid_budget(value: dict) -> None:
    if value.get("schema") != "adv-frame-budget-v1" or value.get("budget_ns") != BUDGET_NS:
        raise ValueError("unknown frame budget schema")
    for name in ("frames", "over_budget_frames", "active_over_budget_frames", "max_wall_ns", "max_active_ns",
                 "wall_total_ns", "active_total_ns", "percentile_window", "percentile_capacity",
                 "coalesced_frames", "observer_ns"):
        if type(value.get(name)) is not int or value[name] < 0:
            raise ValueError("invalid frame counter")
    if (not value["frames"] or value["over_budget_frames"] > value["frames"]
            or value["active_over_budget_frames"] > value["frames"]
            or value["percentile_window"] > min(value["frames"], value["percentile_capacity"])):
        raise ValueError("invalid frame count relationship")
    for name in ("p95_wall_ns", "p99_wall_ns", "p99_active_ns"):
        if type(value.get(name)) not in (int, float) or not math.isfinite(value[name]) or value[name] < 0:
            raise ValueError("invalid percentile")


def classify(row: dict) -> dict:
    """Rank explicit heuristics; counts are evidence, never a CPU time estimate."""
    row = clean_row(row)
    cpu = row.get("cpu") or {}
    active = row.get("active_ns", 0)
    wall = row.get("wall_ns", 0)
    hle = row.get("hle_ns", {})
    reasons = sorted(cpu.get("reasons", {}).items(), key=lambda x: (-x[1], x[0]))[:3]
    evidence = {"active_ms": active / 1e6, "wall_ms": wall / 1e6,
                "top_interpreter_reasons": dict(reasons)}
    candidates = []
    threshold = max(8_000_000, active * .3)
    if cpu.get("sync_compile_ns", 0) >= threshold:
        candidates.append((cpu["sync_compile_ns"], "cold.sync_compile", "measured"))
    if cpu.get("compile_wait_execution_ns", 0) >= threshold:
        candidates.append((cpu["compile_wait_execution_ns"], "cold.compile_pending_execution", "measured_overlap"))
    lane_times = cpu.get("reason_ns", {})
    for name, duration in lane_times.items():
        if name != "pending" and duration >= threshold:
            candidates.append((duration, "execution.interpreter_" + name, "measured_overlap"))
    if hle:
        group, duration = max(hle.items(), key=lambda x: x[1])
        if duration >= threshold:
            candidates.append((duration, "host." + group, "measured"))
    overshoot = row.get("wait_overshoot_ns", 0)
    if overshoot >= 1_000_000 and (wall - overshoot <= BUDGET_NS or overshoot >= wall * .3):
        candidates.append((overshoot, "platform.wait_overshoot", "measured"))
    for key, cause in (("scheduler_ns", "host.scheduler"), ("frontend_ns", "host.frontend_control"),
                       ("control_pause_ns", "observer.control_pause")):
        duration = row.get(key, 0)
        if duration >= max(8_000_000, wall * .3):
            candidates.append((duration, cause, "residual_hint" if key == "frontend_ns" else "measured"))
    work = active + row.get("service_audio_ns", 0) + row.get("service_graphics_ns", 0)
    for key, cause in (("service_audio_ns", "host.audio_service"), ("service_graphics_ns", "host.graphics_service"),
                       ("scene_check_ns", "observer.scene_check")):
        duration = row.get(key, 0)
        if duration >= max(8_000_000, work * .3):
            candidates.append((duration, cause, "measured"))
    if candidates:
        _, cause, confidence = max(candidates)
    elif (lane_times and active > BUDGET_NS and cpu.get("tier2_blocks", 0)
          and row.get("guest_exec_ns", 0) - sum(lane_times.values()) - cpu.get("sync_compile_ns", 0) >= active * .6):
        # Entry/helper counts cannot outrank a measured small fallback lane.
        cause, confidence = "structure.tier2_execution", "residual_hint"
    elif active > BUDGET_NS and cpu.get("softfloat_calls", 0) >= 1000:
        cause, confidence = "pathological.softfloat", "count_hint"
    elif active > BUDGET_NS and cpu.get("invalidations", 0) >= 8 and cpu.get("compilations", 0) >= 8:
        cause, confidence = "pathological.recompile", "count_hint"
    elif active > BUDGET_NS and cpu.get("interpreter_entries", 0) >= 1000:
        cause, confidence = "pathological.interpreter_storm", "count_hint"
    elif (active > BUDGET_NS and row.get("guest_exec_ns", 0) >= active * .6
          and cpu.get("tier2_blocks", 0) and cpu.get("interpreter_entries", 0) < 1000):
        cause, confidence = "structure.tier2_execution", "residual_hint"
    elif row.get("wait_ns", 0) >= wall * .5 and wall > BUDGET_NS:
        cause, confidence = "platform.wait_or_idle", "intent_unknown"
    elif (wall > BUDGET_NS and row.get("wait_ns", 0) >= 1_000_000
          and active + row.get("wait_ns", 0) >= wall * .8):
        cause, confidence = "platform.mixed_wait_execution", "measured_combination"
    elif (wall > BUDGET_NS and row.get("wait_ns", 0) >= 1_000_000
          and work + row.get("wait_ns", 0) >= wall * .8):
        cause, confidence = "platform.mixed_wait_execution_services", "measured_combination"
    elif (wall > BUDGET_NS and row.get("scene_check_ns", 0) >= 1_000_000
          and wall - row["scene_check_ns"] <= BUDGET_NS
          and work + row.get("wait_ns", 0) + row["scene_check_ns"] >= wall * .8):
        cause, confidence = "observer.mixed_scene_work", "measured_combination"
    elif active > BUDGET_NS and row.get("guest_exec_ns", 0) >= active * .6:
        cause, confidence = "structure.guest_execution", "residual_hint"
    else:
        cause, confidence = "unattributed", "insufficient"
    for key in ("sync_compile_ns", "compile_wait_execution_ns", "compile_wait_units", "softfloat_calls",
                "interpreter_entries", "invalidations", "recompilations", "tier2_blocks"):
        evidence[key] = cpu.get(key)
    evidence.update(reason_ns=lane_times, hle_ns=hle, wait_ns=row.get("wait_ns"), wait_overshoot_ns=overshoot,
                    core=row.get("core"), frequency_khz=row.get("frequency_khz"), top_apis=row.get("top_apis", []),
                    **numeric_fields(row, ("scheduler_ns", "frontend_ns", "control_pause_ns", "accounting_excess_ns",
                                           "service_audio_ns", "service_graphics_ns", "scene_check_ns")))
    return {"cause": cause, "confidence": confidence, "evidence": evidence}


def native_estimate(row: dict, rate: int | None) -> dict:
    """Keep a confirmed Tier 0 subset separate from the missing native retirement.

    The fastest authored x64 rate deliberately minimizes estimated time. This
    conditional estimate is not an ARM64 hardware bound and never excludes a frame.
    """
    count = (row.get("cpu") or {}).get("retired_tier0_instructions")
    ns = count * 1_000_000_000 // rate if count is not None and rate else None
    return {"retired_guest_instructions_lower_bound": count,
            "retirement_coverage": "tier0_only" if count is not None else "unmeasured",
            "native_time_lower_estimate_ns": ns,
            "native_over_budget_estimate": ns is not None and ns > BUDGET_NS}


def summarize_budget(budget: dict, native_rate: int | None = None) -> dict:
    rows = [dict(clean_row(row), attribution=classify(row)) for row in budget["rows"]]
    for row in rows:
        row.update(native_estimate(row, native_rate))
    over_rows = [r for r in rows if r["wall_ns"] > budget["budget_ns"]]
    return {key: budget[key] for key in (
        "frames", "over_budget_frames", "active_over_budget_frames", "max_wall_ns", "max_active_ns",
        "wall_total_ns", "active_total_ns", "p95_wall_ns", "p99_wall_ns", "p99_active_ns",
        "percentile_window", "percentile_capacity", "coalesced_frames", "observer_ns")} | {
        "retained_over_budget_frames": len(over_rows),
        "dropped_over_budget_rows": max(0, budget["over_budget_frames"] - len(over_rows)),
        "cause_distribution_retained": dict(Counter(r["attribution"]["cause"] for r in over_rows)),
        "top_apis_retained": top_apis(over_rows),
        "longest": dict(clean_row(budget["longest"]), attribution=classify(budget["longest"]),
                        **native_estimate(budget["longest"], native_rate)) if budget.get("longest") else None,
        "native_reference": {"status": "partial_estimate" if native_rate else "unmeasured", "excluded_frames": 0,
                             "instructions_per_second": native_rate,
                             "over_budget_estimate_retained": [r["serial"] for r in over_rows if r["native_over_budget_estimate"]]},
        "rows": rows,
    }


def top_apis(rows: list[dict]) -> list[dict]:
    """Aggregate retained frame top-five subsets, never claim full API totals."""
    times, calls = Counter(), Counter()
    for row in rows:
        for api in row.get("top_apis", []):
            times[api["api"]] += api.get("ns", 0)
            calls[api["api"]] += api.get("calls", 0)
    return [dict(api=api, ns=ns, calls=calls[api]) for api, ns in times.most_common(5)]


def distribution(values: list[float]) -> dict | None:
    if not values:
        return None
    values = sorted(values)
    return {"count": len(values), **{name: values[max(0, math.ceil(len(values) * p) - 1)]
            for name, p in (("p20", .2), ("p50", .5), ("p95", .95), ("p99", .99), ("max", 1))}}


def cost_totals(rows: list[dict]) -> dict:
    """Disjoint wall lanes; compiler/fallback and operations are inclusive subsets."""
    fields = ("wall_ns", "active_ns", "guest_exec_ns", "wait_ns", "wait_overshoot_ns",
              "scheduler_ns", "frontend_ns", "control_pause_ns", "scene_check_ns",
              "service_audio_ns", "service_graphics_ns", "file_read_bytes")
    total = {key: sum(row.get(key, 0) for row in rows) for key in fields}
    for key, names in (("hle_ns", HLE_GROUPS), ("operation_ns", ("text", "image_decode", "audio_decode"))):
        total[key] = {name: sum(row.get(key, {}).get(name, 0) for row in rows) for name in names}
    total["cpu"] = {key: sum((row.get("cpu") or {}).get(key, 0) for row in rows) for key in CPU_FIELDS}
    for key in ("reasons", "reason_ns"):
        total["cpu"][key] = {name: sum((row.get("cpu") or {}).get(key, {}).get(name, 0) for row in rows)
                              for name in REASONS}
    fallback_ns = sum(total["cpu"]["reason_ns"].values())
    total["cpu"]["fallback_lane_ns"] = fallback_ns
    total["cpu"]["native_dispatch_residual_ns"] = max(0, total["guest_exec_ns"]
                                                       - total["cpu"]["sync_compile_ns"] - fallback_ns)
    total["top_apis_subset"] = top_apis(rows)
    return total


def animation_windows(rows: list[dict], cadence_ms: float | None = None) -> dict:
    """Find continuous presentation windows, preserving uncertain load boundaries.

    A provided cadence is an authored scene reference. Otherwise p20 of the
    observed continuous intervals is only a self-cadence proxy, never a native
    reference. IO-bearing gaps >=500ms separate *possible* loading; long compute
    frames remain in animation windows. Diagnostic freezes split windows too.
    """
    if cadence_ms is not None and (type(cadence_ms) not in (int, float)
            or not math.isfinite(cadence_ms) or cadence_ms <= 0):
        raise ValueError("cadence_ms must be positive and finite")
    chunks, chunk, boundaries = [], [], []
    for row in rows:
        interval = row.get("present_interval_ns")
        if row.get("startup") or interval is None:
            continue
        observer = row.get("control_pause_ns", 0) + row.get("scene_check_ns", 0)
        boundary = ("observer" if observer >= max(20_000_000, interval * .5) else
                    "possible_loading" if interval >= 500_000_000 and row.get("file_read_bytes", 0) else
                    "idle_or_loading_unknown" if interval >= 500_000_000 and row.get("wait_ns", 0) >= interval * .8 else None)
        if boundary:
            if chunk:
                chunks.append(chunk)
                chunk = []
            boundaries.append({"serial": row["serial"], "kind": boundary,
                               "interval_ms": interval / 1e6, "attribution": classify(row)})
        else:
            chunk.append(row)
    if chunk:
        chunks.append(chunk)
    phases = []
    for chunk in chunks:
        # A sustained switch between work and waiting is a cadence boundary,
        # not hundreds of lost animation frames inferred from an earlier burst.
        modes = [r.get("wait_ns", 0) >= r.get("wall_ns", 0) * .7 for r in chunk]
        begin = 0
        mode = modes[0]
        for i in range(8, len(chunk) - 7):
            following = modes[i:i + 8]
            if all(m == following[0] for m in following) and following[0] != mode:
                phases.append(chunk[begin:i])
                boundaries.append({"serial": chunk[i]["serial"], "kind": "cadence_phase_change",
                                   "interval_ms": chunk[i]["present_interval_ns"] / 1e6})
                begin, mode = i, following[0]
        phases.append(chunk[begin:])
    chunks = phases
    windows = []
    for chunk in chunks:
        intervals = [r["present_interval_ns"] / 1e6 for r in chunk]
        stats = distribution(intervals)
        if len(chunk) < 8:
            continue
        cadence = cadence_ms if cadence_ms is not None else stats["p20"]
        if cadence <= 0:
            continue
        missed = [max(0, math.floor(t / cadence + .5) - 1) for t in intervals]
        runs, run = [], 0
        for count in missed + [0]:
            if count:
                run += count
            elif run:
                runs.append(run)
                run = 0
        stalls = [r for r, count in zip(chunk, missed) if count]
        windows.append({"start_serial": chunk[0]["serial"], "end_serial": chunk[-1]["serial"],
            "interval_ms": stats, "cadence_ms": cadence,
            "observed_frequency_hz": 1000 / stats["p50"] if stats["p50"] else None,
            "cadence_reference": "scene_reference" if cadence_ms is not None else "observed_p20_proxy",
            "waiting_dominated": sum(r.get("wait_ns", 0) >= r.get("wall_ns", 0) * .7 for r in chunk) >= len(chunk) * .7,
            "estimated_missed_presents": sum(missed),
            "gaps_missing_at_least_two": sum(count >= 2 for count in missed),
            "consecutive_loss_events_at_least_two": sum(count >= 2 for count in runs),
            "longest_consecutive_loss": max(runs, default=0),
            "stalled_present_intervals": len(stalls), "costs": cost_totals(chunk),
            "stall_costs": cost_totals(stalls),
            "tier_up_publication_frames": sum((r.get("cpu") or {}).get("recompilations", 0) > 0 for r in chunk),
            "stall_causes": dict(Counter(classify(r)["cause"] for r in stalls)),
            "longest": dict(clean_row(max(chunk, key=lambda r: r["present_interval_ns"])),
                            attribution=classify(max(chunk, key=lambda r: r["present_interval_ns"])))})
    return {"scope": "continuous_presentation_candidates", "native_reference": "unmeasured",
            "unclassified_frames": len(rows) - sum(w["interval_ms"]["count"] for w in windows)
                - sum(b["kind"] != "cadence_phase_change" for b in boundaries),
            "windows": windows, "boundaries": sorted(boundaries, key=lambda b: b["serial"])}


def ux_metrics(budget: dict, definition: dict) -> dict:
    """Reject sparse/evicted sequences for cadence and whole-window cost totals."""
    rows = [clean_row(row) for row in budget.get("sequence", [])]
    serials = [row.get("serial") for row in rows]
    complete = (len(rows) == budget.get("frames") and bool(rows)
                and budget.get("sequence_dropped", 0) == 0
                and budget.get("coalesced_frames", 0) == 0
                and all(type(s) is int for s in serials)
                and all(b == a + 1 for a, b in zip(serials, serials[1:])))
    result = {"sequence_complete": complete, "retained_frames": len(rows),
              "dropped_frames": budget.get("sequence_dropped"),
              "file_read_scope": "ReadFile_returned_bytes_including_live_NT_bridge",
              "image_decode_scope": "recognized_HLE_APIs_only_guest_decode_unmeasured"}
    if complete:
        result.update(costs=cost_totals(rows), animation=animation_windows(rows, definition.get("cadence_ms")))
    else:
        result["reason"] = "full_contiguous_sequence_required"
    return result


def first_entry(directory: Path, checkpoint: str, cold: bool, ux: dict, gated: bool) -> dict:
    """Cold title gate elapsed time includes authored startup delays and input."""
    if not cold or not gated:
        return {"status": "unmeasured", "reason": "cold_verified_title_gate_required"}
    actions = load_json(directory / "actions.json", [])
    report = next((a for a in actions if a.get("command", "").endswith("/" + checkpoint + ".json")
                   or a.get("command", "").endswith("\\" + checkpoint + ".json")), None)
    # The acknowledgement, rather than a wall-clock frame total, observes title
    # arrival; the later checkpoint serialization is outside this upper bound.
    title = next((a for a in reversed(actions[:actions.index(report)])
                  if a.get("command", "").startswith("await-frame ")) if report else None, None)
    elapsed = title.get("ack_seconds") if title else None
    valid = type(elapsed) in (int, float) and math.isfinite(elapsed) and elapsed > 0
    return {"status": "observed_upper_bound" if valid else "unmeasured",
            "seconds": elapsed if valid else None, "includes_authored_delays_and_input": True,
            "cache_state": "empty_overlay_and_discovery_profile_caller_required",
            "title_interactivity": "title_pixels_only_following_input_requires_review", "costs": ux.get("costs"),
            "cost_scope": "completed_present_intervals_tail_not_included"}


def load_json(path: Path, default=None):
    return json.loads(path.read_text(encoding="utf-8-sig")) if path.is_file() else default


def fixed_segment(directory: Path, scenario: dict, checkpoint: str) -> dict | None:
    steps = scenario["schedule"].get("steps", []) if isinstance(scenario["schedule"], dict) else []
    target = next((s for s in steps if s.get("checkpoint") == checkpoint), {})
    spec = target.get("await", {})
    if spec.get("hold_key") != 17:
        return None
    start_name = scenario["scenes"]["ctrl"].get("start_checkpoint")
    start = load_json(directory / f"{start_name}.json", {})
    end = load_json(directory / f"{checkpoint}.json", {})
    if not start or not end or end.get("frame_fnv1a64") != spec.get("frame_fnv1a64"):
        return None
    try:
        baseline.valid_report(start, "segment start")
        baseline.valid_report(end, "segment end")
        if any(r.get("scene_gate", {}).get("frozen") is not True or r.get("scene_gate", {}).get("pending") is not False for r in (start, end)):
            return None
    except (ValueError, RuntimeError, TypeError, KeyError):
        return None
    start_step = next((s for s in steps if s.get("checkpoint") == start_name), {})
    if start.get("frame_fnv1a64") != start_step.get("await", {}).get("frame_fnv1a64"):
        return None
    for action in load_json(directory / "actions.json", []):
        if action.get("command", "").startswith("await-frame " + spec["frame_fnv1a64"] + " "):
            ack = action.get("ack") or action.get("reply") or ""
            words = action["command"].split()
            if (not valid_acknowledgement(action["command"], ack)
                    or "hold" not in words or words[words.index("hold") + 1] != "17"):
                continue
            fields = dict(word.split("=", 1) for word in ack.split()[3:] if "=" in word)
            elapsed = action["ack_seconds"] - action["sent_seconds"]
            if (int(fields["frame"]) != end["frames"] or int(fields["start_frame"]) != start["frames"]
                    or end["frames"] <= start["frames"] or not math.isfinite(elapsed) or elapsed <= 0):
                return None
            return {"seconds": elapsed,
                    "start_frame": start["frames"], "end_frame": end["frames"],
                    "advanced_frames": end["frames"] - start["frames"],
                    "commits_per_second": (end["frames"] - start["frames"]) / elapsed,
                    "time_skip_delta": {key: end["time_skip"][key] - start["time_skip"][key]
                        for key in ("jumps", "skipped_guest_us")
                        if type((end.get("time_skip") or {}).get(key)) is int
                        and type((start.get("time_skip") or {}).get(key)) is int
                        and end["time_skip"][key] >= start["time_skip"][key]},
                    "endpoint_hashes_matched": True, "independent_reference": False,
                    "includes_input_delivery": True}
    return None


def timed_ctrl_segment(directory: Path, scenario: dict, checkpoint: str) -> dict | None:
    """Verified held Ctrl from a frozen start, with an explicit timed-work scope."""
    definition = scenario["scenes"]["ctrl"]
    start_name = definition.get("start_checkpoint")
    start, end = (load_json(directory / f"{name}.json", {}) for name in (start_name, checkpoint))
    if (not start or not end or start.get("scene_gate", {}).get("frozen") is not True
            or start.get("scene_gate", {}).get("pending") is not False):
        return None
    try:
        baseline.valid_report(start, "timed Ctrl start")
        baseline.valid_report(end, "timed Ctrl end")
    except (ValueError, RuntimeError, TypeError, KeyError):
        return None
    steps = scenario["schedule"].get("steps", [])
    step = next((s for s in steps if s.get("checkpoint") == start_name), {})
    if start.get("frame_fnv1a64") != step.get("await", {}).get("frame_fnv1a64"):
        return None
    actions = load_json(directory / "actions.json", [])
    name_matches = lambda a: a.get("command", "").replace("\\", "/").endswith("/" + checkpoint + ".json")
    end_index = next((i for i, a in enumerate(actions) if a.get("command", "").startswith("report ") and name_matches(a)), None)
    if end_index is None:
        return None
    down_index = next((i for i in range(end_index - 1, -1, -1) if actions[i].get("command") == "key-down 17"), None)
    if down_index is None or any(a.get("command") == "key-up 17" for a in actions[down_index + 1:end_index]):
        return None
    down = actions[down_index]
    release = next((a for a in actions[end_index + 1:] if a.get("command") == "key-up 17"), None)
    if not release or any(not valid_acknowledgement(a["command"], a.get("ack") or a.get("reply") or "") for a in (down, release)):
        return None
    seconds = actions[end_index]["sent_seconds"] - down["ack_seconds"]
    frames = end["frames"] - start["frames"]
    if not math.isfinite(seconds) or seconds <= 0 or frames <= 0:
        return None
    return {"seconds": seconds, "advanced_frames": frames, "commits_per_second": frames / seconds,
            "start_hash_matched": True, "held_key_verified": True, "fixed_workload": False,
            "includes_input_delivery": True, "reference": "fixed_start_timed_observation",
            "time_skip_delta": {key: end["time_skip"][key] - start["time_skip"][key]
                for key in ("jumps", "skipped_guest_us") if type((end.get("time_skip") or {}).get(key)) is int
                and type((start.get("time_skip") or {}).get(key)) is int
                and end["time_skip"][key] >= start["time_skip"][key]}}


def summarize_run(directory: Path, scenario: dict, heat: str, platform: str, native_rate: int | None = None) -> list[dict]:
    result = []
    metadata = load_json(directory / "session.json", {})
    for name in SCENES:
        definition = scenario["scenes"].get(name, {})
        checkpoint = definition.get("checkpoint")
        corrupt = False
        try:
            report = load_json(directory / f"{checkpoint}.json", {}) if checkpoint else {}
        except (ValueError, OSError):
            report, corrupt = {}, True
        row = {"game": scenario["id"], "scene": name, "heat": heat, "platform": platform,
               "binary_sha256": metadata.get("binary_sha256"),
               "guest_exe_sha256": metadata.get("guest_exe_sha256"),
               "scenario_sha256": scenario["scenario_sha256"],
               "measurement": definition.get("measurement", "not_run"),
               "status": "skipped", "reason": definition.get("unavailable", "checkpoint_not_reached"),
               "warmth": "empty_jit_profile" if heat == "cold" else "new_process_reusing_discovery_profile"}
        row["run_status"] = ("failed" if metadata.get("error") or any(metadata.get(k) not in (0, None) for k in
            ("exit_code", "remote_exit_code", "adb_exit_code")) else "incomplete" if
            metadata.get("completed") is False or metadata.get("evidence_complete") is False else "complete")
        if not metadata:
            row["run_status"] = "not_run"
        if corrupt:
            row.update(status="blocked", reason="invalid_checkpoint_json")
        if report:
            try:
                baseline.valid_report(report, "checkpoint")
                steps = scenario["schedule"].get("steps", []) if isinstance(scenario["schedule"], dict) else scenario["schedule"]
                step = next((s for s in steps if s.get("checkpoint") == checkpoint), {})
                if "await" in step and (report.get("frame_fnv1a64") != step["await"]["frame_fnv1a64"]
                        or report.get("scene_gate", {}).get("frozen") is not True
                        or report.get("scene_gate", {}).get("pending") is not False):
                    raise ValueError("checkpoint gate does not match")
                if not report.get("frame_budget"):
                    row.update(status="control", reason="instrumentation_disabled", performance=numeric_fields(report.get("performance") or {},
                        ("runtime_step_seconds", "runtime_steps", "runtime_step_max_seconds", "measurement_elapsed_seconds")))
                    if name == "ctrl":
                        row["fixed_segment"] = fixed_segment(directory, scenario, checkpoint)
                    result.append(row)
                    continue
                if report["frame_budget"]["coalesced_frames"]:
                    raise ValueError("coalesced frame boundaries")
                valid_budget(report["frame_budget"])
                row.update(status="observed", reason=None, metrics=summarize_budget(report["frame_budget"], native_rate),
                           execution=execution_metrics(report), config=baseline.report_config(report),
                           wait_diagnostics=wait_metrics(report),
                           time_skip=numeric_fields(report.get("time_skip") or {}, ("jumps", "skipped_guest_us")))
                row["ux"] = ux_metrics(report["frame_budget"], definition)
                if name == "title":
                    row["ux"]["first_entry"] = first_entry(directory, checkpoint, heat == "cold", row["ux"], "await" in step)
                if name == "ctrl":
                    row["fixed_segment"] = fixed_segment(directory, scenario, checkpoint)
                    if row["fixed_segment"] is None:
                        row["ctrl_segment"] = timed_ctrl_segment(directory, scenario, checkpoint) if definition.get("measurement") == "held_observation" else None
                        if row["ctrl_segment"] is None:
                            row.update(status="skipped", reason="ctrl_window_not_verified")
            except (ValueError, RuntimeError, KeyError, TypeError, AssertionError) as error:
                reason = "pcm_incomplete" if "missing PCM frames" in str(error) else "checkpoint_validation_failed"
                row.update(status="blocked", reason=reason)
                if reason == "pcm_incomplete" and report.get("frame_budget"):
                    # Retain diagnostic statistics without changing acceptance.
                    row["rejected_ux"] = ux_metrics(report["frame_budget"], definition)
        result.append(row)
    # Keep startup failures useful without promoting their frames to a reached scene.
    fallback = load_json(directory / "blocked.json") or load_json(directory / "final.json")
    if fallback and result[0]["status"] != "observed":
        result[0]["reason"] = "guest_failure" if fallback.get("failure") else "title_endpoint_not_reached"
        if fallback.get("frame_budget"):
            result[0]["startup_observation"] = summarize_budget(fallback["frame_budget"], native_rate)
    return result


def markdown(rows: list[dict]) -> str:
    text = ["# 帧预算基线", "", "无头帧提交观测；原生参照未测，未排除任何帧。热表示新进程复用发现档案；启用 code_cache 时也复用本轮冷运行产生的机器码档案。", "",
            "| 游戏 | 场景 | 冷/热 | 状态 | 超预算/帧数 | 最长墙钟 ms | 最长活跃 ms | p99 墙钟 ms | Ctrl 秒 | 主因（保留行） |",
            "|---|---|---|---|---:|---:|---:|---:|---:|---|"]
    for row in rows:
        metrics = row.get("metrics", {})
        number = lambda key: f"{metrics[key] / 1e6:.2f}" if metrics.get(key) is not None else "—"
        counts = f"{metrics['over_budget_frames']}/{metrics['frames']}" if metrics else "—"
        segment = row.get("fixed_segment") or row.get("ctrl_segment")
        elapsed = f"{segment['seconds']:.3f}" if segment else "—"
        causes = ", ".join(f"{k}:{v}" for k, v in metrics.get("cause_distribution_retained", {}).items()) or "—"
        text.append(f"| {row['game']} | {row['scene']} | {row['heat']} | {row['status']} | {counts} | {number('max_wall_ns')} | {number('max_active_ns')} | {number('p99_wall_ns')} | {elapsed} | {causes} |")
    text += ["", "## 超预算保留帧的 API 前五名", "", "只累计每帧前五名子集；次数包含 handler 与 continuation 服务段。", "",
             "| 游戏 | 场景 | 冷/热 | API | 累计 ms | 服务段数 |", "|---|---|---|---|---:|---:|"]
    for row in rows:
        for api in row.get("metrics", {}).get("top_apis_retained", []):
            text.append(f"| {row['game']} | {row['scene']} | {row['heat']} | {api['api']} | {api['ns']/1e6:.3f} | {api['calls']} |")
    text += ["", "## 原生部分估算", "", "仅 Tier 0 已确认退休指令；Tier 2 未计。使用最快合成 x64 速率的条件下界估算，不排除任何帧。", "",
             "| 游戏 | 场景 | 冷/热 | native_over_budget_estimate（保留 serial） |", "|---|---|---|---|"]
    for row in rows:
        reference = row.get("metrics", {}).get("native_reference", {})
        if reference.get("status") == "partial_estimate":
            serials = reference["over_budget_estimate_retained"]
            text.append(f"| {row['game']} | {row['scene']} | {row['heat']} | {', '.join(map(str, serials)) or '无'} |")
    text += ["", "## 体验诊断", "", "连续呈现候选段；p20 自节奏仅为估计，未经原生节奏验证。读取量为 ReadFile 返回字节，含缓存。", "",
             "| 游戏 | 场景 | 冷/热 | 连续段 | p50/p95/最长 ms | 自节奏 ms | 估计丢呈现 | 连丢≥2事件 |", "|---|---|---|---:|---|---:|---:|---:|"]
    for row in rows:
        for index, window in enumerate(row.get("ux", {}).get("animation", {}).get("windows", []), 1):
            d = window["interval_ms"]
            text.append(f"| {row['game']} | {row['scene']} | {row['heat']} | {index} | {d['p50']:.2f}/{d['p95']:.2f}/{d['max']:.2f} | {window['cadence_ms']:.2f} | {window['estimated_missed_presents']} | {window['consecutive_loss_events_at_least_two']} |")
    text += ["", "| 游戏 | 冷/热 | Ctrl 提交/s | 冷到标题门控上界 s |", "|---|---|---:|---:|"]
    for row in rows:
        segment = row.get("fixed_segment") or row.get("ctrl_segment") or {}
        first = row.get("ux", {}).get("first_entry", {})
        if row["scene"] in ("title", "ctrl"):
            rate, elapsed = segment.get("commits_per_second"), first.get("seconds")
            text.append(f"| {row['game']} | {row['heat']} | {rate:.2f}" if rate is not None else f"| {row['game']} | {row['heat']} | —")
            text[-1] += f" | {elapsed:.3f} |" if elapsed is not None else " | — |"
    return "\n".join(text) + "\n"


def summarize(root: Path) -> dict:
    plan = load_json(root / "plan.json")
    rows = []
    for scenario in plan["scenarios"]:
        for heat in plan["heats"]:
            rows += summarize_run(root / f"{scenario['id']}-{heat}", scenario, heat, plan["platform"], plan.get("native_instructions_per_second"))
    summary = {"schema": "adv-frame-budget-summary-v1", "budget_ms": 33.3,
               "execution_options": plan.get("execution_options", {}),
               "source_revision": plan["source_revision"], "source_diff_sha256": plan["source_diff_sha256"],
               "analysis_tool_sha256": digest(Path(__file__)),
               "rows": rows}
    write_json(root / "summary.json", summary)
    (root / "summary.md").write_text(markdown(rows), encoding="utf-8")
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    analyze = commands.add_parser("summarize")
    analyze.add_argument("directory", type=Path)
    args = parser.parse_args()
    summarize(args.directory)


if __name__ == "__main__":
    main()
