#!/usr/bin/env python3
"""Replay normal/held-key play through the production frontend, with tracing off.

The JSON schedule is a list of {"at": seconds, "command": "key-down 17"} or
{"at": seconds, "checkpoint": "normal", "reset_metrics": true} entries.
Checkpoints save the actual framebuffer and frontend/device report, then optionally
start a new measurement window. Times are host time since process launch, and every
command waits for its real acknowledgement; no release may overtake its key-down.
All guest writes and evidence go to --output. Game assets are never modified.
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
import subprocess
import threading
import time


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


def run(args: argparse.Namespace) -> None:
    steps = load_schedule(args.schedule)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    executable = args.binary.resolve(strict=True)
    game = args.exe.resolve(strict=True)
    # Diagnostics can substantially change decoder throughput and scheduling.
    environment = dict(os.environ)
    removed = []
    for name in ("ADV_AUDIO_TRACE", "ADV_AUDIO_DUMP_DIR", "ADV_PRINTF_DIAG_PATH", "ADV_PRINTF_DIAG_WORDS", "ADV_AUDIO_DUMP_CHUNKS", "ADV_AUDIO_CAPTURE", "ADV_AUDIO_CAPTURE_CALLBACK", "ADV_AUDIO_MUTE_DEVICE", "ADV_MMIO_TRACE", "ADV_JIT_DUMP_DIR"):
        if environment.pop(name, None) is not None:
            removed.append(name)
    if args.capture_audio:
        if not args.audio or args.mode == "headless":
            raise ValueError("--capture-audio requires --audio and --mode hidden/window")
        environment.update(ADV_AUDIO_MUTE_DEVICE="1",
                           ADV_AUDIO_CAPTURE_CALLBACK=str(output / "actual.f32"),
                           ADV_AUDIO_TRACE=str(output / "sound.log"),
                           ADV_AUDIO_DUMP_DIR=str(output / "source-dumps"),
                           ADV_AUDIO_DUMP_CHUNKS="1",
                           ADV_MMIO_TRACE=str(output / "mmio.log"))
    duration = max(15.0, float(steps[-1]["at"]) + args.tail_seconds)
    command = [str(executable), str(game), "--control-stdin", "--codepage", str(args.codepage),
               "--seconds", str(duration), "--auto-dialog-ms", "50",
               "--data-dir", str(output / "overlay"), "--report", str(output / "final.json"),
               "--screenshot", str(output / "final.png")]
    if args.mode != "window":
        command.append("--" + args.mode)
    if args.audio:
        command.append("--audio")
    command.extend(["--codec", args.codec])
    if args.sync_compilation:
        command.append("--sync-compilation")
    acknowledgement: queue.Queue[str | None] = queue.Queue()
    actions = []
    metadata = {"command": command, "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                "guest_exe_sha256": hashlib.sha256(game.read_bytes()).hexdigest(),
                "schedule": steps, "removed_diagnostics": removed,
                "audio_device_expected": args.audio and args.mode != "headless",
                "diagnostic_capture": args.capture_audio,
                "suitable_for_performance_comparison": not args.capture_audio}
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

        def send(line: str) -> None:
            sent = time.monotonic() - start
            if process.poll() is not None:
                raise RuntimeError(f"runner exited before {line!r}: {process.returncode}")
            assert process.stdin is not None
            process.stdin.write(line + "\n")
            process.stdin.flush()
            response = acknowledgement.get(timeout=30)
            actions.append({"sent_seconds": sent, "ack_seconds": time.monotonic() - start,
                            "command": line, "ack": response})
            (output / "actions.json").write_text(json.dumps(actions, indent=2), encoding="utf-8")
            if response is None or not response.startswith("control ok "):
                raise RuntimeError(f"command failed: {line!r}: {response}")
            print(f"{sent:.3f}s {response}", flush=True)

        try:
            for step in steps:
                while time.monotonic() - start < step["at"]:
                    if process.poll() is not None:
                        raise RuntimeError(f"runner exited at {time.monotonic() - start:.3f}s: {process.returncode}")
                    time.sleep(min(0.05, max(0.001, step["at"] - (time.monotonic() - start))))
                if "command" in step:
                    send(step["command"])
                else:
                    name = step["checkpoint"]
                    report_path = output / f"{name}.json"
                    send(f"report {report_path}")
                    report = json.loads(report_path.read_text(encoding="utf-8"))
                    if report.get("failure") or not report.get("frames"):
                        raise RuntimeError(f"checkpoint {name} has no valid presented frame")
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
            result = process.wait(timeout=max(5, duration - (time.monotonic() - start) + 10))
            if result:
                raise RuntimeError(f"runner failed: {result}; see stderr.log and final.json")
            report = json.loads((output / "final.json").read_text(encoding="utf-8"))
            if report.get("failure") or report.get("screenshot_error"):
                raise RuntimeError(f"failed final report: {report.get('failure')} {report.get('screenshot_error')}")
            if metadata["audio_device_expected"] and not report.get("audio", {}).get("non_silent_samples", 0):
                raise RuntimeError("real audio session produced no non-silent device samples")
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
    parser.add_argument("--mode", choices=("headless", "hidden", "window"), default="headless")
    parser.add_argument("--codepage", choices=(932, 936), type=int, default=936)
    parser.add_argument("--audio", action="store_true")
    parser.add_argument("--capture-audio", action="store_true", help="capture actual callbacks and source PCM; hardware receives zeros; excludes performance comparisons")
    parser.add_argument("--codec", choices=("guest", "native"), default="native")
    parser.add_argument("--sync-compilation", action="store_true")
    parser.add_argument("--tail-seconds", type=float, default=3)
    options = parser.parse_args()
    if not math.isfinite(options.tail_seconds) or not 0 <= options.tail_seconds <= 3600:
        parser.error("tail seconds must be finite and within 0..3600")
    run(options)
