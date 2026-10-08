#!/usr/bin/env python3
"""Bounded, sequential startup probes using the production ADV runner.

The manifest contains a samples list of {id, exe, engine, codepage}. Guest files
are read through the runtime VFS; each run gets a fresh write overlay. Hardware
audio is always muted. Reaching a frame is startup evidence, not playability.
Imported but uncalled APIs are recorded separately from actual failure evidence.
Optional manifest native_baseline evidence names exe_sha256, outcome, source,
reviewer and note; codepage may be absent/unknown, otherwise it must match.
--quick runs a regression gate of one sample per engine: the samples marked
"quick": true, or else the first sample of each engine, 15 seconds each.
Relative entries are resolved under ADV_SAMPLES; min_seconds can extend a known
slow startup. --warm-from reuses only validated JIT archives in fresh overlays.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
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


API_TOKEN = r"[A-Za-z0-9_.-]+\.dll![A-Za-z0-9_@?#.$:-]+"
API_MENTION = re.compile(API_TOKEN, re.IGNORECASE)
STUB_FAILURE = re.compile(r"unimplemented:\s+(" + API_TOKEN + r")", re.IGNORECASE)
ERROR_MESSAGE = re.compile(
    r"\berror\b|\bfatal\b|\bfailed\b|\bfailure\b|\bcannot\b|\bcould not\b|"
    r"\bunable\b|\bmissing\b|\bnot found\b|\bunimplemented\b|\bunsupported\b|"
    r"\bnot supported\b|错误|錯誤|无法|無法|失败|失敗|不存在|找不到|エラー|ありません|存在しません",
    re.IGNORECASE,
)
FALLBACK_MESSAGE = re.compile(
    r"\busing (?:a |the )?fallback\b|\bfalling back\b|\bcontinuing\b|"
    r"\bwill continue\b|\boptional probe\b|\bno errors?\b", re.IGNORECASE,
)


def _count(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def _api_name(module: object, name: object) -> str | None:
    if (not isinstance(module, str) or not isinstance(name, str) or not name
            or not re.fullmatch(r"[A-Za-z0-9_.-]+\.dll", module, re.IGNORECASE)):
        return None
    return module.lower() + "!" + name


def validate_native_baseline(sample: dict, exe_hash: str | None = None) -> dict | None:
    """A native observation belongs to one entry executable, never an engine family.

    A user's native playability statement does not supply an ACP/locale. Missing,
    null and 'unknown' codepages remain unknown rather than inheriting guest 932.
    """
    baseline = sample.get("native_baseline")
    if baseline is None:
        return None
    if not isinstance(baseline, dict):
        raise ValueError("native_baseline must be an object")
    expected = exe_hash or sample.get("guest_exe_sha256") or sample.get("sha256")
    supplied = baseline.get("exe_sha256")
    if (not isinstance(supplied, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", supplied)
            or not isinstance(expected, str) or supplied.lower() != expected.lower()):
        raise ValueError("native_baseline requires the matching entry exe_sha256")
    if baseline.get("outcome") not in ("playable", "startup_failed", "unknown"):
        raise ValueError("native_baseline outcome must be playable, startup_failed or unknown")
    if any(not isinstance(baseline.get(key), str) or not baseline[key].strip()
           for key in ("source", "reviewer", "note")):
        raise ValueError("native_baseline requires source, reviewer and note")
    codepage = baseline.get("codepage")
    if codepage not in (None, "unknown"):
        if _count(codepage) is None or codepage != sample.get("codepage", 932):
            raise ValueError("a supplied native_baseline codepage must match the guest codepage")
    return dict(baseline, exe_sha256=supplied.lower())


def _review(evidence: dict | None, report_hash: str | None) -> dict | None:
    if not evidence:
        return None
    expectation = evidence.get("exit_expectation")
    valid_exit = (isinstance(expectation, dict)
                  and expectation.get("kind") in ("requested", "reviewed")
                  and _count(expectation.get("guest_exit_code")) is not None)
    stage = evidence.get("stage")
    valid_stage = stage in ("S1", "S2", "S3", "S4")
    if (not report_hash or evidence.get("report_sha256") != report_hash
            or not evidence.get("reviewer") or not evidence.get("note")
            or (stage is not None and not valid_stage) or not (valid_stage or valid_exit)
            or ("exit_expectation" in evidence and not valid_exit)):
        raise ValueError("review evidence requires matching report_sha256, stage or exit_expectation, reviewer and note")
    return evidence


def diagnostic_completeness(api: dict) -> dict:
    """Total counters and bounded recent records are deliberately different facts."""
    version = _count(api.get("discovery_diagnostics_version"))
    truncated = _count(api.get("truncated_diagnostic_fields"))
    result = {"version": version, "truncated_fields": truncated}
    for label, array_key, count_key, dropped_key in (
        ("dynamic_lookups", "dynamic_lookup_failures", "dynamic_lookup_failure_count", "dropped_dynamic_lookup_failures"),
        ("message_boxes", "message_boxes", "message_box_count", "dropped_message_boxes"),
    ):
        records = api.get(array_key)
        observed = len(records) if isinstance(records, list) else None
        total = _count(api.get(count_key))
        dropped = _count(api.get(dropped_key))
        missing = [key for key in (array_key, count_key, dropped_key) if key not in api]
        consistent = (observed is not None and total is not None and dropped is not None
                      and total == observed + dropped)
        result[label] = {"collected": version is not None and version >= 1,
                         "observed_records": observed, "total": total, "dropped_records": dropped,
                         "missing_fields": missing, "counters_consistent": consistent,
                         "complete": bool(version is not None and version >= 1 and consistent
                                          and dropped == 0 and truncated == 0)}
    recent = api.get("recent_stubs")
    total = _count(api.get("stub_calls"))
    observed = len(recent) if isinstance(recent, list) else None
    result["called_stubs"] = {"total": total, "recent_records": observed,
                              "recent_history_complete": total is not None and total == observed,
                              "aggregated_calls_available": isinstance(api.get("called_gaps"), dict)}
    return result


def _called_stubs(api: dict) -> list[dict]:
    calls = {}
    for name, count in (api.get("called_gaps") or {}).items():
        if isinstance(name, str) and _count(count) and "!" in name:
            calls[name] = {"api": name, "count": count, "recent_calls": []}
    for recent in api.get("recent_stubs") or []:
        if not isinstance(recent, dict):
            continue
        name = recent.get("api") or _api_name(recent.get("module"), recent.get("name"))
        if not isinstance(name, str):
            continue
        calls.setdefault(name, {"api": name, "count": None, "recent_calls": []})["recent_calls"].append(recent)
    return [calls[name] for name in sorted(calls)]


def _is_error_message(box: dict) -> bool:
    text = str(box.get("caption") or "") + "\n" + str(box.get("text") or "")
    if FALLBACK_MESSAGE.search(text):
        return False
    style = _count(box.get("style")) or 0
    return style & 0xf0 == 0x10 or bool(ERROR_MESSAGE.search(text))


def _matching_lookups(box: dict, failures: list[dict]) -> list[dict]:
    text = str(box.get("caption") or "") + "\n" + str(box.get("text") or "")
    context, sequence = _count(box.get("context")), _count(box.get("sequence"))
    if context is None or sequence is None:
        return []
    snapshot = box.get("preceding_lookup_failure")
    history = failures + ([snapshot] if isinstance(snapshot, dict) else [])
    mentioned_modules = {name.lower() for name in re.findall(r"[A-Za-z0-9_.-]+\.dll", text, re.IGNORECASE)}
    matches = {}
    for failure in history:
        if (not isinstance(failure, dict) or failure.get("kind") != "unimplemented"
                or _count(failure.get("context")) != context
                or _count(failure.get("sequence")) is None or failure["sequence"] >= sequence):
            continue
        requested = failure.get("name")
        # An ordinal/unknown export is not evidence of a known unsupported API.
        if not isinstance(requested, str) or not re.fullmatch(r"[A-Za-z_?][A-Za-z0-9_@?$.-]*", requested):
            continue
        name = failure.get("failure_name") or requested
        module = failure.get("failure_module") or failure.get("module")
        api = _api_name(module, name)
        if not api or not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_?][A-Za-z0-9_@?$.-]*", name):
            continue
        if mentioned_modules and not mentioned_modules.intersection(
                {str(module).lower(), str(failure.get("module")).lower()}):
            continue
        named = [symbol for symbol in {name, requested} if re.search(
            r"(?<![A-Za-z0-9_@?$])" + re.escape(symbol) + r"(?![A-Za-z0-9_@?$])", text)]
        if named:
            matches[(api, failure["sequence"])] = dict(failure, api=api, mentioned_symbols=sorted(named))
    return list(matches.values())


def _runtime_failure_kind(failure: str) -> str:
    first = failure.splitlines()[0].lower()
    if any(term in first for term in ("exception dispatch", "seh", "nonlocal unwind", "exception handler")):
        return "seh_dispatch"
    if any(term in first for term in ("0xc0000005", "access violation", "memfault", "unmapped", "memory fault")):
        return "cpu_access_violation"
    if (re.search(r"(?:unsupported|unimplemented|unrecognized) (?:guest )?instruction", first)
            or any(term in first for term in ("invalid opcode", "0xc000001d", "decode instruction"))):
        return "unsupported_instruction"
    if "unhandled windows exception" in first:
        return "cpu_exception"
    if any(term in first for term in ("watchdog", "execution budget", "instruction budget")):
        return "execution_budget"
    if any(term in first for term in ("park address", "fetch at zero", "invalid instruction pointer")):
        return "cpu_control_flow"
    if "cpu fault" in first:
        return "cpu_fault"
    return "runtime_failure"


def analyze_report(report: dict, stderr: str = "", *, sample: dict | None = None,
                   review: dict | None = None, report_hash: str | None = None) -> dict:
    """Classify observed execution without treating an optional GPA probe as a call.

    Native provenance and review explain a comparison/exit; neither manufactures
    missing APIs, playback, healthy progress or a CPU crash from a frame count.
    """
    api = report.get("api") or {}
    called = _called_stubs(api)
    lookups = [entry for entry in api.get("dynamic_lookup_failures") or [] if isinstance(entry, dict)]
    completeness = diagnostic_completeness(api)
    frames = _count(report.get("frames")) or 0
    messages = []
    for box in api.get("message_boxes") or []:
        if isinstance(box, dict):
            event_frame = _count(box.get("frame"))
            after = frames - event_frame if event_frame is not None and frames >= event_frame else None
            # Three later presentation serials are explicit counter progression,
            # not proof of correct pixels, playback, or recovery of every feature.
            later_frames = after >= 3 if after is not None else None
            messages.append(dict(box, is_error=_is_error_message(box),
                                 matching_unimplemented_queries=_matching_lookups(box, lookups),
                                 subsequent_frames_observed=after,
                                 later_frame_progression=later_frames))
    proof = _review(review, report_hash)
    expectation = proof.get("exit_expectation") if proof else None
    confirmed_exit = (report.get("stop_reason") == "guest_exit" and expectation is not None
                      and expectation["guest_exit_code"] == report.get("guest_exit_code"))
    elapsed = report.get("elapsed_seconds")
    rate = (frames / elapsed if isinstance(elapsed, (float, int)) and math.isfinite(elapsed) and elapsed > 0 else None)
    low_frames = frames <= 1 or (rate is not None and elapsed >= 5 and rate < 1)
    native = validate_native_baseline(sample) if sample else None
    result = {"blocker_api": None, "blocker_evidence": None, "called_stubs": called,
              "dynamic_lookup_candidates": [dict(entry, api=_api_name(
                  entry.get("failure_module") or entry.get("module"), entry.get("failure_name") or entry.get("name"))) for entry in lookups],
              "engine_messages": messages, "failure_kind": None,
              "error_history": [box for box in messages if box["is_error"]
                                and (box["later_frame_progression"] is True or confirmed_exit)],
              "diagnostic_completeness": completeness,
              "observations": {"low_frame_observation": low_frames, "frame_rate_observed": rate,
                               "interpretation": "undetermined", "review_required": False,
                               "note": "Frame counts do not establish correct playback or explain low frame production."},
              "native_baseline": native,
              "failure_api_evidence": sorted(set(API_MENTION.findall(report.get("failure") or ""))),
              "failure_api_evidence_kind": "text_mentions_only"}
    failure = report.get("failure") or ""
    stop = report.get("stop_reason")
    stub = STUB_FAILURE.search(failure.splitlines()[0]) if failure else None
    actual_stub = bool(stub and (any(call["api"].lower() == stub.group(1).lower() for call in called)
                                or "strict ABI stub" in failure.splitlines()[0]
                                or "stub_calls" not in api))
    terminal_messages = [box for box in messages if box["is_error"] and box["later_frame_progression"] is not True]
    terminal_error = stop == "guest_exit" and bool(terminal_messages)
    if report.get("external_timeout"):
        result.update(status="external_timeout", failure_kind="external_timeout")
    elif report.get("launch_error"):
        result.update(status="launch_error", failure_kind="launch_error")
    elif report.get("report_error"):
        result.update(status="diagnostic_error", failure_kind="invalid_report")
    elif failure:
        result.update(status="blocked", failure_kind="called_stub" if actual_stub else _runtime_failure_kind(failure))
        if actual_stub:
            result.update(blocker_api=stub.group(1), blocker_evidence={"kind": "called_stub", "failure": failure.splitlines()[0]})
    elif confirmed_exit:
        result.update(status="expected_exit" if expectation["kind"] == "requested" else "reviewed_exit")
    elif terminal_error:
        result.update(status="startup_error" if frames <= 1 else "engine_error_exit",
                      failure_kind="engine_startup_error" if frames <= 1 else "engine_error")
        named = [match for box in terminal_messages for match in box["matching_unimplemented_queries"]]
        names = {match["api"] for match in named}
        if len(names) == 1 and completeness["truncated_fields"] == 0:
            result.update(blocker_api=next(iter(names)), blocker_evidence={
                "kind": "message_box_named_unimplemented_lookup", "queries": named,
                "note": "The terminal engine error names a prior known unsupported query in the same context."})
    elif stop == "guest_exit":
        result.update(status="startup_exit" if frames <= 1 else "early_exit",
                      failure_kind="guest_exit_unreviewed")
        result["observations"].update(review_required=True,
            note="Guest exit needs review; its code and frame count alone do not identify a crash or API blocker.")
    elif stop == "time_limit":
        result["status"] = "frames_observed" if frames else "no_frame"
        result["observations"].update(review_required=low_frames,
            note="Time limit reached. Low frame production remains undiagnosed." if low_frames else "Frames were observed within the time limit; playability remains unverified.")
    elif stop in ("host_window_closed", "control_close", "frame_limit"):
        result["status"] = "requested_stop"
    elif report.get("report_available") is False:
        result.update(status="diagnostic_error", failure_kind="missing_report")
    elif report.get("exit_code") not in (None, 0) and not report.get("screenshot_error"):
        result.update(status="blocked", failure_kind="host_process_failure")
    else:
        result["status"] = "frames_observed" if frames > 1 else "unverified_outcome"
        result["observations"]["review_required"] = True
    if native:
        outcome = native["outcome"]
        result["native_comparison"] = ("guest_startup_differs_from_playable_native"
            if outcome == "playable" and result["status"] in ("startup_exit", "startup_error")
            else "native_startup_also_failed" if outcome == "startup_failed" and result["status"] in ("startup_exit", "startup_error")
            else "not_established")
    completeness["stderr_tail_truncated"] = len(stderr) > 12000
    result["unstructured_message_mentions"] = [line for line in stderr.splitlines() if re.search(r"\bMessageBox(?:Ex)?[AW]\b", line)]
    return result


def milestones(report: dict, evidence: dict | None = None, report_hash: str | None = None) -> dict:
    """Only an inspected title/audio/chapter can certify S2-S4, not a frame counter.

    External evidence must name the exact report digest, its reviewer and a note.
    Discovery runs keep observations but never acquire an accepted stage.
    """
    api = report.get("api") or {}
    strict = api.get("mode") == "strict"
    startup_error = analyze_report(report, review=evidence, report_hash=report_hash)["status"] == "startup_error"
    eligible = (strict and api.get("acceptance_eligible") is True
                and api.get("stub_calls") == 0 and not api.get("signature_conflicts")
                and not report.get("failure") and not report.get("screenshot_error") and not startup_error)
    startup = report.get("startup") or {}
    reached = "S1" if startup.get("loaded") and startup.get("guest_windows", 0) > 0 else None
    proof = _review(evidence, report_hash)
    if proof and proof.get("stage"):
        reached = proof["stage"]
    return {"stage_reached": reached, "accepted_stage": reached if eligible else None,
            "acceptance_eligible": eligible, "review": proof,
            "observations": {"frames": report.get("frames", 0),
                             "first_frame_seconds": report.get("first_frame_seconds"),
                             "note": "Frames alone do not establish a title screen, correct sound or chapter completion."}}


def quick_samples(samples: list[dict]) -> set[str]:
    """Ids of the quick gate: the marked samples, else the first of each engine."""
    marked = {sample["id"] for sample in samples if sample.get("quick") is True}
    if marked:
        return marked
    first = {}
    for sample in samples:
        first.setdefault(sample.get("engine", "unknown"), sample["id"])
    return set(first.values())


def sample_path(sample: dict) -> Path:
    path = Path(sample["exe"])
    if path.is_absolute():
        return path.resolve(strict=True)
    root = Path(os.environ["ADV_SAMPLES"]).resolve(strict=True)
    path = (root / path).resolve(strict=True)
    if not path.is_relative_to(root):
        raise ValueError("sample path escapes ADV_SAMPLES")
    return path


def import_warm_archives(source: Path, folder: Path, sample: dict, binary_hash: str, guest_hash: str) -> dict:
    """Keep guest state fresh and reject archives from another measured input."""
    previous = json.loads((source / "results.json").read_text(encoding="utf-8-sig"))
    row = next((r for r in previous["samples"] if r["id"] == sample["id"]), None)
    if (previous["binary_sha256"] != binary_hash or row is None
            or row["guest_exe_sha256"] != guest_hash
            or row.get("codepage", 932) != sample.get("codepage", 932)):
        raise ValueError("warm input does not match the runner, guest image and codepage")
    origin = (source / sample["id"] / "overlay").resolve(strict=True)
    if not origin.is_relative_to(source.resolve(strict=True)):
        raise ValueError("warm overlay escapes its run root")
    overlay = folder / "overlay"
    overlay.mkdir()
    inputs = {}
    for name in ("jit-warm.bin", "jit-code.bin"):
        archive = origin / name
        if not archive.is_file() or not archive.resolve().is_relative_to(origin):
            raise ValueError("warm input is missing an owned JIT archive")
        inputs[name] = digest(archive)
        shutil.copy2(archive, overlay / name)
    return inputs


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
        seconds = sample.get("min_seconds", 0)
        if type(seconds) not in (int, float) or not math.isfinite(seconds) or not 0 <= seconds <= 3600:
            raise ValueError(f"invalid minimum startup duration for {name}")
    if selected - ids:
        raise ValueError(f"unknown sample ids: {sorted(selected - ids)}")
    if getattr(args, "quick", False):
        selected = (selected or ids) & quick_samples(samples)
        if not selected:
            raise ValueError("--only names no quick-gate samples")
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    # Never place diagnostic files or overlays in a source game's directory.
    for sample in samples:
        game_dir = sample_path(sample).parent
        if output == game_dir or game_dir in output.parents:
            raise ValueError("output must be outside all game directories")
    output.mkdir(parents=True, exist_ok=False)
    environment = {k: v for k, v in os.environ.items()
                   if not k.startswith(("ADV_AUDIO_", "ADV_PRINTF_DIAG_")) and k not in ("ADV_MMIO_TRACE", "ADV_JIT_DUMP_DIR")}
    environment["ADV_AUDIO_MUTE_DEVICE"] = "1"
    metadata = {"binary": str(binary), "binary_sha256": digest(binary),
                "manifest": str(args.manifest.resolve()), "seconds_per_sample": args.seconds,
                "quick_gate": bool(getattr(args, "quick", False)),
                "mode": args.mode, "api_mode": args.api_mode, "codec": args.codec, "hardware_muted": True,
                "auto_dialog_ms": 50, "qualification": "startup probe, not playability or peak throughput",
                "classification_schema_version": 1, "samples": []}
    write_json(output / "results.json", metadata)
    for sample in samples:
        if selected and sample["id"] not in selected:
            continue
        folder = output / sample["id"]
        folder.mkdir()
        game = sample_path(sample)
        seconds = max(args.seconds, sample.get("min_seconds", 0))
        command = [str(binary), str(game), "--" + args.mode, "--audio",
                   "--codec", args.codec, "--api-mode", args.api_mode, "--seconds", str(seconds),
                   "--codepage", str(sample.get("codepage", 932)),
                   "--auto-dialog-ms", "50", "--data-dir", str(folder / "overlay"),
                   "--report", str(folder / "report.json"),
                   "--screenshot", str(folder / "frame.png")]
        if args.trace:
            command += ["--trace", str(folder / "api.log")]
        command += getattr(args, "runner_arg", [])
        result = dict(sample, command=command, guest_exe_sha256=digest(game))
        result["seconds"] = seconds
        if getattr(args, "warm_from", None):
            result["warm_inputs"] = import_warm_archives(args.warm_from, folder, sample,
                                                        metadata["binary_sha256"], result["guest_exe_sha256"])
        (folder / "cmd.txt").write_text(subprocess.list2cmdline(command) + "\n" +
            f"binary_sha256={metadata['binary_sha256']}\nguest_exe_sha256={result['guest_exe_sha256']}\n" +
            "environment=" + json.dumps({k: v for k, v in environment.items()
                                          if k.startswith("ADV_") or k in ("TEMP", "TMP", "TMPDIR")}) + "\n",
            encoding="utf-8")
        validate_native_baseline(result)
        result["report_available"] = False
        result["milestones"] = milestones({})
        started = time.monotonic()
        print(f"START {sample['id']}", flush=True)
        try:
            with (folder / "stdout.log").open("wb") as stdout, (folder / "stderr.log").open("wb") as stderr:
                process = subprocess.Popen(command, stdout=stdout, stderr=stderr,
                                           stdin=subprocess.DEVNULL, env=environment,
                                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                try:
                    result["exit_code"] = process.wait(timeout=seconds + args.timeout_grace)
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
                for key in ("stop_reason", "guest_exit_code", "frames", "first_frame_seconds", "elapsed_seconds", "host_calls",
                            "width", "height", "frame_fnv1a64", "missing_apis", "failure",
                            "screenshot_error", "audio", "pcm", "codec", "performance", "api", "startup", "gpu",
                            "code_cache", "aot", "compile_requests", "compilations"):
                    result[key] = report.get(key)
                result["report_available"] = True
                result["report_sha256"] = digest(report_path)
                result["milestones"] = milestones(report, evidence.get(sample["id"]), result["report_sha256"])
            except (ValueError, OSError) as error:
                result["report_error"] = str(error)
        stderr = (folder / "stderr.log").read_text(encoding="utf-8", errors="replace")
        result["stderr_tail"] = stderr[-12000:]
        result.update(analyze_report(result, stderr, sample=result, review=evidence.get(sample["id"]),
                                     report_hash=result.get("report_sha256")))
        state = result["status"]
        metadata["samples"].append(result)
        write_json(folder / "probe.json", result)
        write_json(output / "results.json", metadata)
        print(f"END {sample['id']}: {state}, frames={result.get('frames', 0)}, "
              f"wall={result['wall_seconds']:.2f}s", flush=True)
        if result.get("failure"):
            print("  " + result["failure"].splitlines()[0], flush=True)
        elif state in ("startup_exit", "early_exit", "startup_error", "engine_error_exit"):
            diagnostics = "report.json and api.log" if args.trace else "report.json"
            print(f"  guest exited with code {result['guest_exit_code']}; see {diagnostics}", flush=True)
    rows = ["# Startup probes", "", "Sequential, bounded, hardware-muted runs. Frame production does not certify playability.",
            "Called stubs, failed dynamic lookup candidates, engine error messages and CPU/SEH failures are separate evidence.",
            "Sparse frame observations and unreviewed exits do not diagnose a hang, input wait or crash.", "",
            "| Sample | Engine evidence | Result | Failure kind | Reached | Accepted | Frames | Low frame observation | Lookup candidates (recorded) | Attributed API blocker |", "|---|---|---|---|---|---|---:|---|---:|---|"]
    for result in metadata["samples"]:
        clean = lambda value: str(value).replace("|", "\\|").replace("\n", " ")
        rows.append("| " + " | ".join(map(clean, [result["id"], result.get("engine", "unknown"),
                    result["status"], result.get("failure_kind") or "none established", result["milestones"]["stage_reached"] or "unverified",
                    result["milestones"]["accepted_stage"] or "none", result.get("frames", 0),
                    result["observations"]["low_frame_observation"], len(result["dynamic_lookup_candidates"]),
                    result.get("blocker_api") or "none attributed"])) + " |")
    (output / "summary.md").write_text("\n".join(rows) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--only", nargs="+")
    parser.add_argument("--seconds", type=float, help="per-sample limit (default 20, or 15 with --quick)")
    parser.add_argument("--quick", action="store_true",
                        help="one sample per engine: those marked \"quick\": true, else each engine's first")
    parser.add_argument("--warm-from", type=Path, help="previous cold run; copy only matching JIT archives into fresh overlays")
    parser.add_argument("--timeout-grace", type=float, default=15)
    parser.add_argument("--mode", choices=("hidden", "headless"), default="hidden")
    parser.add_argument("--codec", choices=("native", "guest"), default="native")
    parser.add_argument("--api-mode", choices=("strict", "discovery"), default="strict")
    parser.add_argument("--evidence", type=Path, help="reviewed S1-S4 evidence keyed by sample id, tied to report SHA-256")
    parser.add_argument("--trace", action="store_true", help="diagnostic API trace, not a performance measurement")
    parser.add_argument("--runner-arg", action="append", default=[],
                        help="explicit candidate option, e.g. --runner-arg=--d3d9-sm2")
    options = parser.parse_args()
    if options.seconds is None:
        options.seconds = 15 if options.quick else 20
    if not math.isfinite(options.seconds) or not 0 < options.seconds <= 3600:
        parser.error("seconds must be finite and within 0..3600")
    if not math.isfinite(options.timeout_grace) or not 1 <= options.timeout_grace <= 120:
        parser.error("timeout grace must be finite and within 1..120")
    run(options)
