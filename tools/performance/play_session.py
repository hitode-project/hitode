#!/usr/bin/env python3
"""Replay normal/held-key play through the production frontend.

The JSON schedule is an adv-scene-v1 document or a list of {"at": seconds, "command": "key-down 17"},
{"at": seconds, "checkpoint": "normal", "reset_metrics": true} or {"at": seconds, "wait": {...}} entries.
A wait polls the runner until a visible top-level window's text contains (or class equals)
{"window": TEXT}, a click at {"hit": [X, Y]} reaches a window whose text contains "text",
or {"frames": N} more frames are presented; "timeout" defaults to 30 seconds. Later steps
run at their "at" time or as soon as the wait ends, whichever is later.
Checkpoints save the actual framebuffer and frontend/device report, then optionally
start a new measurement window. Times are host time since process launch, and every
command waits for its real acknowledgement; no release may overtake its key-down.
Evidence and the Wine prefix go to --output. Wine reads and writes the original
game folder in place.
Use --audio --mute-audio --mode hidden (or window) to keep the real audio device
path active while sending silence to hardware, without enabling capture or tracing.
session.json records whether the hardware was requested to be muted.

Tracing is off by default; --trace writes api.trace for compatibility diagnosis
and marks the session unsuitable for performance comparison.
--d3d9-sm3 explicitly opts into the runner's SM3 capability profile; SM2 is the default.
--pin-runner runs a copy of the binary from OUTPUT/bin; session.json records
the SHA-256 of what actually ran.
--wine DIST supplies the required Wine distribution.
"""
from __future__ import annotations

import argparse
import ctypes
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


class _MacUsage(ctypes.Structure):
    # rusage_info_v0 from sys/resource.h; CPU times use Mach absolute ticks.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64) for name in (
            "user_time", "system_time", "idle_wakes", "interrupt_wakes", "pageins",
            "wired", "resident", "footprint", "started", "exited",
        )
    ]


def process_usage(pid):
    """Cumulative macOS process CPU and memory; None when unavailable."""
    if sys.platform != "darwin":
        return None
    function = ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage
    function.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    function.restype = ctypes.c_int
    usage = _MacUsage()
    if function(pid, 0, ctypes.byref(usage)):
        return None
    timebase = (ctypes.c_uint32 * 2)()
    basis = ctypes.CDLL("/usr/lib/libSystem.B.dylib").mach_timebase_info
    basis.argtypes = [ctypes.c_void_p]
    basis.restype = ctypes.c_int
    if basis(ctypes.byref(timebase)):
        return None
    seconds_per_tick = timebase[0] / timebase[1] / 1e9
    return {"user_seconds": usage.user_time * seconds_per_tick,
            "system_seconds": usage.system_time * seconds_per_tick,
            "resident_bytes": usage.resident, "footprint_bytes": usage.footprint}


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
        if sum(key in step for key in ("command", "checkpoint", "wait")) != 1:
            raise ValueError("each step needs exactly one command, checkpoint or wait")
        if "wait" in step:
            validate_wait(step["wait"])
            continue
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


def validate_wait(wait: dict) -> None:
    if not isinstance(wait, dict) or sum(key in wait for key in ("window", "hit", "frames")) != 1:
        raise ValueError("a wait needs exactly one of window, hit or frames")
    timeout = wait.get("timeout", 30)
    if type(timeout) not in (int, float) or not 0 < timeout <= 3600:
        raise ValueError("wait timeout must be within (0, 3600] seconds")
    if "window" in wait and (not isinstance(wait["window"], str) or not wait["window"]):
        raise ValueError("wait window needs a nonempty text fragment")
    if "hit" in wait and (not isinstance(wait["hit"], list) or len(wait["hit"]) != 2
                          or not all(type(v) is int for v in wait["hit"])
                          or not isinstance(wait.get("text", ""), str)):
        raise ValueError("wait hit needs [X, Y] and an optional text fragment")
    if "frames" in wait and (type(wait["frames"]) is not int or wait["frames"] < 1):
        raise ValueError("wait frames must be a positive count")


def wait_satisfied(wait: dict, response: str, first_frames: int | None) -> tuple[bool, int | None]:
    """Whether a `windows` or `hit` acknowledgement meets `wait`; also the frame count seen."""
    payload = json.loads(response.split(" ", 3)[3])
    if "hit" in wait:
        text = wait.get("text", "")
        return payload is not None and text in payload["text"], first_frames
    frames = payload["frames"]
    if "frames" in wait:
        start = frames if first_frames is None else first_frames
        return frames - start >= wait["frames"], start
    return any(wait["window"] in window["text"] or wait["window"] == window["class"]
               for window in payload["windows"] if window["visible"]), first_frames


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


