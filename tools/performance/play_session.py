#!/usr/bin/env python3
"""Replay normal/held-key play through the production frontend.

The JSON schedule is an adv-scene-v1 document (shared with Android replay) or a list of {"at": seconds, "command": "key-down 17"} or
{"at": seconds, "checkpoint": "normal", "reset_metrics": true} entries.
Checkpoints save the actual framebuffer and frontend/device report, then optionally
start a new measurement window. Times are host time since process launch, and every
command waits for its real acknowledgement; no release may overtake its key-down.
All guest writes and evidence go to --output. Game assets are never modified.
Use --audio --mute-audio --mode hidden (or window) to keep the real audio device
path active while sending silence to hardware, without enabling capture or tracing.
session.json records whether the hardware was requested to be muted.

Tracing is off by default; --trace writes api.trace for compatibility diagnosis
and marks the session unsuitable for performance comparison.
--d3d9-sm2/--d3d9-sm3 explicitly opt into the runner's shader capability profiles.
Native caching/static discovery are default; --no-code-cache diagnoses failures.
--warm-code-cache
imports a previous local archive into the new overlay. --cold clears both caches.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import sys
import threading
import time


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "android"))
from scene_schedule import load_scene, await_command, assert_report, valid_acknowledgement


def load_schedule(path: Path) -> list[dict]:
    steps = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(steps, list) or not steps:
        raise ValueError("schedule must be a nonempty list")
    previous = 0.0
    names = set()
    for step in steps:
        at = step.get("at")
        if (type(at) not in (int, float) or not math.isfinite(at)
                or at < previous or at > 86400):
            raise ValueError("schedule times must be finite, nonnegative and ordered")
        previous = at
        if ("command" in step) == ("checkpoint" in step):
            raise ValueError("each step needs exactly one command or checkpoint")
        if "command" in step:
            command = step["command"]
            if not isinstance(command, str) or not command.strip() or "\n" in command or "\r" in command:
                raise ValueError("command must be a single nonempty line")
        else:
            name = step["checkpoint"]
            if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name) or name in names:
                raise ValueError("checkpoint names must be unique filename-safe names")
            names.add(name)
    return steps


def launch_environment(args: argparse.Namespace, output: Path) -> tuple[dict[str, str], list[str]]:
    """Scrub inherited diagnostics, then explicitly enable requested audio modes."""
    mute_audio = getattr(args, "mute_audio", False)
    if mute_audio and (not args.audio or args.mode == "headless"):
        raise ValueError("--mute-audio requires --audio and --mode hidden/window")
    if args.capture_audio and (not args.audio or args.mode == "headless"):
        raise ValueError("--capture-audio requires --audio and --mode hidden/window")
    # Diagnostics can substantially change decoder throughput and scheduling.
    environment = dict(os.environ)
    removed = []
    for name in ("ADV_AUDIO_TRACE", "ADV_AUDIO_DUMP_DIR", "ADV_PRINTF_DIAG_PATH", "ADV_PRINTF_DIAG_WORDS", "ADV_AUDIO_DUMP_CHUNKS", "ADV_AUDIO_CAPTURE", "ADV_AUDIO_CAPTURE_CALLBACK", "ADV_AUDIO_MUTE_DEVICE", "ADV_MMIO_TRACE", "ADV_JIT_DUMP_DIR"):
        if environment.pop(name, None) is not None:
            removed.append(name)
    environment.pop("ADV_PERF_MAP_DIR", None)
    if getattr(args, "compile_threads", None):
        environment["ADV_COMPILE_THREADS"] = str(args.compile_threads)
    if getattr(args, "perf_map", False):
        environment["ADV_PERF_MAP_DIR"] = str(output)
    if mute_audio or args.capture_audio:
        environment["ADV_AUDIO_MUTE_DEVICE"] = "1"
    if args.capture_audio:
        environment.update(ADV_AUDIO_CAPTURE_CALLBACK=str(output / "actual.f32"),
                           ADV_AUDIO_TRACE=str(output / "sound.log"),
                           ADV_AUDIO_DUMP_DIR=str(output / "source-dumps"),
                           ADV_AUDIO_DUMP_CHUNKS="1",
                           ADV_MMIO_TRACE=str(output / "mmio.log"))
    return environment, removed


def preserve_failure(send, output: Path) -> None:
    """Capture a frozen failed gate before close handlers change the scene."""
    send("key-up 17")
    send(f"report {output / 'blocked.json'}")
    send(f"capture {output / 'blocked.png'}")


def run(args: argparse.Namespace) -> None:
    scene = load_scene(args.schedule)
    steps = scene["steps"] if scene else load_schedule(args.schedule)
    output = args.output.resolve()
    environment, removed = launch_environment(args, output)
    output.mkdir(parents=True, exist_ok=False)
    if args.seed_data:
        # Copy an existing data root, never let acceptance mutate a user's save.
        shutil.copytree(args.seed_data.resolve(strict=True), output / "overlay",
                        ignore=shutil.ignore_patterns("*.lock"))
    executable = args.binary.resolve(strict=True)
    game = args.exe.resolve(strict=True)
    duration = max(15.0, float(scene["timeout_seconds"]) if scene else float(steps[-1]["at"]) + args.tail_seconds)
    command = [str(executable), str(game), "--control-stdin", "--codepage", str(args.codepage),
               "--seconds", str(duration), "--auto-dialog-ms", str(getattr(args, "auto_dialog_ms", 50)),
               "--data-dir", str(output / "overlay"), "--report", str(output / "final.json"),
               "--screenshot", str(output / "final.png")]
    for mount in getattr(args, "cdrom", []):
        command.extend(["--cdrom", mount])
    if getattr(args, "frame_budget", False):
        command.append("--frame-budget")
    if getattr(args, "no_code_cache", False):
        command.append("--no-code-cache")
    if getattr(args, "headless_gpu", False):
        command.append("--headless-gpu")
    if getattr(args, "no_time_skip", False):
        command.append("--no-time-skip")
    if args.mode != "window":
        command.append("--" + args.mode)
    if args.audio:
        command.append("--audio")
    command.extend(["--codec", args.codec])
    command.extend(["--guest-workers", str(args.guest_workers)])
    if args.sync_compilation:
        command.append("--sync-compilation")
    if args.no_tiering:
        command.append("--no-tiering")
    if args.trace:
        command.extend(["--trace", str(output / "api.trace")])
    for feature in ("d3d9_sm2", "d3d9_sm3"):
        if getattr(args, feature, False):
            command.append("--" + feature.replace("_", "-"))
    if getattr(args, "cold", False):
        (output / "overlay" / "jit-warm.bin").unlink(missing_ok=True)
        (output / "overlay" / "jit-code.bin").unlink(missing_ok=True)
    if getattr(args, "warm_profile", None):
        (output / "overlay").mkdir(exist_ok=True)
        shutil.copy2(args.warm_profile, output / "overlay" / "jit-warm.bin")
    if getattr(args, "warm_code_cache", None):
        if getattr(args, "cold", False):
            raise ValueError("cold replay cannot import native cache")
        (output / "overlay").mkdir(exist_ok=True)
        shutil.copy2(args.warm_code_cache, output / "overlay" / "jit-code.bin")
    acknowledgement: queue.Queue[str | None] = queue.Queue()
    actions = []
    metadata = {"command": command, "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                "guest_exe_sha256": hashlib.sha256(game.read_bytes()).hexdigest(),
                "schedule": steps, "removed_diagnostics": removed,
                "audio_device_expected": args.audio and args.mode != "headless",
                "muted": getattr(args, "mute_audio", False) or args.capture_audio,
                "diagnostic_capture": args.capture_audio,
                "diagnostic_api_trace": args.trace,
                "suitable_for_performance_comparison": not (args.capture_audio or args.trace)}
    (output / "session.json").write_text(json.dumps(metadata, indent=2, ensure_ascii=False), encoding="utf-8")
    start = time.monotonic()
    with (output / "stdout.log").open("w", encoding="utf-8") as stdout, \
            (output / "stderr.log").open("w", encoding="utf-8") as stderr:
        process = subprocess.Popen(command, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=stderr, text=True, encoding="utf-8", errors="strict",
                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))

        def receive() -> None:
            assert process.stdout is not None
            try:
                for line in process.stdout:
                    stdout.write(line)
                    stdout.flush()
                    if line.startswith("control "):
                        acknowledgement.put(line.strip())
            finally:
                acknowledgement.put(None)

        reader = threading.Thread(target=receive, daemon=True)
        reader.start()

        def send(line: str, timeout: float = 30) -> str:
            sent = time.monotonic() - start
            if process.poll() is not None:
                raise RuntimeError(f"runner exited before {line!r}: {process.returncode}")
            assert process.stdin is not None
            process.stdin.write(line + "\n")
            process.stdin.flush()
            response = acknowledgement.get(timeout=timeout)
            actions.append({"sent_seconds": sent, "ack_seconds": time.monotonic() - start,
                            "command": line, "ack": response})
            (output / "actions.json").write_text(json.dumps(actions, indent=2), encoding="utf-8")
            if not valid_acknowledgement(line, response):
                raise RuntimeError(f"command failed: {line!r}: {response}")
            print(f"{sent:.3f}s {response}", flush=True)
            return response

        try:
            for step in steps:
                due = time.monotonic() - start if scene else step["at"]
                if "delay_ms" in step:
                    send("resume")
                    due += step["delay_ms"] / 1000
                while time.monotonic() - start < due:
                    if process.poll() is not None:
                        raise RuntimeError(f"runner exited at {time.monotonic() - start:.3f}s: {process.returncode}")
                    time.sleep(min(0.05, max(0.001, due - (time.monotonic() - start))))
                if "delay_ms" in step:
                    continue
                if "command" in step:
                    send(step["command"])
                else:
                    name = step["checkpoint"]
                    if scene and "await" in step:
                        gate_ack = send(await_command(step["await"]), step["await"]["timeout_ms"] / 1000 + 30)
                    report_path = output / f"{name}.json"
                    send(f"report {report_path}")
                    report = json.loads(report_path.read_text(encoding="utf-8"))
                    if report.get("failure") or not report.get("frames"):
                        raise RuntimeError(f"checkpoint {name} has no valid presented frame")
                    if scene and "await" in step:
                        if report.get("frame_fnv1a64") != step["await"]["frame_fnv1a64"]:
                            raise RuntimeError(f"checkpoint {name} hash mismatch")
                        fields = dict(token.split("=", 1) for token in gate_ack.split()[3:] if "=" in token)
                        gate = report.get("scene_gate", {})
                        if report["frames"] != int(fields["frame"]) or gate.get("frozen") is not True or gate.get("pending") is not False:
                            raise RuntimeError(f"checkpoint {name} differs from frozen receipt")
                        assert_report(report, step.get("assert", {}), name)
                    device = report.get("audio", {})
                    if metadata["audio_device_expected"]:
                        if args.capture_audio and (not device.get("hardware_muted") or device.get("capture_dropped", 0)):
                            raise RuntimeError(f"checkpoint {name} is not a complete muted callback capture")
                        if not device.get("device_requested") or device.get("device_errors", 0):
                            raise RuntimeError(f"checkpoint {name} has an audio device failure")
                        # Pausing the device between voices or in a silent scene is
                        # intentional. A schedule can require continuous sound at
                        # specific known BGM/voice points instead of assuming it.
                        if step.get("expect_audio") and not device.get("device_running"):
                            raise RuntimeError(f"checkpoint {name} expected active audio")
                    send(f"capture {output / (name + '.png')}")
                    if step.get("reset_metrics", False):
                        send("reset-metrics")
            if scene and steps[-1].get("command", "").strip() != "close":
                send("close")
            result = process.wait(timeout=max(5, duration - (time.monotonic() - start) + 10))
            if result:
                raise RuntimeError(f"runner failed: {result}; see stderr.log and final.json")
            report = json.loads((output / "final.json").read_text(encoding="utf-8"))
            if report.get("failure") or report.get("screenshot_error"):
                raise RuntimeError(f"failed final report: {report.get('failure')} {report.get('screenshot_error')}")
            if metadata["audio_device_expected"] and not report.get("audio", {}).get("non_silent_samples", 0):
                raise RuntimeError("real audio session produced no non-silent device samples")
            metadata["completed"] = True
        except BaseException as error:
            metadata["completed"] = False
            metadata["suitable_for_performance_comparison"] = False
            metadata["error"] = str(error)
            if process.poll() is None:
                try:
                    preserve_failure(send, output)
                except (RuntimeError, OSError, queue.Empty) as capture_error:
                    metadata["failure_capture_error"] = type(capture_error).__name__
            raise
        finally:
            if process.poll() is None:
                try:
                    assert process.stdin is not None
                    process.stdin.write("key-up 17\nclose\n")
                    process.stdin.flush()
                    process.wait(timeout=5)
                except (OSError, subprocess.TimeoutExpired):
                    process.kill()
                    process.wait()
            reader.join(timeout=5)
            metadata.update(exit_code=process.returncode, wall_seconds=time.monotonic() - start)
            (output / "session.json").write_text(json.dumps(metadata, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--schedule", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cdrom", action="append", default=[], metavar="DRIVE=IMAGE",
                        help="mount an image in the guest; repeat for multiple drives")
    parser.add_argument("--auto-dialog-ms", type=int, default=50,
                        help="automatic dialog delay; zero leaves dialogs pending")
    parser.add_argument("--seed-data", type=Path, help="copy an existing runtime data root into the isolated overlay")
    parser.add_argument("--guest-workers", type=int, choices=(1, 2), default=1)
    parser.add_argument("--mode", choices=("headless", "hidden", "window"), default="headless")
    parser.add_argument("--codepage", choices=(932, 936), type=int, default=936)
    parser.add_argument("--audio", action="store_true")
    parser.add_argument("--mute-audio", action="store_true",
                        help="mute hardware output without diagnostics; requires --audio and --mode hidden/window")
    parser.add_argument("--capture-audio", action="store_true", help="capture actual callbacks and source PCM; hardware receives zeros; excludes performance comparisons")
    parser.add_argument("--trace", action="store_true", help="write API trace in the session output for compatibility diagnosis; excludes performance comparisons")
    parser.add_argument("--d3d9-sm2", action="store_true", help="enable the runner's SM2 shader capability profile")
    parser.add_argument("--d3d9-sm3", action="store_true", help="enable the runner's SM3 shader capability profile")
    parser.add_argument("--codec", choices=("guest", "native"), default="native")
    parser.add_argument("--frame-budget", action="store_true")
    parser.add_argument("--headless-gpu", action="store_true")
    parser.add_argument("--no-time-skip", action="store_true")
    parser.add_argument("--perf-map", action="store_true")
    warmth = parser.add_mutually_exclusive_group()
    warmth.add_argument("--cold", action="store_true")
    warmth.add_argument("--warm-profile", type=Path)
    parser.add_argument("--warm-code-cache", type=Path)
    parser.add_argument("--no-code-cache", action="store_true", help="diagnostic bypass of native caching and image precompilation")
    parser.add_argument("--compile-threads", type=int, choices=range(1, 9))
    parser.add_argument("--sync-compilation", action="store_true")
    parser.add_argument("--no-tiering", action="store_true",
                        help="disable Fast-to-Speed promotion for matched worker comparisons")
    parser.add_argument("--tail-seconds", type=float, default=3)
    options = parser.parse_args()
    if not math.isfinite(options.tail_seconds) or not 0 <= options.tail_seconds <= 3600:
        parser.error("tail seconds must be finite and within 0..3600")
    if options.auto_dialog_ms < 0:
        parser.error("auto dialog delay must be nonnegative")
    run(options)
