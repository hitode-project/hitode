#!/usr/bin/env python3
"""Summarize validated scene baselines, preserving each run's own percentiles.

Pass cold/warm replay directories for ONE sample, binary and runtime configuration.
Warm runs must use the same explicit warm archive; cold runs must have none.
Output files stay under ignored target/. Percentiles are copied from reports,
never averaged or subtracted. Wall/frozen observations are diagnostics, not FPS.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from scene_schedule import assert_report, await_command, digest_json, exact_hash, validate_reference

REPO = Path(__file__).resolve().parents[2]
ACTIVE = "warm_active_runtime_between_frame_observations"
GLOBAL_COUNTERS = ("frames", "compilations", "compile_seconds", "compile_requests",
                   "compile_failures", "stale_compilations", "idle_waits", "idle_wait_seconds",
                   "host_calls", "native_entries", "interpreter_entries", "input_messages")
WINDOW_COUNTERS = ("runtime_steps", "runtime_soft_yields", "runtime_step_seconds", "frame_observations",
                   "guest_serial_skipped_between_observations", "wait_requested_seconds",
                   "wait_deadline_wakes", "wait_input_wakes")
GATE_COUNTERS = ("arms", "matches", "frame_checks", "full_hash_checks", "check_seconds",
                 "hash_seconds", "frozen_observation_seconds")
REPORT_CONFIG = ("backend", "compile_strategy", "region_linking", "tiered_compilation", "async_compilation", "codepage")
REQUIRED_FILES = ("final.json", "final.png", "stderr.log", "remote-stdout.log", "exit-code.txt", "actions.json", "jit-warm.bin")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_json(path: Path) -> object:
    return json.loads(path.read_text(encoding="utf-8"))


def number(value: object, name: str, *, integer: bool = False) -> int | float:
    require(type(value) in ((int,) if integer else (int, float)) and math.isfinite(value) and value >= 0,
            f"{name}: expected a finite nonnegative {'integer' if integer else 'number'}")
    return value


def counters(report: dict, names: tuple, label: str) -> dict:
    return {name: number(report.get(name), f"{label}.{name}") for name in names}


def delta(current: dict, previous: dict | None, label: str) -> dict:
    result = {}
    for name, value in current.items():
        difference = value - (previous[name] if previous is not None else 0)
        require(difference >= -1e-9, f"{label}.{name}: cumulative counter decreased")
        result[name] = max(0, difference)
    return result


def distribution(value: object, label: str) -> dict:
    require(isinstance(value, dict), f"{label}: missing distribution")
    count = number(value.get("samples"), f"{label}.samples", integer=True)
    window = number(value.get("percentile_window_samples"), f"{label}.window", integer=True)
    capacity = number(value.get("percentile_window_capacity"), f"{label}.capacity", integer=True)
    require(0 < capacity and window == min(count, capacity), f"{label}: inconsistent percentile window")
    number(value.get("total_ms"), f"{label}.total_ms")
    for name in ("mean_ms", "p50_ms", "p95_ms", "p99_ms", "max_ms"):
        if count:
            number(value.get(name), f"{label}.{name}")
        else:
            require(value.get(name) is None, f"{label}: empty distribution must have null {name}")
    if count:
        require(value["p50_ms"] <= value["p95_ms"] <= value["p99_ms"] <= value["max_ms"],
                f"{label}: inconsistent percentile ordering")
    return {**value, "percentiles_cover": "all samples" if count <= capacity else "last window only",
            "max_covers": "all samples since this metric reset"}


def valid_report(report: object, label: str) -> dict:
    require(isinstance(report, dict), f"{label}: report must be an object")
    require(not report.get("failure") and not report.get("screenshot_error"), f"{label}: guest/capture failure")
    require(type(report.get("frames")) is int and report["frames"] > 0, f"{label}: no presented frame")
    api = report.get("api", {})
    require(isinstance(api, dict) and api.get("mode") == "strict" and api.get("acceptance_eligible") is True
            and api.get("stub_calls") == 0 and not api.get("signature_conflicts"), f"{label}: strict ABI failed")
    require(isinstance(report.get("audio"), dict) and report["audio"].get("device_requested") is False
            and report["audio"].get("device_running") is False, f"{label}: expected virtual audio")
    require(isinstance(report.get("pcm"), dict) and report["pcm"].get("missing_stream_frames") == 0
            and report["pcm"].get("missing_output_frames") == 0, f"{label}: missing PCM frames")
    return report


def report_config(report: dict) -> dict:
    require(all(name in report for name in REPORT_CONFIG), "incomplete runtime configuration")
    require(all(type(report[name]) is bool for name in ("region_linking", "tiered_compilation", "async_compilation")),
            "invalid runtime booleans")
    require(isinstance(report.get("codec"), dict) and report["codec"].get("provider") in ("guest", "native"),
            "missing codec provider")
    require(isinstance(report.get("codec_kernels"), dict) and report["codec_kernels"].get("mode") in ("guest", "native"),
            "Verify or unknown kernel mode cannot be a performance baseline")
    return {name: report[name] for name in REPORT_CONFIG} | {
        "codec_provider": report["codec"]["provider"], "codec_kernel_mode": report["codec_kernels"]["mode"],
        "codec_kernels_installed": report["codec_kernels"].get("installed"), "workers": 1,
        "time_skip_enabled": report.get("time_skip", {}).get("enabled"),
        "compile_threads": report.get("compilation_schedule", {}).get("compile_threads"),
        "available_cores": report.get("compilation_schedule", {}).get("available_cores")}


def execution_metrics(report: dict) -> dict:
    """Keep aggregate execution counters and compiler histograms per observation."""
    cliffs = report.get("cpu_cliffs", {})
    return {"compilation_schedule": report.get("compilation_schedule"),
            "interpreter": cliffs.get("interpreter"), "interpreter_details": cliffs.get("interpreter_details")}


def condition_identity(metadata: dict) -> dict | None:
    before, after = metadata.get("device_conditions_before"), metadata.get("device_conditions_after")
    require(not metadata.get("device_conditions_error"), "device condition capture failed")
    if before is None and after is None:
        return None
    require(isinstance(before, dict) and isinstance(after, dict), "incomplete device conditions")
    fixed = ("screen", "foreground", "allowed_cpus")
    for state in (before, after):
        require(all(isinstance(state.get(key), str) and state[key] for key in fixed), "missing display/CPU state")
        number(state.get("battery_temperature_c"), "battery temperature")
    require(all(before[key] == after[key] for key in fixed), "screen/foreground/CPU state changed during run")
    return {key: before[key] for key in fixed}


def command_option(command: list, name: str) -> str:
    require(command.count(name) == 1, f"session command needs one {name}")
    index = command.index(name) + 1
    require(index < len(command) and isinstance(command[index], str), f"invalid session {name}")
    return command[index]


def load_run(directory: Path) -> dict:
    directory = directory.resolve(strict=True)
    session_path = directory / "session.json"
    metadata = read_json(session_path)
    require(isinstance(metadata, dict), "session must be an object")
    for name in ("completed", "evidence_complete", "fixed_workload_verified", "suitable_for_performance_comparison",
                 "remote_executable_hashes_verified", "remote_asset_seed_hashes_verified", "remote_font_hashes_verified",
                 "warm_profile_input_verified"):
        require(metadata.get(name) is True, f"{name} is not true")
    require(metadata.get("scene_validation_status") == "validated", "scene is not validated")
    require(not metadata.get("error") and not metadata.get("evidence_errors") and not metadata.get("missing_required_evidence"),
            "session has incomplete or failed evidence")
    require(metadata.get("remote_exit_code") == 0 and metadata.get("adb_exit_code") == 0, "session exit was not zero")
    require(metadata.get("font_diagnostics") is False and metadata.get("capture_pcm") is False,
            "diagnostic font/PCM I/O is ineligible for performance")
    require(metadata.get("context") == "adb-shell", "expected adb-shell session")
    for name in REQUIRED_FILES:
        require((directory / name).is_file(), f"missing {name}")
    require((directory / "exit-code.txt").read_text(encoding="utf-8").strip() == "0", "actual remote exit was not zero")
    for name, field in (("final.json", "final_report_sha256"), ("final.png", "final_png_sha256"),
                        ("jit-warm.bin", "warm_output_sha256")):
        require(sha256(directory / name) == exact_hash(metadata.get(field), 64), f"{name}: recorded hash differs")
    binding = metadata.get("scene_binding")
    require(isinstance(binding, dict) and all(key in binding for key in
            ("guest_exe_sha256", "game_files", "seed_files", "font_manifest", "codepage", "scene_sha256")),
            "incomplete scene binding")
    require(binding["guest_exe_sha256"] == exact_hash(metadata.get("guest_exe_sha256"), 64), "guest hash differs from binding")
    require(binding["font_manifest"] == metadata.get("font_manifest"), "font manifest differs from binding")
    require(binding["seed_files"] == [row for row in metadata.get("seed_files", []) if row.get("path") != "jit-warm.bin"],
            "seed differs from binding")
    require(binding["game_files"] == metadata.get("game_files"), "asset inventory differs from binding")
    reference_path = Path(metadata.get("scene_reference", ""))
    require(reference_path.is_file(), "approved reference file is missing")
    reference_sha = exact_hash(metadata.get("scene_reference_sha256"), 64)
    require(sha256(reference_path) == reference_sha, "approved reference hash differs")
    steps = metadata.get("schedule")
    require(isinstance(steps, list), "missing scene steps")
    reference = read_json(reference_path)
    validate_reference(reference, binding, steps)
    warmth = metadata.get("warm_mode")
    require(warmth in ("cold", "warm"), "use explicit --cold or --warm-profile; seed mode is ambiguous")
    warm_input = metadata.get("warm_input_sha256")
    if warmth == "cold":
        require(warm_input is None, "cold run has a warm input")
    else:
        warm_input = exact_hash(warm_input, 64)
    actions = read_json(directory / "actions.json")
    require(isinstance(actions, list) and all(isinstance(row, dict) for row in actions), "invalid actions evidence")
    resets = []
    for index, action in enumerate(actions):
        if action.get("command") == "reset-metrics":
            match = re.fullmatch(r"control ok reset-metrics start_frame=(\d+)", action.get("ack", ""))
            require(match is not None, "reset-metrics has no actual acknowledgement")
            resets.append((index, int(match[1])))
    final = valid_report(read_json(directory / "final.json"), "final")
    require(final.get("guest_exit_code") == 0 and final.get("stop_reason") == "guest_exit", "final guest exit was not normal")
    config = report_config(final)
    command = metadata.get("command")
    require(isinstance(command, list) and all(isinstance(word, str) for word in command), "invalid runner command")
    config.update(codec_requested=command_option(command, "--codec"),
                  codec_kernels_requested=command_option(command, "--codec-kernels"),
                  cpu_mask_requested=command[1] if len(command) > 2 and command[0] == "taskset" else None)
    require(config["codec_kernels_requested"] == config["codec_kernel_mode"], "requested/reported kernel mode differs")
    require(config["codepage"] == binding["codepage"], "report codepage differs from binding")
    checkpoints = metadata.get("checkpoints")
    expected_steps = [step for step in steps if "checkpoint" in step]
    require(isinstance(checkpoints, list) and [row.get("name") for row in checkpoints] ==
            [step["checkpoint"] for step in expected_steps], "checkpoint sequence differs from scene")
    previous_global = previous_gate = previous_window = None
    previous_epoch = None
    previous_action = -1
    rows = []
    for entry, step in zip(checkpoints, expected_steps, strict=True):
        name = entry["name"]
        require(re.fullmatch(r"[A-Za-z0-9_-]+", name) is not None, "invalid checkpoint name")
        report_path, png_path = directory / (name + ".json"), directory / (name + ".png")
        require(sha256(report_path) == exact_hash(entry.get("report_sha256"), 64), f"{name}: report hash differs")
        require(sha256(png_path) == exact_hash(entry.get("png_sha256"), 64), f"{name}: PNG hash differs")
        report = valid_report(read_json(report_path), name)
        require(report_config(report) == {key: value for key, value in config.items()
                if key not in ("codec_requested", "codec_kernels_requested", "cpu_mask_requested")}, f"{name}: runtime configuration changed")
        require(report.get("frame_fnv1a64") == step["await"]["frame_fnv1a64"] == entry.get("frame_fnv1a64")
                and report["frames"] == entry.get("frames"), f"{name}: checkpoint frame/hash differs")
        assert_report(report, step.get("assert", {}), name)
        assert_report(report, reference["checkpoints"][name].get("assert", {}), name)
        gate = report.get("scene_gate")
        require(isinstance(gate, dict) and gate.get("frozen") is True and gate.get("pending") is False
                and not gate.get("failure"), f"{name}: no frozen successful gate")
        fields = dict(token.split("=", 1) for token in entry.get("gate_ack", "").split()[3:] if "=" in token)
        require(fields.get("hash") == report["frame_fnv1a64"] and fields.get("frame") == str(report["frames"])
                and fields.get("frozen") == "true", f"{name}: gate ACK differs")
        require(entry.get("gate_ack", "").startswith("control ok await-frame "), f"{name}: gate ACK failed")
        start_frame = int(fields.get("start_frame", "-1"))
        require(start_frame >= 0 and (step["await"].get("current") or report["frames"] > start_frame),
                f"{name}: gate did not observe a new required frame")
        release = step["await"].get("release_key", step["await"].get("hold_key"))
        require(fields.get("release") == (str(release) if release is not None else "none")
                and (release is None or fields.get("release_posted") == "true"), f"{name}: gate key release differs")
        target = await_command(step["await"])
        action_index = next((i for i in range(previous_action + 1, len(actions)) if
                             actions[i].get("command") == target and actions[i].get("ack") == entry["gate_ack"]), None)
        require(action_index is not None, f"{name}: gate ACK missing from actual actions")
        previous_action = action_index
        before = [(index, frame) for index, frame in resets if index < action_index]
        epoch = len(before)
        expected_start = before[-1][1] if before else 0
        performance = report.get("performance")
        require(isinstance(performance, dict) and performance.get("measurement_start_frame") == expected_start,
                f"{name}: measurement epoch differs from reset acknowledgements")
        active = distribution(performance.get(ACTIVE), f"{name}.active")
        wait = distribution(performance.get("wait_deadline_overshoot"), f"{name}.wait")
        wall = distribution(performance.get("warm_wall_time_between_frame_observations"), f"{name}.wall")
        global_now = counters(report, GLOBAL_COUNTERS, name)
        gate_now = counters(gate, GATE_COUNTERS, f"{name}.gate")
        window_now = counters(performance, WINDOW_COUNTERS, f"{name}.window")
        row = {"checkpoint": name, "frame_fnv1a64": report["frame_fnv1a64"],
               "report_sha256": entry["report_sha256"], "png_sha256": entry["png_sha256"],
               "metrics_epoch": epoch, "measurement_start_frame": expected_start,
               "active_frame_interval": active, "wait_deadline_overshoot": wait,
               "wall_frame_interval_diagnostic_only": wall,
               "process_counters": global_now, "process_counter_delta": delta(global_now, previous_global, name),
               "window_counters": window_now,
               "window_counter_delta": delta(window_now, previous_window if epoch == previous_epoch else None, name),
               "window_delta_scope": "since previous checkpoint" if epoch == previous_epoch else "since metric reset/startup",
               "gate_counters": gate_now, "gate_counter_delta": delta(gate_now, previous_gate, f"{name}.gate"),
               "gate_hash_max_seconds": number(gate.get("hash_max_seconds"), f"{name}.gate.hash_max"),
               "pending_compilations": number(report.get("pending_compilations"), f"{name}.pending_compilations"),
               "codec": report["codec"], "codec_kernels": report["codec_kernels"],
               "measurement_elapsed_seconds_diagnostic_only": performance.get("measurement_elapsed_seconds"),
               "execution_metrics": execution_metrics(report)}
        rows.append(row)
        previous_global, previous_gate, previous_window, previous_epoch = global_now, gate_now, window_now, epoch
    require(sum(row["active_frame_interval"]["samples"] for row in rows) > 0, "no active frame interval samples")
    require(final["frames"] >= rows[-1]["process_counters"]["frames"], "final frame counter decreased")
    identity = {"binary_sha256": exact_hash(metadata.get("binary_sha256"), 64), "scene_binding": binding,
                "scene_reference_sha256": reference_sha, "schedule_sha256": exact_hash(metadata.get("schedule_sha256"), 64),
                "runtime_config": config, "device": metadata.get("device"), "serial": metadata.get("serial"),
                "device_conditions": condition_identity(metadata)}
    return {"run_directory": str(directory), "run_name": directory.name, "warm_mode": warmth,
            "warm_input_sha256": warm_input, "warm_output_sha256": metadata["warm_output_sha256"],
            "session_sha256": sha256(session_path), "actions_sha256": sha256(directory / "actions.json"),
            "final_report_sha256": metadata["final_report_sha256"], "final_png_sha256": metadata["final_png_sha256"],
            "identity": identity, "checkpoints": rows,
            "final_process_counters": counters(final, GLOBAL_COUNTERS, "final"),
            "final_scene_gate": final.get("scene_gate"), "first_frame_seconds": final.get("first_frame_seconds"),
            "final_execution_metrics": execution_metrics(final),
            "device_conditions_before": metadata.get("device_conditions_before"),
            "device_conditions_after": metadata.get("device_conditions_after")}


def summarize(directories: list[Path]) -> dict:
    require(bool(directories), "supply at least one run")
    require(len({str(path.resolve()) for path in directories}) == len(directories), "duplicate run directory")
    runs = []
    for directory in directories:
        try:
            runs.append(load_run(directory))
        except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
            raise ValueError(f"{directory}: {error}") from error
    identity = runs[0]["identity"]
    require(all(run["identity"] == identity for run in runs), "runs differ in scene binding, binary, reference, device or runtime config")
    groups = []
    for mode in ("cold", "warm"):
        selected = [run for run in runs if run["warm_mode"] == mode]
        if not selected:
            continue
        inputs = {run["warm_input_sha256"] for run in selected}
        require(len(inputs) == 1, f"{mode} runs mix different warm archive inputs")
        groups.append({"warm_mode": mode, "repeat_count": len(selected), "warm_input_sha256": next(iter(inputs)),
                       "runs": [{key: value for key, value in run.items() if key != "identity"} for run in selected]})
    return {"schema": "adv-scene-baseline-summary-v1", "eligible": True, "identity": identity,
            "identity_sha256": digest_json(identity), "groups": groups,
            "interpretation": ["Every p95/p99 is a single run's reported percentile; no pooled percentile or average is produced.",
                "Percentiles cover the report's recent bounded window; max covers all samples since reset.",
                "A checkpoint distribution is cumulative since its metric reset, not a percentile of the delta to the previous checkpoint.",
                "Wall and frozen observation time are diagnostics and are never converted to FPS or workload throughput.",
                "Compile counters can include background compilation during frozen observation; they are not guest execution time.",
                "Cold/warm denotes the input guest-code profile; these runs still compile ARM code anew."]}


def table(summary: dict) -> str:
    if not summary.get("eligible"):
        return "Baseline rejected: " + "; ".join(summary.get("evidence_errors", [])) + "\n"
    lines = ["Per-run checkpoint observations; p95/p99 are not pooled. Wall/freeze time is not FPS.",
             "", "| mode | run | checkpoint | epoch | active samples/window | active p95/p99/max ms | compile delta s | idle delta s | gate checks/hash delta | gate check delta ms | frozen delta s |",
             "|---|---|---|---:|---|---|---:|---:|---|---:|---:|"]
    for group in summary["groups"]:
        for run in group["runs"]:
            for row in run["checkpoints"]:
                active, process, gate = row["active_frame_interval"], row["process_counter_delta"], row["gate_counter_delta"]
                values = "/".join("-" if active[key] is None else f"{active[key]:.3f}" for key in ("p95_ms", "p99_ms", "max_ms"))
                name = run["run_name"].replace("|", "\\|")
                lines.append(f"| {group['warm_mode']} | {name} | {row['checkpoint']} | {row['metrics_epoch']} | "
                             f"{active['samples']}/{active['percentile_window_samples']} | {values} | {process['compile_seconds']:.6f} | "
                             f"{process['idle_wait_seconds']:.6f} | {gate['frame_checks']}/{gate['full_hash_checks']} | "
                             f"{gate['check_seconds'] * 1000:.3f} | {gate['frozen_observation_seconds']:.6f} |")
    return "\n".join(lines) + "\n"


def save(path: Path, content: str) -> None:
    root = Path(os.environ.get("ADV_ARTIFACT_ROOT", REPO / "target")).resolve()
    require(path.resolve().is_relative_to(root), "output must stay inside ADV_ARTIFACT_ROOT (default: ignored target/)")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("runs", type=Path, nargs="+")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--table", type=Path, help="optional readable Markdown table inside target/")
    args = parser.parse_args()
    try:
        result = summarize(args.runs)
        save(args.output, json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n")
        rendered = table(result)
        if args.table:
            save(args.table, rendered)
        print(rendered, end="")
        return 0
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        result = {"schema": "adv-scene-baseline-summary-v1", "eligible": False, "evidence_errors": [str(error)]}
        print(table(result), file=sys.stderr, end="")
        try:
            save(args.output, json.dumps(result, ensure_ascii=False, indent=2) + "\n")
            if args.table:
                save(args.table, table(result))
        except (OSError, ValueError) as output_error:
            print(f"Cannot save failed summary: {output_error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