def pin_runner(executable: Path, bin_dir: Path) -> Path:
    """Copy the runner and the dynamic libraries beside it into bin_dir."""
    bin_dir.mkdir(parents=True)
    for library in executable.parent.glob("*"):
        if library.is_file() and re.search(r"\.(dylib|so(\.\d+)*|dll)$", library.name):
            shutil.copy2(library, bin_dir / library.name)
    pinned = bin_dir / executable.name
    shutil.copy2(executable, pinned)
    return pinned


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
    if getattr(args, "pin_runner", False):
        executable = pin_runner(executable, output / "bin")
    game = args.exe.resolve(strict=True)
    waits = sum(float(step["wait"].get("timeout", 30)) for step in steps if "wait" in step)
    duration = max(15.0, float(scene["timeout_seconds"]) if scene else float(steps[-1]["at"]) + waits + args.tail_seconds)
    command = [str(executable), str(game), "--control-stdin", "--codepage", str(args.codepage),
               "--seconds", str(duration), "--auto-dialog-ms", str(getattr(args, "auto_dialog_ms", 50)),
               "--data-dir", str(output / "overlay"), "--report", str(output / "final.json"),
               "--screenshot", str(output / "final.png")]
    command.extend(["--wine", str(args.wine.resolve(strict=True)),
                    "--wine-prefix", str(output / "prefix")])
    if getattr(args, "host_profile", False):
        command.extend(["--host-profile", str(output / "host-services.csv")])
    for mount in getattr(args, "cdrom", []):
        command.extend(["--cdrom", mount])
    if getattr(args, "frame_budget", False):
        command.append("--frame-budget")
    if getattr(args, "headless_gpu", False):
        command.append("--headless-gpu")
    if getattr(args, "no_time_skip", False):
        command.append("--no-time-skip")
    if args.mode != "window":
        command.append("--" + args.mode)
    if args.audio:
        command.append("--audio")
    command.extend(["--codec-kernels", args.codec])
    if args.trace:
        command.extend(["--trace", str(output / "api.trace")])
    if getattr(args, "d3d9_sm3", False):
        command.append("--d3d9-sm3")
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

        def send(line: str, timeout: float = 30, quiet: bool = False) -> str:
            sent = time.monotonic() - start
            if process.poll() is not None:
                raise RuntimeError(f"runner exited before {line!r}: {process.returncode}")
            assert process.stdin is not None
            process.stdin.write(line + "\n")
            process.stdin.flush()
            response = acknowledgement.get(timeout=timeout)
            if not quiet:
                actions.append({"sent_seconds": sent, "ack_seconds": time.monotonic() - start,
                                "command": line, "ack": response})
                (output / "actions.json").write_text(json.dumps(actions, indent=2), encoding="utf-8")
            if not valid_acknowledgement(line, response):
                raise RuntimeError(f"command failed: {line!r}: {response}")
            if not quiet:
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
                if "wait" in step:
                    wait = step["wait"]
                    query = "hit {} {}".format(*wait["hit"]) if "hit" in wait else "windows"
                    deadline = time.monotonic() + wait.get("timeout", 30)
                    first = None
                    while True:
                        met, first = wait_satisfied(wait, send(query, quiet=True), first)
                        if met:
                            print(f"{time.monotonic() - start:.3f}s wait met {json.dumps(wait)}", flush=True)
                            break
                        if time.monotonic() > deadline:
                            raise RuntimeError(f"wait timed out: {json.dumps(wait)}")
                        time.sleep(0.25)
                elif "command" in step:
                    send(step["command"])
                else:
                    name = step["checkpoint"]
                    if scene and "await" in step:
                        gate_ack = send(await_command(step["await"]), step["await"]["timeout_ms"] / 1000 + 30)
                    usage = process_usage(process.pid)
                    if usage is not None:
                        usage["wall_seconds"] = time.monotonic() - start
                        (output / f"{name}.process.json").write_text(json.dumps(usage), encoding="utf-8")
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


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--schedule", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--wine", required=True, type=Path, help="Wine distribution; keep its prefix under OUTPUT")
    parser.add_argument("--host-profile", action="store_true", help="record host service costs under OUTPUT")
    parser.add_argument("--cdrom", action="append", default=[], metavar="DRIVE=IMAGE",
                        help="mount an image in the guest; repeat for multiple drives")
    parser.add_argument("--auto-dialog-ms", type=int, default=50,
                        help="automatic dialog delay; zero leaves dialogs pending")
    parser.add_argument("--seed-data", type=Path, help="copy an existing runtime data root into the isolated overlay")
    parser.add_argument("--mode", choices=("headless", "hidden", "window"), default="headless")
    parser.add_argument("--codepage", choices=("auto", "932", "936"), default="auto",
                        help="guest ANSI code page; auto detects it as adv-run does")
    parser.add_argument("--pin-runner", action="store_true",
                        help="run a copy of --binary (and FFmpeg libraries beside it) from OUTPUT/bin, so a rebuild cannot swap it")
    parser.add_argument("--audio", action="store_true")
    parser.add_argument("--mute-audio", action="store_true",
                        help="mute hardware output without diagnostics; requires --audio and --mode hidden/window")
    parser.add_argument("--capture-audio", action="store_true", help="capture actual callbacks and source PCM; hardware receives zeros; excludes performance comparisons")
    parser.add_argument("--trace", action="store_true", help="write API trace in the session output for compatibility diagnosis; excludes performance comparisons")
    parser.add_argument("--d3d9-sm3", action="store_true", help="enable the runner's SM3 shader capability profile")
    parser.add_argument("--codec", choices=("guest", "native"), default="native")
    parser.add_argument("--frame-budget", action="store_true")
    parser.add_argument("--headless-gpu", action="store_true")
    parser.add_argument("--no-time-skip", action="store_true")
    parser.add_argument("--perf-map", action="store_true")
    parser.add_argument("--tail-seconds", type=float, default=3)
    return parser


if __name__ == "__main__":
    parser = build_parser()
    options = parser.parse_args()
    if not math.isfinite(options.tail_seconds) or not 0 <= options.tail_seconds <= 3600:
        parser.error("tail seconds must be finite and within 0..3600")
    if options.auto_dialog_ms < 0:
        parser.error("auto dialog delay must be nonnegative")
    run(options)
