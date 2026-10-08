#!/usr/bin/env python3
"""Run or summarize the bounded frame-budget baseline with existing replay tools.

Usage: frame_budget.py run --platform android|windows --binary RUNNER --output DIR
       frame_budget.py summarize DIR
Inputs: ADV_SAMPLES, checked-in frame-budget-*.json scenarios, optional save/font
seeds. Android reuses replay-sample.py; Windows reuses play_session.py. Every run
uses one frozen binary, independent overlay and cold/warm discovery profiles.
Output: summary.json/summary.md contain statistics only. Reports, actions, hashes,
perf maps and failure logs stay under the explicit local artifact root. No APK,
foreground change, server management, or native Windows comparison is performed.
"""
from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import uuid

from sample_matrix import digest, write_json

REPO = Path(__file__).resolve().parents[2]
SCENARIOS = Path(__file__).with_name("scenarios")
sys.path.insert(0, str(REPO / "tools" / "android"))
from scene_schedule import load_scene


def import_tool(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


replay = import_tool("budget_replay", REPO / "tools/android/replay-sample.py")
baseline = import_tool("budget_baseline", REPO / "tools/android/summarize-baseline.py")
BUDGET_NS = 33_300_000
SCENES = ("title", "normal", "ctrl", "transition")
REASONS = ("uncovered", "rejected", "pending", "interior", "fallback", "forced")
HLE_GROUPS = ("graphics", "file", "audio", "other")
CPU_FIELDS = ("tier0_units", "tier1_units", "tier2_blocks", "native_entries", "interpreter_entries",
              "sync_compile_ns", "compile_wait_execution_ns", "compile_wait_units", "invalidations",
              "recompilations", "compilations", "x87_helpers", "softfloat_calls")


def numeric_fields(value: dict, names) -> dict:
    return {name: value[name] for name in names if type(value.get(name)) in (int, float)
            and math.isfinite(value[name]) and value[name] >= 0}


def clean_row(row: dict) -> dict:
    """Use an allowlist so future raw diagnostics cannot leak into shareable output."""
    result = numeric_fields(row, ("serial", "wall_ns", "active_ns", "guest_exec_ns", "present_call_ns",
                                 "wait_ns", "wait_overshoot_ns", "wait_timeouts", "core", "frequency_khz"))
    result.update(startup=row.get("startup") is True,
                  hle_ns=numeric_fields(row.get("hle_ns", {}), HLE_GROUPS),
                  native_reference="unmeasured")
    cpu = row.get("cpu")
    result["cpu"] = None if cpu is None else numeric_fields(cpu, CPU_FIELDS) | {
        "reasons": numeric_fields(cpu.get("reasons", {}), REASONS)}
    return result


def execution_metrics(report: dict) -> dict:
    raw = baseline.execution_metrics(report)
    tier1 = raw.get("tier1") or {}
    schedule = raw.get("compilation_schedule") or {}
    return {"tier1": numeric_fields(tier1, ("compilations", "compile_seconds", "fallback_entries",
        "fallback_instructions", "native_entries", "bridge_entries", "bridge_units", "invalidations",
        "publication_revisions", "publication_flushes", "cache_evictions", "compile_misses", "resident_bytes")),
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
    if hle:
        group, duration = max(hle.items(), key=lambda x: x[1])
        if duration >= threshold:
            candidates.append((duration, "host." + group, "measured"))
    overshoot = row.get("wait_overshoot_ns", 0)
    if overshoot >= 1_000_000 and (wall - overshoot <= BUDGET_NS or overshoot >= wall * .3):
        candidates.append((overshoot, "platform.wait_overshoot", "measured"))
    if candidates:
        _, cause, confidence = max(candidates)
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
    else:
        cause, confidence = "unattributed", "insufficient"
    for key in ("sync_compile_ns", "compile_wait_execution_ns", "compile_wait_units", "softfloat_calls",
                "interpreter_entries", "invalidations", "recompilations", "tier2_blocks"):
        evidence[key] = cpu.get(key)
    evidence.update(hle_ns=hle, wait_ns=row.get("wait_ns"), wait_overshoot_ns=overshoot,
                    core=row.get("core"), frequency_khz=row.get("frequency_khz"))
    return {"cause": cause, "confidence": confidence, "evidence": evidence}


def summarize_budget(budget: dict) -> dict:
    rows = [dict(clean_row(row), attribution=classify(row)) for row in budget["rows"]]
    over_rows = [r for r in rows if r["wall_ns"] > budget["budget_ns"]]
    return {key: budget[key] for key in (
        "frames", "over_budget_frames", "active_over_budget_frames", "max_wall_ns", "max_active_ns",
        "wall_total_ns", "active_total_ns", "p95_wall_ns", "p99_wall_ns", "p99_active_ns",
        "percentile_window", "percentile_capacity", "coalesced_frames", "observer_ns")} | {
        "retained_over_budget_frames": len(over_rows),
        "dropped_over_budget_rows": max(0, budget["over_budget_frames"] - len(over_rows)),
        "cause_distribution_retained": dict(Counter(r["attribution"]["cause"] for r in over_rows)),
        "longest": dict(clean_row(budget["longest"]), attribution=classify(budget["longest"])) if budget.get("longest") else None,
        "native_reference": {"status": "unmeasured", "excluded_frames": 0}, "rows": rows,
    }


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
            if (not replay.valid_acknowledgement(action["command"], ack)
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
                    "endpoint_hashes_matched": True, "independent_reference": False,
                    "includes_input_delivery": True}
    return None


def summarize_run(directory: Path, scenario: dict, heat: str, platform: str) -> list[dict]:
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
                row.update(status="observed", reason=None, metrics=summarize_budget(report["frame_budget"]),
                           execution=execution_metrics(report), config=baseline.report_config(report),
                           wait_diagnostics=wait_metrics(report),
                           time_skip=numeric_fields(report.get("time_skip") or {}, ("jumps", "skipped_guest_us")))
                if name == "ctrl":
                    row["fixed_segment"] = fixed_segment(directory, scenario, checkpoint)
                    if row["fixed_segment"] is None:
                        row.update(status="skipped", reason="fixed_endpoint_not_verified")
            except (ValueError, RuntimeError, KeyError, TypeError, AssertionError) as error:
                reason = "pcm_incomplete" if "missing PCM frames" in str(error) else "checkpoint_validation_failed"
                row.update(status="blocked", reason=reason)
        result.append(row)
    # Keep startup failures useful without promoting their frames to a reached scene.
    fallback = load_json(directory / "blocked.json") or load_json(directory / "final.json")
    if fallback and result[0]["status"] != "observed":
        result[0]["reason"] = "guest_failure" if fallback.get("failure") else "title_endpoint_not_reached"
        if fallback.get("frame_budget"):
            result[0]["startup_observation"] = summarize_budget(fallback["frame_budget"])
    return result


def markdown(rows: list[dict]) -> str:
    text = ["# 帧预算基线", "", "无头帧提交观测；原生参照未测，未排除任何帧。热表示新进程复用 JIT 发现档案。", "",
            "| 游戏 | 场景 | 冷/热 | 状态 | 超预算/帧数 | 最长墙钟 ms | 最长活跃 ms | p99 墙钟 ms | Ctrl 秒 | 主因（保留行） |",
            "|---|---|---|---|---:|---:|---:|---:|---:|---|"]
    for row in rows:
        metrics = row.get("metrics", {})
        number = lambda key: f"{metrics[key] / 1e6:.2f}" if metrics.get(key) is not None else "—"
        counts = f"{metrics['over_budget_frames']}/{metrics['frames']}" if metrics else "—"
        segment = row.get("fixed_segment")
        elapsed = f"{segment['seconds']:.3f}" if segment else "—"
        causes = ", ".join(f"{k}:{v}" for k, v in metrics.get("cause_distribution_retained", {}).items()) or "—"
        text.append(f"| {row['game']} | {row['scene']} | {row['heat']} | {row['status']} | {counts} | {number('max_wall_ns')} | {number('max_active_ns')} | {number('p99_wall_ns')} | {elapsed} | {causes} |")
    return "\n".join(text) + "\n"


def summarize(root: Path) -> dict:
    plan = load_json(root / "plan.json")
    rows = []
    for scenario in plan["scenarios"]:
        for heat in plan["heats"]:
            rows += summarize_run(root / f"{scenario['id']}-{heat}", scenario, heat, plan["platform"])
    summary = {"schema": "adv-frame-budget-summary-v1", "budget_ms": 33.3,
               "source_revision": plan["source_revision"], "source_diff_sha256": plan["source_diff_sha256"],
               "analysis_tool_sha256": digest(Path(__file__)),
               "rows": rows}
    write_json(root / "summary.json", summary)
    (root / "summary.md").write_text(markdown(rows), encoding="utf-8")
    return summary


@contextmanager
def device_lock(path: Path | None, root: Path, helper: Path | None = None):
    """Use the shared broker when supplied; otherwise retain legacy locking."""
    if helper is not None:
        task = os.environ.get("ADV_DEVICE_TASK", "bud")
        if not re.fullmatch(r"[a-zA-Z0-9_-]+", task):
            raise ValueError("invalid device task name")
        environment = dict(os.environ, ADV_DEVICE_HELPER=str(helper.resolve(strict=True)), ADV_DEVICE_TASK=task)
        prefix = "$ErrorActionPreference='Stop'; . $env:ADV_DEVICE_HELPER; "
        try:
            # Acquisition is idempotent for this task. A broker can briefly
            # hold owner.txt while handing over the reservation; no guest run
            # has started yet, so retrying that helper cannot duplicate work.
            for attempt in range(4):
                try:
                    subprocess.run(["powershell", "-NoProfile", "-Command",
                                    prefix + "Request-Device -Task $env:ADV_DEVICE_TASK"],
                                   env=environment, check=True)
                    break
                except subprocess.CalledProcessError:
                    if attempt == 3:
                        raise
                    time.sleep(2)
            yield time.monotonic() + 1100
        finally:
            subprocess.run(["powershell", "-NoProfile", "-Command",
                            prefix + "Release-Device -Task $env:ADV_DEVICE_TASK"],
                           env=environment, check=True, timeout=30)
        return
    if path is None:
        raise ValueError("a broker helper or legacy device lock is required")
    stamp = root / "last-lock-end.txt"
    while True:
        if stamp.exists():
            remaining = 60 - (time.time() - float(stamp.read_text()))
            if remaining > 0:
                print(f"device cooldown {remaining:.0f}s", flush=True)
                time.sleep(remaining)
        if os.name == "nt":
            quoted = str(path).replace("'", "''")
            acquired = subprocess.run(["powershell", "-NoProfile", "-Command",
                f"try {{ New-Item -ItemType Directory -Path '{quoted}' -ErrorAction Stop | Out-Null; exit 0 }} catch {{ exit 1 }}"],
                capture_output=True, timeout=15).returncode == 0
        else:
            try:
                path.mkdir()
                acquired = True
            except FileExistsError:
                acquired = False
        if acquired:
            # A previous lease can end during PowerShell startup. Recheck after
            # mkdir, release this empty directory, and cool down outside the lock.
            remaining = 60 - (time.time() - float(stamp.read_text())) if stamp.exists() else 0
            if remaining > 0:
                path.rmdir()
                time.sleep(remaining)
                continue
            break
        print("device lock occupied; retry in 30s", flush=True)
        time.sleep(30)
    owner = path / "owner.txt"
    try:
        owner.write_text("bud", encoding="utf-8")
        yield time.monotonic() + 1100
    finally:
        try:
            stamp.write_text(str(time.time()), encoding="utf-8")
        finally:
            if owner.exists() and owner.read_text(encoding="utf-8") == "bud":
                owner.unlink()
                path.rmdir()
            elif not owner.exists():
                path.rmdir()  # This invocation created it, but writing owner.txt failed.


def cleanup_remote(adb, remote: str) -> None:
    if not re.fullmatch(r"/data/local/tmp/adv-host-(?:bud|sample)-[0-9a-f]{32}", remote):
        raise ValueError("refusing cleanup outside owned session root")
    # Kill only the executable in this exact private directory, never shared runners.
    script = (f'for p in $(pidof adv-run); do [ "$(readlink /proc/$p/exe)" = "{remote}/adv-run" ] '
              f'&& kill -9 "$p"; done; rm -rf {remote}')
    adb.call("shell", script, timeout=15, idempotent=True)


def run_child(command: list[str], log: Path, timeout: float) -> int:
    with log.open("w", encoding="utf-8") as stream:
        try:
            return subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT,
                                  timeout=max(1, timeout)).returncode
        except subprocess.TimeoutExpired:
            return 124


def prepare_font(source: Path | None, remote: str, output: Path, adb) -> list[str]:
    if source is None:
        return []
    profile = load_json(source)
    path = remote + "/font-profile.json"
    adb.push(source, path)
    files = {f["path"]: f["sha256"] for f in profile["faces"]}
    manifest = output / "font-manifest.json"
    write_json(manifest, {"profile": {"path": path, "sha256": digest(source)},
                          "files": [{"path": p, "sha256": h} for p, h in files.items()]})
    return ["--font-profile", path, "--font-manifest", str(manifest)]


def deploy_game(adb, game: Path, destination: str, scenario: dict, deadline: float) -> None:
    """Push files independently so a large game does not hit one directory timeout."""
    files = replay.inventory(game)
    directories = scenario.get("deploy_directories")
    if directories is not None:
        files = [row for row in files if "/" not in row["path"] or row["path"].split("/", 1)[0] in directories]
    parents = sorted({str(Path(row["path"]).parent).replace("\\", "/") for row in files})
    for offset in range(0, len(parents), 32):
        set_transfer_deadline(adb, deadline)
        words = ["mkdir", "-p", *[destination + "/" + parent for parent in parents[offset:offset + 32]]]
        adb.call("shell", replay.shell_words(words), idempotent=True)
    for row in files:
        set_transfer_deadline(adb, deadline)
        adb.push(game / row["path"], destination + "/" + row["path"])


def set_transfer_deadline(adb, deadline: float) -> None:
    remaining = deadline - time.monotonic() - 240
    if remaining <= 90:
        raise TimeoutError("deployment exhausted this device-lock window")
    # Three transport attempts and two 30-second backoffs must fit the window.
    adb.timeout = min(120, (remaining - 60) / 3)


def preserve_undeployed(output: Path, root: Path) -> None:
    """Preserve a failed pre-launch attempt; never overwrite actual guest evidence."""
    target = output.with_name(output.name + "-undeployed-" + uuid.uuid4().hex)
    if ((output / "session.json").exists() or not output.resolve().is_relative_to(root.resolve())
            or not target.resolve().is_relative_to(root.resolve())):
        raise ValueError("refusing to replace guest evidence or move outside the run root")
    output.rename(target)


def run(args) -> None:
    if args.heats not in (["cold"], ["cold", "warm"]):
        raise ValueError("heats must be cold, or cold followed by warm")
    root = args.output.resolve()
    artifact = Path(os.environ.get("ADV_ARTIFACT_ROOT", REPO / "target")).resolve()
    resume = getattr(args, "resume", False)
    if not root.is_relative_to(artifact) or (root.exists() and not resume):
        raise ValueError("output must be a fresh directory below ADV_ARTIFACT_ROOT")
    samples = Path(os.environ["ADV_SAMPLES"]).resolve(strict=True)
    binary = args.binary.resolve(strict=True)
    root.mkdir(parents=True, exist_ok=resume)
    frozen = root / "frozen"
    frozen.mkdir(exist_ok=resume)
    if resume:
        if digest(binary) != digest(frozen / binary.name):
            raise ValueError("resume requires the original frozen runner")
    else:
        shutil.copy2(binary, frozen / binary.name)
        for dll in binary.parent.glob("*.dll"):
            shutil.copy2(dll, frozen / dll.name)
    binary = frozen / binary.name
    scenarios = []
    for name in args.games:
        path = SCENARIOS / f"frame-budget-{name}.json"
        scenario = load_json(path)
        scenario["scenario_sha256"] = digest(path)
        game = (samples / scenario["game_dir"]).resolve(strict=True)
        if not game.is_relative_to(samples):
            raise ValueError("scenario game path escapes ADV_SAMPLES")
        schedule = root / f"{name}-schedule.json"
        write_json(schedule, scenario["schedule"])
        load_scene(schedule)  # Timed-list validation remains owned by play_session.
        scenarios.append(scenario)
    diff = subprocess.check_output(["git", "diff", "HEAD"], cwd=REPO)
    import hashlib
    plan = {"platform": args.platform, "heats": args.heats, "scenarios": scenarios,
            "no_time_skip": getattr(args, "no_time_skip", False),
            "source_revision": getattr(args, "binary_source_revision", None),
            "tool_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO).decode().strip(),
            "source_diff_sha256": hashlib.sha256(diff).hexdigest()}
    if resume:
        previous = load_json(root / "plan.json")
        if (any(previous[key] != plan[key] for key in ("platform", "heats", "scenarios"))
                or any(previous.get(key, False) != plan[key] for key in ("no_time_skip",))):
            raise ValueError("resume requires unchanged scenarios and run conditions")
    else:
        write_json(root / "plan.json", plan)
    adb = replay.Adb(args) if args.platform == "android" else None
    for scenario in scenarios:
        name = scenario["id"]
        game = samples / scenario["game_dir"]
        remote = "/data/local/tmp/adv-host-bud-" + uuid.uuid4().hex
        deployed = False
        remote_created = False
        deployment_file = root / f"{name}-deployment.json"
        if resume and deployment_file.is_file():
            deployment = load_json(deployment_file)
            remote = deployment["remote"]
            if not re.fullmatch(r"/data/local/tmp/adv-host-bud-[0-9a-f]{32}", remote):
                raise ValueError("invalid owned deployment receipt")
            deployed, remote_created = deployment["complete"], True
        for heat in args.heats:
            output = root / f"{name}-{heat}"
            if resume and output.exists():
                if getattr(args, "retry_undeployed", False) and not (output / "session.json").exists():
                    preserve_undeployed(output, root)
                else:
                    print(f"{name} {heat}: preserving previous attempt", flush=True)
                    continue
            warm = root / f"{name}-cold" / ("jit-warm.bin" if adb else "overlay/jit-warm.bin")
            if heat == "warm" and not warm.is_file():
                print(f"{name} warm skipped: no cold profile", flush=True)
                continue
            command = [sys.executable, str(REPO / ("tools/android/replay-sample.py" if adb else "tools/performance/play_session.py")),
                       "--binary", str(binary), "--schedule", str(root / f"{name}-schedule.json"),
                       "--output", str(output), "--codepage", str(scenario["codepage"]), "--auto-dialog-ms", "50"]
            command += ["--cold"] if heat == "cold" else ["--warm-profile", str(warm)]
            if not args.no_instrumentation:
                command += ["--frame-budget"]
            if getattr(args, "no_time_skip", False):
                command += ["--no-time-skip"]
            if args.perf_map:
                command += ["--perf-map"]
            seed = args.seed_root / name if args.seed_root else None
            if seed and seed.is_dir():
                command += ["--seed-data", str(seed)]
            print(f"{name} {heat} starting", flush=True)
            if adb:
                helper = getattr(args, "device_helper", None)
                lease = device_lock(args.device_lock, artifact, helper) if helper else device_lock(args.device_lock, artifact)
                with lease as deadline:
                    try:
                        if not deployed:
                            if not remote_created:
                                set_transfer_deadline(adb, deadline)
                                adb.shell(["mkdir", remote])
                                remote_created = True
                                write_json(deployment_file, {"remote": remote, "complete": False})
                            deploy_game(adb, game, remote + "/game", scenario, deadline)
                            deployed = True
                            write_json(deployment_file, {"remote": remote, "complete": True})
                        set_transfer_deadline(adb, deadline)
                        command += prepare_font(args.font_profile_source, remote, root, adb)
                        command += ["--adb", str(args.adb), "--serial", args.serial,
                                    "--game-dir", str(game), "--exe-relative", scenario["exe"],
                                    "--remote-game-dir", remote + "/game", "--record-device-state",
                                    "--transfer-timeout", "120"]
                        exit_code = run_child(command, root / f"{name}-{heat}-driver.log", deadline - time.monotonic() - 240)
                    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                        exit_code = 125
                        (root / f"{name}-{heat}-transport.log").write_text(str(error), encoding="utf-8")
                    finally:
                        receipt = output / "remote-owned.txt"
                        owned = receipt.read_text(encoding="utf-8").strip() if receipt.is_file() else None
                        cleanup = [owned] if owned else []
                        if remote_created and (heat == args.heats[-1] or not (output / "jit-warm.bin").is_file()):
                            cleanup.append(remote)
                            deployed = False
                            remote_created = False
                        errors = []
                        for target in cleanup:
                            try:
                                cleanup_remote(adb, target)
                                if target == remote:
                                    deployment_file.unlink(missing_ok=True)
                            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                                errors.append({"remote": target, "error": str(error)})
                        if errors:
                            write_json(root / f"{name}-{heat}-cleanup-pending.json", errors)
            else:
                command += ["--exe", str(game / scenario["exe"]), "--mode", "headless", "--audio"]
                exit_code = run_child(command, root / f"{name}-{heat}-driver.log", 600)
            output.mkdir(exist_ok=True)
            (output / "cmd.txt").write_text(subprocess.list2cmdline(command) + "\n" +
                f"binary_sha256={digest(binary)}\nguest_exe_sha256={digest(game / scenario['exe'])}\n" +
                "environment=" + json.dumps({k: v for k, v in os.environ.items() if k.startswith("ADV_") or k in ("TEMP", "TMP", "TMPDIR")}) + "\n",
                encoding="utf-8")
            print(f"{name} {heat} exit={exit_code}", flush=True)
            summarize(root)
    summarize(root)
    print("Statistics written to summary.json and summary.md", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    analyze = commands.add_parser("summarize")
    analyze.add_argument("directory", type=Path)
    execute = commands.add_parser("run")
    execute.add_argument("--platform", choices=("android", "windows"), required=True)
    execute.add_argument("--binary", type=Path, required=True)
    execute.add_argument("--output", type=Path, required=True)
    execute.add_argument("--resume", action="store_true", help="preserve attempted runs and continue an unchanged plan")
    execute.add_argument("--retry-undeployed", action="store_true", help="with resume, retain then retry attempts that never launched a replay session")
    execute.add_argument("--binary-source-revision", help="explicit source revision of the supplied runner, if known")
    execute.add_argument("--games", nargs="+", default=["majiro", "bgi", "softpal", "kirikiri", "himawari"])
    execute.add_argument("--heats", nargs="+", choices=("cold", "warm"), default=["cold", "warm"])
    execute.add_argument("--seed-root", type=Path)
    execute.add_argument("--font-profile-source", type=Path)
    execute.add_argument("--adb", type=Path, default=Path("adb"))
    execute.add_argument("--serial", default=os.environ.get("ANDROID_SERIAL"))
    execute.add_argument("--device-lock", type=Path)
    execute.add_argument("--device-helper", type=Path, default=os.environ.get("ADV_DEVICE_HELPER"),
                         help="shared PowerShell Request-Device/Release-Device helper (task bud)")
    execute.add_argument("--perf-map", action="store_true")
    execute.add_argument("--no-time-skip", action="store_true", help="disable existing Ctrl idle-time skipping")
    execute.add_argument("--no-instrumentation", action="store_true", help="paired overhead control only")
    args = parser.parse_args()
    if args.command == "summarize":
        summarize(args.directory)
    else:
        args.adb_port, args.transfer_timeout = 5037, 120
        if args.platform == "android" and (not args.serial or not (args.device_helper or args.device_lock)):
            parser.error("Android requires --serial and --device-helper (or a legacy --device-lock); the server must already be running")
        run(args)


if __name__ == "__main__":
    main()
