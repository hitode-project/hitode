#!/usr/bin/env python3
"""Bounded, sequential startup probes using the production ADV runner.

The manifest contains a samples list of {id, exe, engine, codepage}. Guest files
are read through the runtime VFS; each run gets a fresh write overlay. Hardware
audio is always muted. Reaching a frame is startup evidence, not playability.
Imported but uncalled APIs are recorded separately from actual failure evidence.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time


def digest(path: Path) -> str:
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            result.update(block)
    return result.hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def milestones(report: dict, evidence: dict | None = None, report_hash: str | None = None) -> dict:
    """Only an inspected title/audio/chapter can certify S2-S4, not a frame counter.

    External evidence must name the exact report digest, its reviewer and a note.
    Discovery runs keep observations but never acquire an accepted stage.
    """
    api = report.get("api") or {}
    strict = api.get("mode") == "strict"
    eligible = (strict and api.get("acceptance_eligible") is True
                and api.get("stub_calls") == 0 and not api.get("signature_conflicts")
                and not report.get("failure") and not report.get("screenshot_error"))
    startup = report.get("startup") or {}
    reached = "S1" if startup.get("loaded") and startup.get("guest_windows", 0) > 0 else None
    proof = None
    if evidence:
        if (not report_hash or evidence.get("report_sha256") != report_hash
                or not evidence.get("reviewer") or not evidence.get("note")
                or evidence.get("stage") not in ("S1", "S2", "S3", "S4")):
            raise ValueError("milestone evidence requires matching report_sha256, stage, reviewer and note")
        reached = evidence["stage"]
        proof = evidence
    return {"stage_reached": reached, "accepted_stage": reached if eligible else None,
            "acceptance_eligible": eligible, "review": proof,
            "observations": {"frames": report.get("frames", 0),
                             "first_frame_seconds": report.get("first_frame_seconds"),
                             "note": "Frames alone do not establish a title screen, correct sound or chapter completion."}}


def run(args: argparse.Namespace) -> None:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8-sig"))
    samples = manifest["samples"] if isinstance(manifest, dict) else manifest
    selected = set(args.only or [])
    evidence = json.loads(args.evidence.read_text(encoding="utf-8-sig")) if args.evidence else {}
    ids = set()
    for sample in samples:
        name = sample["id"]
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name) or name in ids:
            raise ValueError(f"invalid/duplicate sample id: {name!r}")
        ids.add(name)
        if sample.get("codepage", 932) not in (932, 936):
            raise ValueError(f"unsupported codepage for {name}")
    if selected - ids:
        raise ValueError(f"unknown sample ids: {sorted(selected - ids)}")
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    # Never place diagnostic files or overlays in a source game's directory.
    for sample in samples:
        game_dir = Path(sample["exe"]).resolve().parent
        if output == game_dir or game_dir in output.parents:
            raise ValueError("output must be outside all game directories")
    output.mkdir(parents=True, exist_ok=False)
    environment = {k: v for k, v in os.environ.items()
                   if not k.startswith(("ADV_AUDIO_", "ADV_PRINTF_DIAG_")) and k not in ("ADV_MMIO_TRACE", "ADV_JIT_DUMP_DIR")}
    environment["ADV_AUDIO_MUTE_DEVICE"] = "1"
    metadata = {"binary": str(binary), "binary_sha256": digest(binary),
                "manifest": str(args.manifest.resolve()), "seconds_per_sample": args.seconds,
                "mode": args.mode, "api_mode": args.api_mode, "codec": args.codec, "hardware_muted": True,
                "auto_dialog_ms": 50, "qualification": "startup probe, not playability or peak throughput",
                "samples": []}
    write_json(output / "results.json", metadata)
    for sample in samples:
        if selected and sample["id"] not in selected:
            continue
        folder = output / sample["id"]
        folder.mkdir()
        game = Path(sample["exe"]).resolve(strict=True)
        command = [str(binary), str(game), "--" + args.mode, "--audio",
                   "--codec", args.codec, "--api-mode", args.api_mode, "--seconds", str(args.seconds),
                   "--codepage", str(sample.get("codepage", 932)),
                   "--auto-dialog-ms", "50", "--data-dir", str(folder / "overlay"),
                   "--report", str(folder / "report.json"),
                   "--screenshot", str(folder / "frame.png")]
        if args.trace:
            command += ["--trace", str(folder / "api.log")]
        result = dict(sample, command=command, guest_exe_sha256=digest(game))
        result["milestones"] = milestones({})
        started = time.monotonic()
        print(f"START {sample['id']}", flush=True)
        try:
            with (folder / "stdout.log").open("wb") as stdout, (folder / "stderr.log").open("wb") as stderr:
                process = subprocess.Popen(command, stdout=stdout, stderr=stderr,
                                           stdin=subprocess.DEVNULL, env=environment,
                                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                try:
                    result["exit_code"] = process.wait(timeout=args.seconds + args.timeout_grace)
                    result["external_timeout"] = False
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    result.update(exit_code=process.returncode, external_timeout=True)
        except OSError as error:
            result["launch_error"] = str(error)
        result["wall_seconds"] = round(time.monotonic() - started, 6)
        report_path = folder / "report.json"
        if report_path.exists():
            try:
                report = json.loads(report_path.read_text(encoding="utf-8"))
                for key in ("stop_reason", "guest_exit_code", "frames", "first_frame_seconds",
                            "width", "height", "frame_fnv1a64", "missing_apis", "failure",
                            "screenshot_error", "audio", "pcm", "codec", "performance", "api", "startup"):
                    result[key] = report.get(key)
                result["report_sha256"] = digest(report_path)
                result["milestones"] = milestones(report, evidence.get(sample["id"]), result["report_sha256"])
            except (ValueError, OSError) as error:
                result["report_error"] = str(error)
        stderr = (folder / "stderr.log").read_text(encoding="utf-8", errors="replace")
        result["stderr_tail"] = stderr[-12000:]
        if result.get("external_timeout"):
            state = "external_timeout"
        elif result.get("failure") or result.get("exit_code") != 0:
            state = "blocked"
        elif not result.get("frames"):
            state = "no_frame"
        else:
            state = "frames_observed"
        result["status"] = state
        # missing_apis includes unresolved imports: it does not prove execution.
        failure_evidence = result.get("failure") or stderr
        blocker = re.search(r"unimplemented:\s+([A-Za-z0-9_.-]+\.dll![A-Za-z0-9_@?#.$-]+)", failure_evidence)
        result["blocker_api"] = blocker.group(1) if blocker else None
        result["failure_api_evidence"] = sorted(set(re.findall(
            r"[A-Za-z0-9_.-]+\.dll![A-Za-z0-9_@?#.$-]+", failure_evidence)))
        metadata["samples"].append(result)
        write_json(folder / "probe.json", result)
        write_json(output / "results.json", metadata)
        print(f"END {sample['id']}: {state}, frames={result.get('frames', 0)}, "
              f"wall={result['wall_seconds']:.2f}s", flush=True)
        if result.get("failure"):
            print("  " + result["failure"].splitlines()[0], flush=True)
        elif state == "blocked" and result.get("guest_exit_code") is not None:
            print(f"  guest exited with code {result['guest_exit_code']}; see api.log and report.json", flush=True)
    rows = ["# Startup probes", "", "Sequential, bounded, hardware-muted runs. Frame production does not certify playability.",
            "Unresolved imports and actual runtime failures must be evaluated separately.", "",
            "| Sample | Engine evidence | Result | Reached | Accepted | Frames | First frame (s) | Called API blocker |", "|---|---|---|---|---|---:|---:|---|"]
    for result in metadata["samples"]:
        clean = lambda value: str(value).replace("|", "\\|").replace("\n", " ")
        rows.append("| " + " | ".join(map(clean, [result["id"], result.get("engine", "unknown"),
                    result["status"], result["milestones"]["stage_reached"] or "unverified",
                    result["milestones"]["accepted_stage"] or "none", result.get("frames", 0),
                    result.get("first_frame_seconds"), result.get("blocker_api") or "see failure evidence"])) + " |")
    (output / "summary.md").write_text("\n".join(rows) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--only", nargs="+")
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--timeout-grace", type=float, default=15)
    parser.add_argument("--mode", choices=("hidden", "headless"), default="hidden")
    parser.add_argument("--codec", choices=("native", "guest"), default="native")
    parser.add_argument("--api-mode", choices=("strict", "discovery"), default="strict")
    parser.add_argument("--evidence", type=Path, help="reviewed S1-S4 evidence keyed by sample id, tied to report SHA-256")
    parser.add_argument("--trace", action="store_true", help="diagnostic API trace, not a performance measurement")
    options = parser.parse_args()
    if not math.isfinite(options.seconds) or not 0 < options.seconds <= 3600:
        parser.error("seconds must be finite and within 0..3600")
    if not math.isfinite(options.timeout_grace) or not 1 <= options.timeout_grace <= 120:
        parser.error("timeout grace must be finite and within 1..120")
    run(options)
