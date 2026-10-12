#!/usr/bin/env python3
"""Replay a timed observation or exact scene checkpoints in an isolated adb session.

Use an ignored target/ output directory. Local game and seed directories are read
only. --remote-game-dir reuses an already pushed game without deleting or changing
that directory; every session has a fresh UUID directory and write overlay.
This records virtual PCM and guest frame observations, not native Android output.
Native caching/static discovery are default; --no-code-cache is diagnostic.
--warm-code-cache
imports and hashes a prior native archive; --cold removes both cache files only
from the fresh overlay, and enabled native cache output is pulled as evidence.
On an arm64 device the engine is a64, which writes neither cache file.
"""
from __future__ import annotations

import argparse
import os
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import queue
import re
import shlex
import subprocess
import sys
import threading
import time
import uuid

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "performance"))
from play_session import load_schedule
sys.path.insert(0, str(Path(__file__).resolve().parent))
from scene_schedule import (assert_report, await_command, digest_json, exact_hash,
                            load_scene, validate_reference, valid_acknowledgement)

INPUT_COMMANDS = frozenset(("click", "move", "mouse-down", "mouse-up", "key", "key-down", "key-up", "menus",
                            "menu", "dialogs", "dialog", "dialog-track", "dialog-close",
                            "memory", "reset-metrics", "close"))
RESERVED_CHECKPOINTS = frozenset(("final", "blocked", "session", "actions", "device",
                                 "stdout", "stderr", "remote-stdout", "adb-stderr", "exit-code"))
REQUIRED_FINAL_EVIDENCE = {"final.json": "final.json", "final.png": "final.png",
                           "stderr.log": "stderr.log", "stdout.log": "remote-stdout.log",
                           "exit-code.txt": "exit-code.txt"}
ACKNOWLEDGEMENT_VERBS = {"dialog-track": "dialog"}
SCENE_COMMANDS = INPUT_COMMANDS | {"resume"}



def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path: Path, value: object) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.replace(path)


def shell_words(words: list[str]) -> str:
    return " ".join(shlex.quote(word) for word in words)


def remote_directory(value: str) -> str:
    if not value.startswith("/") or "\x00" in value or "\n" in value or "\r" in value:
        raise ValueError("remote game directory must be an absolute single-line POSIX path")
    if any(part in (".", "..") for part in value.split("/")):
        raise ValueError("remote game directory cannot contain . or .. components")
    return str(PurePosixPath(value))


def relative_exe(value: str) -> str:
    value = value.replace("\\", "/")
    path = PurePosixPath(value)
    if (not value or path.is_absolute() or ":" in value or "\x00" in value
            or "\n" in value or "\r" in value
            or any(part in ("", ".", "..") for part in value.split("/"))):
        raise ValueError("exe-relative must stay inside the game directory")
    return str(path)


def inventory(directory: Path, *, hashes: bool = False) -> list[dict]:
    rows = []
    if directory.is_symlink():
        raise ValueError(f"source directory cannot be a symlink: {directory}")
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"source tree cannot contain symlinks: {path}")
        if path.is_file():
            row = {"path": path.relative_to(directory).as_posix(), "bytes": path.stat().st_size}
            if hashes:
                row["sha256"] = sha256(path)
            rows.append(row)
    return rows


def load_font_manifest(path: Path | None, profile: str | None) -> dict | None:
    if path is None and profile is None:
        return None
    if path is None or profile is None:
        raise ValueError("--font-profile and --font-manifest must be supplied together")
    profile = remote_directory(profile)
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or set(manifest) != {"profile", "files"}:
        raise ValueError("font manifest requires profile and files")
    font_profile = manifest["profile"]
    files = manifest["files"]
    if (not isinstance(font_profile, dict) or set(font_profile) != {"path", "sha256"}
            or font_profile["path"] != profile or not isinstance(files, list) or not files):
        raise ValueError("font manifest profile must match --font-profile and list font files")
    font_profile["sha256"] = exact_hash(font_profile["sha256"], 64)
    paths = {profile}
    for row in files:
        if not isinstance(row, dict) or set(row) - {"path", "sha256", "bytes"}:
            raise ValueError("font file requires path, sha256 and optional bytes")
        row["path"] = remote_directory(row.get("path", ""))
        if row["path"] in paths:
            raise ValueError("font manifest contains duplicate paths")
        paths.add(row["path"])
        row["sha256"] = exact_hash(row.get("sha256"), 64)
        if "bytes" in row and (type(row["bytes"]) is not int or row["bytes"] < 0):
            raise ValueError("font manifest byte length must be nonnegative")
    files.sort(key=lambda row: row["path"])
    return manifest


def verify_remote_hashes(adb: "Adb", rows: list[tuple[str, str]]) -> None:
    # Bound each device query. Paths are shell-quoted, and every output row is
    # matched to its exact path rather than relying on output order.
    for start in range(0, len(rows), 32):
        batch = rows[start:start + 32]
        output = adb.shell(["sha256sum", *[path for path, _ in batch]])
        actual = {}
        for line in output.splitlines():
            match = re.fullmatch(r"([0-9a-fA-F]{64})\s+\*?(.*)", line)
            if not match or match[2] in actual:
                raise RuntimeError("device hash query returned an invalid/duplicate row")
            actual[match[2]] = match[1].lower()
        if set(actual) != {path for path, _ in batch}:
            raise RuntimeError("device hash query did not return every manifest path")
        for path, expected in batch:
            if actual[path] != expected:
                raise RuntimeError(f"device manifest hash differs: {path}")


def deployed_asset_inventory(adb: "Adb", game: str, local_files: list[dict], exe: str) -> list[dict]:
    # A reused deployment can intentionally omit unrelated local patches/saves.
    # Bind the actual device tree, require a local counterpart for every file,
    # then verify all their bytes before execution. Unlisted device assets cannot
    # silently participate in a scene reference.
    if adb.shell(["find", game, "-type", "l"]):
        raise ValueError("remote game tree cannot contain symlinks")
    listing = adb.shell(["find", game, "-type", "f"])
    local = {row["path"]: row for row in local_files}
    deployed = {}
    for path in listing.splitlines():
        if not path.startswith(game + "/"):
            raise ValueError("remote asset inventory escaped the selected game directory")
        relative = relative_exe(path[len(game) + 1:])
        if relative in deployed or relative not in local:
            raise ValueError(f"remote asset has no unique local counterpart: {relative}")
        deployed[relative] = local[relative]
    if not deployed or exe not in deployed:
        raise ValueError("remote asset inventory does not contain the selected executable")
    return [deployed[name] for name in sorted(deployed)]


def pull_pcm_evidence(adb: "Adb", remote: str, output: Path, metadata: dict) -> None:
    adb.pull(remote + "/audio.log", output / "audio.log")
    # Parse only the bounded Play snapshots, never recurse through a device tree
    # or pull chunk captures. The trace supplies the exact expected filenames.
    trace = output / "audio.log"
    if trace.stat().st_size > 16 * 1024 * 1024:
        raise RuntimeError("PCM trace exceeds the 16 MiB capture limit")
    snapshots = {}
    for line in trace.read_text(encoding="utf-8").splitlines():
        if not line.startswith("start "):
            continue
        fields = dict(token.split("=", 1) for token in line.split()[1:] if "=" in token)
        if int(fields.get("bytes", 0)) == 0:
            continue
        if (not re.fullmatch(r"[0-9]+", fields.get("us", ""))
                or not re.fullmatch(r"[0-9a-f]{8}", fields.get("id", ""))
                or not re.fullmatch(r"[0-9]+", fields.get("generation", ""))
                or not re.fullmatch(r"[0-9a-f]{16}", fields.get("hash", ""))):
            raise RuntimeError("invalid PCM start identity in diagnostic trace")
        name = f"{fields['us']}-{fields['id']}-{fields['generation']}-{fields['hash']}.pcm"
        if name in snapshots:
            raise RuntimeError("duplicate PCM start identity")
        snapshots[name] = int(fields["bytes"])
        if len(snapshots) > 512 or sum(snapshots.values()) > 256 * 1024 * 1024:
            raise RuntimeError("PCM snapshots exceed the 512-file/256-MiB capture limit")
    if not snapshots:
        raise RuntimeError("PCM capture contains no nonempty Play snapshots")
    pcm = output / "pcm"
    pcm.mkdir(exist_ok=True)
    metadata["pcm_captures"] = []
    for name, size in snapshots.items():
        adb.pull(remote + "/pcm/" + name, pcm / name)
        if size < 1 or (pcm / name).stat().st_size != size:
            raise RuntimeError(f"PCM capture length differs: {name}")
        metadata["pcm_captures"].append({"file": name, "bytes": size, "sha256": sha256(pcm / name)})
    metadata["pcm_evidence_complete"] = True


def validate_report(report: dict, name: str) -> None:
    if report.get("failure") or report.get("screenshot_error"):
        raise RuntimeError(f"{name}: {report.get('failure')} {report.get('screenshot_error')}")
    if type(report.get("frames")) is not int or report["frames"] <= 0:
        raise RuntimeError(f"{name}: no presented guest frame")
    api = report.get("api", {})
    if (not isinstance(api, dict) or api.get("stub_calls") != 0 or api.get("mode") != "strict"
            or api.get("acceptance_eligible") is not True or api.get("signature_conflicts")):
        raise RuntimeError(f"{name}: strict ABI acceptance failed")


class Adb:
    def __init__(self, args: argparse.Namespace):
        self.prefix = [str(args.adb), "-H", "127.0.0.1", "-P", str(args.adb_port), "-s", args.serial]
        self.timeout = args.transfer_timeout
        self.transport_retries = []

    @staticmethod
    def transient_transport_error(message: str) -> bool:
        message = message.lower()
        return any(marker in message for marker in (
            "connection reset", "push failed", "cannot connect to daemon", "daemon still not running",
            "failed to start daemon", "daemon startup failed", "adb server didn't ack"))

    def call(self, *args: str, timeout: float | None = None,
             idempotent: bool = False) -> subprocess.CompletedProcess:
        # Retries apply only to repeatable transfers/queries. Interactive stdin,
        # directory creation and directory push are never sent through this path
        # with retries: an uncertain directory push could otherwise nest a copy.
        for attempt in range(3):
            timed_out = None
            try:
                result = subprocess.run(self.prefix + list(args), capture_output=True, text=True,
                                        encoding="utf-8", errors="replace",
                                        timeout=self.timeout if timeout is None else timeout)
            except subprocess.TimeoutExpired as caught:
                timed_out = caught
                details = []
                for value in (caught.output, caught.stderr):
                    details.append(value.decode("utf-8", errors="replace") if isinstance(value, bytes)
                                   else value or "")
                message = " ".join(details)
            else:
                if not result.returncode:
                    return result
                message = f"adb {args[0]} failed ({result.returncode}): {result.stderr.strip()} {result.stdout.strip()}"
            before_dispatch = "cannot connect to daemon" in message.lower()
            if (not idempotent and not before_dispatch) or attempt == 2 or not self.transient_transport_error(message):
                if timed_out is not None:
                    raise timed_out
                raise RuntimeError(message)
            self.transport_retries.append({"command": args[0], "retry": attempt + 1, "error": message})
            time.sleep(30)
        raise AssertionError("unreachable adb retry state")

    def shell(self, words: list[str]) -> str:
        repeatable = bool(words) and words[0] in ("getprop", "getconf", "sha256sum", "chmod", "find")
        return self.call("shell", shell_words(words), idempotent=repeatable).stdout.strip()

    def push(self, source: Path, destination: str) -> None:
        self.call("push", str(source), destination, idempotent=source.is_file())

    def pull(self, source: str, destination: Path) -> None:
        self.call("pull", source, str(destination), idempotent=True)


def runner_command(args: argparse.Namespace, remote: str, game: str, duration: float) -> list[str]:
    command = [remote + "/adv-run", game + "/" + args.exe_relative,
               "--headless", "--audio", "--api-mode", "strict",
               "--codepage", str(args.codepage), "--seconds", str(duration), "--control-stdin",
               "--data-dir", remote + "/data", "--report", remote + "/final.json",
               "--screenshot", remote + "/final.png", "--codec", args.codec,
               "--codec-kernels", args.codec_kernels]
    if getattr(args, "frame_budget", False):
        command.append("--frame-budget")
    if getattr(args, "headless_gpu", False):
        command.append("--headless-gpu")
    if getattr(args, "no_time_skip", False):
        command.append("--no-time-skip")
    if args.no_tiering:
        command.append("--no-tiering")
    if args.schedule_profile:
        command.extend(["--schedule-profile", remote + "/schedule.tsv"])
    if args.auto_dialog_ms is not None:
        command.extend(["--auto-dialog-ms", str(args.auto_dialog_ms)])
    environment = []
    if getattr(args, "no_code_cache", False):
        command.append("--no-code-cache")
    if getattr(args, "perf_map", False):
        environment.append("ADV_PERF_MAP_DIR=" + remote)
    if getattr(args, "compile_threads", None) is not None:
        environment.append("ADV_COMPILE_THREADS=" + str(args.compile_threads))
    if args.font_diagnostics:
        environment.append("ADV_TEXT_DIAGNOSTICS=1")
    if getattr(args, "font_profile", None):
        environment.append("ADV_FONT_PROFILE=" + args.font_profile)
    if getattr(args, "capture_pcm", False):
        environment += ["ADV_AUDIO_TRACE=" + remote + "/audio.log", "ADV_AUDIO_DUMP_DIR=" + remote + "/pcm"]
    if environment:
        command = ["env"] + (["-u", "ADV_AUDIO_DUMP_CHUNKS"] if getattr(args, "capture_pcm", False) else []) + environment + command
    if getattr(args, "cpu_mask", None):
        command = ["taskset", args.cpu_mask] + command
    return command


def launch_script(command: list[str], remote: str, game: str, timeout: int) -> str:
    # The subshell records timeout/guest status before tee. Do not interpret tee's
    # exit status as the guest status. The independent remote timeout survives a
    # disconnected adb client. Only the fresh session directory receives writes.
    return (f"cd {shlex.quote(game)} && ("
            f"{shell_words(['timeout', '-s', 'KILL', str(timeout)] + command)} "
            f"2>{shlex.quote(remote + '/stderr.log')}; "
            f"result=$?; printf '%s\\n' \"$result\" >{shlex.quote(remote + '/exit-code.txt')}; "
            f") | tee {shlex.quote(remote + '/stdout.log')}")


def device_conditions(adb: Adb) -> dict:
    """Read aggregate thermal/display/scheduling conditions outside guest timing."""
    text = adb.call("shell", "dumpsys battery; dumpsys power; dumpsys window; cat /proc/self/status",
                    idempotent=True).stdout
    def field(pattern: str) -> str | None:
        match = re.search(pattern, text, re.MULTILINE)
        return match[1].strip() if match else None
    temperature = field(r"^\s*temperature:\s*(\d+)\s*$")
    focus = field(r"^\s*mCurrentFocus=(.*)$")
    if focus:
        focus = re.sub(r"Window\{[0-9a-f]+\s+", "Window{", focus)
    return {"battery_temperature_c": int(temperature) / 10 if temperature else None,
            "screen": field(r"^\s*mWakefulness=([A-Za-z]+)\s*$"),
            "foreground": focus, "allowed_cpus": field(r"^Cpus_allowed_list:\s*(.*)$")}


def run(args: argparse.Namespace) -> None:
    scene = load_scene(args.schedule)
    steps = scene["steps"] if scene else load_schedule(args.schedule)
    font_manifest = load_font_manifest(getattr(args, "font_manifest", None),
                                       getattr(args, "font_profile", None))
    reference_path = getattr(args, "scene_reference", None)
    record_path = getattr(args, "record_scene_reference", None)
    if scene and any("await" in step for step in steps) and not font_manifest:
        raise ValueError("scene checkpoints require --font-profile and --font-manifest")
    if (reference_path or record_path) and not scene:
        raise ValueError("scene references require an adv-scene-v1 schedule")
    if record_path and any(step.get("observe") for step in steps):
        raise ValueError("ungated observations cannot record a fixed-work reference")
    if reference_path and record_path:
        raise ValueError("recording a reference and validating it require separate runs")
    cold = getattr(args, "cold", False)
    warm_profile = getattr(args, "warm_profile", None)
    warm_code_cache = getattr(args, "warm_code_cache", None)
    if warm_code_cache and (cold or not warm_code_cache.is_file() or warm_code_cache.is_symlink()):
        raise ValueError("native cache import requires a regular file and a warm replay")
    if cold and warm_profile:
        raise ValueError("--cold and --warm-profile are mutually exclusive")
    for step in steps:
        if "command" in step and step["command"].split()[0] not in (SCENE_COMMANDS if scene else INPUT_COMMANDS):
            raise ValueError("schedule report/capture paths must use named checkpoints")
        if step.get("checkpoint") in RESERVED_CHECKPOINTS:
            raise ValueError("checkpoint name is reserved for session evidence")
    args.exe_relative = relative_exe(args.exe_relative)
    output = args.output.resolve()
    artifact_root = Path(os.environ.get("ADV_ARTIFACT_ROOT", REPO / "target")).resolve()
    if not output.is_relative_to(artifact_root):
        raise ValueError("--output must be inside ADV_ARTIFACT_ROOT (default: ignored target/)")
    binary = args.binary.resolve(strict=True)
    game_dir = args.game_dir.absolute()
    if not game_dir.is_dir():
        raise ValueError("--game-dir must be a local game directory")
    game_files = inventory(game_dir, hashes=bool(scene))
    exe = (game_dir / args.exe_relative).resolve(strict=True)
    if not exe.is_file() or not exe.is_relative_to(game_dir.resolve()):
        raise ValueError("guest executable must be a file inside game-dir")
    seed = args.seed_data.absolute() if args.seed_data else None
    if seed and not seed.is_dir():
        raise ValueError("--seed-data must be a directory")
    seed_files = inventory(seed, hashes=True) if seed else []
    if warm_profile and (not warm_profile.is_file() or warm_profile.is_symlink()):
        raise ValueError("--warm-profile must be a regular local file")
    warm_profile = warm_profile.resolve(strict=True) if warm_profile else None
    game_reused = remote_directory(args.remote_game_dir) if args.remote_game_dir else None
    adb = Adb(args)
    if scene and game_reused:
        game_files = deployed_asset_inventory(adb, game_reused, game_files, args.exe_relative)
    reference = json.loads(reference_path.read_text(encoding="utf-8")) if reference_path else None
    binding = {"guest_exe_sha256": sha256(exe), "game_files": game_files,
               "seed_files": [row for row in seed_files if row["path"] != "jit-warm.bin"],
               "font_manifest": font_manifest, "codepage": args.codepage,
               "scene_sha256": digest_json(scene)} if scene else None
    if reference is not None:
        validate_reference(reference, binding, steps)
    if record_path:
        record_path = record_path.resolve()
        if (not record_path.is_relative_to(output) or record_path.suffix.lower() != ".json"
                or record_path.name in {"session.json", "actions.json", "final.json", "blocked.json"}):
            raise ValueError("--record-scene-reference must be a fresh JSON path inside --output")
    remote = "/data/local/tmp/adv-host-sample-" + uuid.uuid4().hex
    game = game_reused or remote + "/game"
    duration = max(15.0, float(scene["timeout_seconds"]) if scene else
                   float(steps[-1]["at"]) + args.tail_seconds)
    remote_timeout = math.ceil(duration + args.timeout_grace)
    command = runner_command(args, remote, game, duration)
    script = launch_script(command, remote, game, remote_timeout)
    output.mkdir(parents=True, exist_ok=False)
    metadata = {"context": "adb-shell", "serial": args.serial, "adb_port": args.adb_port,
                "binary": str(binary), "binary_sha256": sha256(binary),
                "game_directory": str(game_dir), "guest_exe_sha256": sha256(exe),
                "game_files": game_files, "game_asset_hash_scope": "all files" if scene else "executable only",
                "game_asset_inventory_source": "actual reused device files" if scene and game_reused else "local source tree",
                "seed_data": str(seed) if seed else None, "seed_files": seed_files,
                "schedule": steps, "schedule_sha256": sha256(args.schedule),
                "remote_run_directory": remote, "remote_game_directory": game,
                "remote_game_reused": bool(game_reused), "command": command,
                "remote_timeout_seconds": remote_timeout, "audio_device_expected": False,
                "font_diagnostics": args.font_diagnostics,
                "font_profile": getattr(args, "font_profile", None), "font_manifest": font_manifest,
                "font_manifest_sha256": sha256(args.font_manifest) if font_manifest else None,
                "capture_pcm": getattr(args, "capture_pcm", False),
                "pcm_evidence_complete": False if getattr(args, "capture_pcm", False) else None,
                "warm_mode": "cold" if cold else "warm" if warm_profile else "seed",
                "warm_input_sha256": sha256(warm_profile) if warm_profile else next(
                    (row["sha256"] for row in seed_files if row["path"] == "jit-warm.bin" and not cold), None),
                "scene_binding": binding, "scene_reference": str(reference_path) if reference_path else None,
                "scene_reference_sha256": sha256(reference_path) if reference_path else None,
                "scene_validation_status": "pending" if reference else "observation_only" if scene else "timed_observation",
                "fixed_workload_verified": False,
                "suitable_for_performance_comparison": not args.font_diagnostics and not getattr(args, "capture_pcm", False)
                    and (not scene or reference is not None),
                "frame_hash_reference": "requires matching font and rendering paths",
                "completed": False, "checkpoints": [], "evidence_errors": []}
    process = None
    reader = None
    started = None
    actions = []
    responses: queue.Queue[str | None] = queue.Queue()
    error = None
    caches = True
    write_json(output / "session.json", metadata)
    try:
        metadata["device"] = {name: adb.shell(["getprop", name]) for name in (
            "ro.product.model", "ro.build.version.release", "ro.build.version.sdk",
            "ro.product.cpu.abilist")}
        caches = not metadata["device"]["ro.product.cpu.abilist"].startswith("arm64")
        metadata["native_cache_files"] = caches
        metadata["device"]["host_page_size"] = adb.shell(["getconf", "PAGESIZE"])
        # mkdir without -p refuses a collision; no shared game directory is changed.
        adb.shell(["mkdir", remote])
        # This durable local receipt is never overwritten with later reports.
        # A caller may clean up only after this mkdir has actually succeeded.
        (output / "remote-owned.txt").write_text(remote, encoding="utf-8")
        adb.push(binary, remote + "/adv-run")
        adb.shell(["chmod", "700", remote + "/adv-run"])
        if not game_reused:
            adb.push(game_dir, game)
        if seed:
            adb.push(seed, remote + "/data")
        else:
            adb.shell(["mkdir", remote + "/data"])
        if cold:
            # Only the newly created run overlay is changed. The seed and shared
            # game directory remain immutable, including their warm archives.
            adb.shell(["rm", "-f", remote + "/data/jit-warm.bin", remote + "/data/jit-code.bin"])
        if warm_profile:
            adb.push(warm_profile, remote + "/data/jit-warm.bin")
        if warm_code_cache:
            adb.push(warm_code_cache, remote + "/data/jit-code.bin")
            verify_remote_hashes(adb, [(remote + "/data/jit-code.bin", sha256(warm_code_cache))])
        for path, expected in ((remote + "/adv-run", metadata["binary_sha256"]),
                               (game + "/" + args.exe_relative, metadata["guest_exe_sha256"])):
            actual = adb.shell(["sha256sum", path]).split()[0].lower()
            if actual != expected:
                raise RuntimeError(f"device file hash differs: {path}")
        metadata["remote_executable_hashes_verified"] = True
        if scene:
            rows = [(game + "/" + row["path"], row["sha256"]) for row in game_files]
            rows += [(remote + "/data/" + row["path"], row["sha256"]) for row in seed_files
                     if row["path"] != "jit-warm.bin"]
            verify_remote_hashes(adb, rows)
            metadata["remote_asset_seed_hashes_verified"] = True
        if font_manifest:
            verify_remote_hashes(adb, [(row["path"], row["sha256"]) for row in
                                       [font_manifest["profile"], *font_manifest["files"]]])
            metadata["remote_font_hashes_verified"] = True
        if warm_profile:
            verify_remote_hashes(adb, [(remote + "/data/jit-warm.bin", sha256(warm_profile))])
        if cold or warm_profile:
            metadata["warm_profile_input_verified"] = True
        write_json(output / "session.json", metadata)
        if getattr(args, "record_device_state", False):
            metadata["device_conditions_before"] = device_conditions(adb)
        with (output / "stdout.log").open("w", encoding="utf-8") as stdout, \
                (output / "adb-stderr.log").open("w", encoding="utf-8") as adb_stderr:
            started = time.monotonic()
            deadline = started + remote_timeout + 10
            process = subprocess.Popen(adb.prefix + ["shell", "-T", script], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=adb_stderr, text=True,
                                       encoding="utf-8", errors="replace", bufsize=1)

            def receive() -> None:
                try:
                    for line in process.stdout:
                        stdout.write(line)
                        stdout.flush()
                        if line.startswith("control "):
                            responses.put(line.strip())
                finally:
                    responses.put(None)

            reader = threading.Thread(target=receive, daemon=True)
            reader.start()

            def send(line: str, timeout: float | None = None) -> str:
                sent = time.monotonic() - started
                if process.poll() is not None:
                    raise RuntimeError(f"runner exited before {line!r}")
                process.stdin.write(line + "\n")
                process.stdin.flush()
                reply = None
                try:
                    remaining = min(args.ack_timeout if timeout is None else timeout,
                                    max(0.001, deadline - time.monotonic()))
                    try:
                        reply = responses.get(timeout=remaining)
                    except queue.Empty as caught:
                        raise TimeoutError(f"control acknowledgement timed out: {line}") from caught
                finally:
                    actions.append({"sent_seconds": sent, "ack_seconds": time.monotonic() - started,
                                    "command": line, "ack": reply})
                    write_json(output / "actions.json", actions)
                if not valid_acknowledgement(line, reply):
                    raise RuntimeError(f"command failed: {line!r}: {reply}")
                print(f"{sent:.3f}s {reply}", flush=True)
                return reply

            try:
                for step in steps:
                    due = time.monotonic() - started if scene else step["at"]
                    if "delay_ms" in step:
                        # UI setup delays explicitly resume queued inputs. These
                        # small host delays never define the measured workload.
                        send("resume")
                        due += step["delay_ms"] / 1000
                    while time.monotonic() - started < due:
                        if process.poll() is not None:
                            raise RuntimeError("runner exited before the scheduled input")
                        if time.monotonic() >= deadline:
                            raise TimeoutError("scene setup exceeded the remote session deadline")
                        time.sleep(min(.05, max(.001, due - (time.monotonic() - started))))
                    if "delay_ms" in step:
                        continue
                    if "command" in step:
                        send(step["command"])
                    else:
                        name = step["checkpoint"]
                        gate_ack = None
                        if scene and "await" in step:
                            spec = step["await"]
                            gate_ack = send(await_command(spec),
                                            timeout=spec["timeout_ms"] / 1000 + args.ack_timeout)
                        send("report " + remote + "/" + name + ".json")
                        adb.pull(remote + "/" + name + ".json", output / (name + ".json"))
                        report = json.loads((output / (name + ".json")).read_text(encoding="utf-8"))
                        send("capture " + remote + "/" + name + ".png")
                        adb.pull(remote + "/" + name + ".png", output / (name + ".png"))
                        validate_report(report, name)
                        if scene and "await" in step:
                            expected_hash = step["await"]["frame_fnv1a64"]
                            if report.get("frame_fnv1a64") != expected_hash:
                                raise RuntimeError(f"{name}: exact checkpoint hash differs: expected {expected_hash}, "
                                                   f"actual {report.get('frame_fnv1a64')}")
                            fields = dict(token.split("=", 1) for token in gate_ack.split()[3:] if "=" in token)
                            if report["frames"] != int(fields["frame"]):
                                raise RuntimeError(f"{name}: report frame differs from frozen gate ACK")
                            gate = report.get("scene_gate", {})
                            if not isinstance(gate, dict) or gate.get("frozen") is not True or gate.get("pending") is not False:
                                raise RuntimeError(f"{name}: checkpoint report does not confirm a frozen latch")
                            assert_report(report, step.get("assert", {}), name)
                            if reference:
                                assert_report(report, reference["checkpoints"][name].get("assert", {}), name)
                        metadata["checkpoints"].append({"name": name, "at_seconds": time.monotonic() - started,
                                                        "frames": report["frames"],
                                                        "frame_fnv1a64": report.get("frame_fnv1a64"),
                                                        "gate_ack": gate_ack,
                                                        "assert": step.get("assert", {}),
                                                        "scene_gate": report.get("scene_gate"),
                                                        "report_sha256": sha256(output / (name + ".json")),
                                                        "png_sha256": sha256(output / (name + ".png"))})
                        write_json(output / "session.json", metadata)
                        if step.get("reset_metrics", False):
                            send("reset-metrics")
                if scene and steps[-1].get("command", "").strip() != "close":
                    send("close")
                process.wait(timeout=max(.001, deadline - time.monotonic()))
            except BaseException:
                # Save the current frame/report before closing on a replay failure.
                if process.poll() is None:
                    for line in ("key-up 17", "report " + remote + "/blocked.json",
                                 "capture " + remote + "/blocked.png", "close"):
                        try:
                            send(line, timeout=1)
                        except Exception:
                            pass
                raise
            finally:
                if process.poll() is None:
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
                reader.join(timeout=5)
    except BaseException as caught:
        error = caught
        metadata["error"] = f"{type(caught).__name__}: {caught}"
    finally:
        if getattr(args, "record_device_state", False):
            try:
                metadata["device_conditions_after"] = device_conditions(adb)
            except Exception as caught:
                metadata["device_conditions_error"] = str(caught)
        # The remote directory remains available after failures/disconnection. Never
        # recursively pull game/data resources and never delete a reused source.
        evidence = ("final.json", "final.png", "stderr.log", "stdout.log", "exit-code.txt")
        if error is not None:
            evidence += ("blocked.json", "blocked.png")
        if args.schedule_profile:
            evidence += ("schedule.tsv",)
        if getattr(args, "perf_map", False):
            try:
                names = adb.shell(["find", remote, "-maxdepth", "1", "-type", "f", "-name", "perf-*"])
                for path in names.splitlines():
                    name = path.rsplit("/", 1)[-1]
                    if re.fullmatch(r"perf-[0-9]+\.(map|events\.tsv)", name):
                        evidence += (name,)
            except Exception as caught:
                metadata["evidence_errors"].append({"file": "perf map", "error": str(caught)})
        pulled_evidence = set()
        for name in evidence:
            try:
                adb.pull(remote + "/" + name, output / ("remote-stdout.log" if name == "stdout.log" else name))
                pulled_evidence.add(name)
            except Exception as caught:
                metadata["evidence_errors"].append({"file": name, "error": str(caught)})
        missing = [name for name, local in REQUIRED_FINAL_EVIDENCE.items()
                   if name not in pulled_evidence or not (output / local).is_file()]
        metadata["missing_required_evidence"] = missing
        metadata["evidence_complete"] = not missing
        metadata["transport_retries"] = getattr(adb, "transport_retries", [])
        metadata["adb_exit_code"] = process.returncode if process else None
        metadata["wall_seconds"] = time.monotonic() - started if started else None
        if getattr(args, "capture_pcm", False):
            try:
                pull_pcm_evidence(adb, remote, output, metadata)
            except Exception as caught:
                metadata["evidence_errors"].append({"file": "PCM captures", "error": str(caught)})
                metadata["missing_required_evidence"].append("PCM captures")
                metadata["evidence_complete"] = False
                if error is None:
                    error = caught
                    metadata["error"] = f"{type(caught).__name__}: {caught}"
        if caches and (cold or warm_profile or scene):
            try:
                adb.pull(remote + "/data/jit-warm.bin", output / "jit-warm.bin")
                metadata["warm_output_sha256"] = sha256(output / "jit-warm.bin")
            except Exception as caught:
                metadata["evidence_errors"].append({"file": "jit-warm.bin", "error": str(caught)})
                metadata["missing_required_evidence"].append("jit-warm.bin")
                metadata["evidence_complete"] = False
                if error is None:
                    error = caught
                    metadata["error"] = f"{type(caught).__name__}: {caught}"
        if caches and not getattr(args, "no_code_cache", False):
            try:
                adb.pull(remote + "/data/jit-code.bin", output / "jit-code.bin")
                metadata["code_cache_output_sha256"] = sha256(output / "jit-code.bin")
            except Exception as caught:
                metadata["evidence_errors"].append({"file": "jit-code.bin", "error": str(caught)})
                metadata["missing_required_evidence"].append("jit-code.bin")
                metadata["evidence_complete"] = False
                if error is None:
                    error = caught
                    metadata["error"] = f"{type(caught).__name__}: {caught}"
        try:
            if "exit-code.txt" in missing:
                raise RuntimeError("missing required final evidence: " + ", ".join(missing))
            exit_code = int((output / "exit-code.txt").read_text(encoding="utf-8").strip())
            metadata["remote_exit_code"] = exit_code
            if exit_code:
                raise RuntimeError(f"remote runner failed with exit code {exit_code}")
            if not (output / "final.json").is_file():
                raise RuntimeError("missing final.json execution report")
            report = json.loads((output / "final.json").read_text(encoding="utf-8"))
            validate_report(report, "final")
            if not (output / "final.png").is_file():
                raise RuntimeError("missing final screenshot")
            metadata["final_report_sha256"] = sha256(output / "final.json")
            metadata["final_png_sha256"] = sha256(output / "final.png")
            if missing:
                raise RuntimeError("missing required final evidence: " + ", ".join(missing))
            if error is None:
                metadata["completed"] = True
                if scene:
                    metadata["scene_validation_status"] = "validated" if reference else "observation_only"
                    metadata["fixed_workload_verified"] = reference is not None
                    if record_path:
                        record_path.parent.mkdir(parents=True, exist_ok=True)
                        if record_path.exists():
                            raise RuntimeError("record reference path already exists")
                        write_json(record_path, {"schema": "adv-scene-reference-v1", "status": "recorded",
                            "binding": binding, "source_actions_sha256": sha256(output / "actions.json"),
                            "source_checkpoint_evidence": [{"name": row["name"],
                                "report_sha256": row["report_sha256"], "png_sha256": row["png_sha256"]}
                                for row in metadata["checkpoints"]],
                            "source_binary_sha256": metadata["binary_sha256"], "checkpoints": {
                                row["name"]: {"frame_fnv1a64": row["frame_fnv1a64"], "assert": row["assert"]}
                                for row in metadata["checkpoints"]}})
                        metadata["recorded_reference"] = str(record_path)
        except Exception as caught:
            if error is None:
                error = caught
                metadata["error"] = f"{type(caught).__name__}: {caught}"
            metadata["completed"] = False
        if error is not None:
            metadata["suitable_for_performance_comparison"] = False
            metadata["fixed_workload_verified"] = False
            if scene:
                metadata["scene_validation_status"] = "failed"
        write_json(output / "session.json", metadata)
    if error:
        raise error
    print(f"Sample evidence: {output}", flush=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adb", required=True, type=Path)
    parser.add_argument("--adb-port", type=int, default=5037)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--game-dir", required=True, type=Path)
    parser.add_argument("--exe-relative", required=True)
    parser.add_argument("--remote-game-dir", help="reuse an existing read-only device game directory")
    parser.add_argument("--seed-data", type=Path)
    parser.add_argument("--schedule", required=True, type=Path)
    parser.add_argument("--scene-reference", type=Path, help="independently approved adv-scene-reference-v1 JSON")
    parser.add_argument("--record-scene-reference", type=Path,
                        help="write an observation-only reference inside the fresh output directory")
    parser.add_argument("--font-profile", help="device absolute ADV_FONT_PROFILE path")
    parser.add_argument("--font-manifest", type=Path, help="local JSON hashes for the device profile and font files")
    warmth = parser.add_mutually_exclusive_group()
    warmth.add_argument("--cold", action="store_true", help="remove jit-warm.bin only from this run's new overlay")
    warmth.add_argument("--warm-profile", type=Path, help="override this run's jit-warm.bin with a captured warm archive")
    parser.add_argument("--warm-code-cache", type=Path, help="import jit-code.bin into this run's independent overlay")
    parser.add_argument("--no-code-cache", action="store_true", help="diagnostic bypass of native caching and image precompilation")
    parser.add_argument("--capture-pcm", action="store_true", help="capture bounded Play snapshots; excludes performance comparisons")
    parser.add_argument("--codepage", required=True, type=int, choices=(932, 936))
    parser.add_argument("--output", required=True, type=Path, help="fresh directory below ignored target/")
    parser.add_argument("--codec", choices=("guest", "native"), default="native")
    parser.add_argument("--codec-kernels", choices=("guest", "verify", "native"), default="native")
    parser.add_argument("--frame-budget", action="store_true", help="bounded per-frame diagnostics in reports")
    parser.add_argument("--headless-gpu", action="store_true", help="explicit portable GPU frontend; requires a gpu runner")
    parser.add_argument("--no-time-skip", action="store_true", help="disable existing Ctrl idle-time skipping")
    parser.add_argument("--perf-map", action="store_true", help="local-only HP JIT symbol map and publication events")
    parser.add_argument("--no-tiering", action="store_true")
    parser.add_argument("--compile-threads", type=int, choices=range(1, 9))
    parser.add_argument("--record-device-state", action="store_true",
                        help="record battery temperature, screen, foreground and CPU affinity before/after execution")
    parser.add_argument("--cpu-mask", help="explicit hexadecimal taskset mask for the guest and compiler workers")
    parser.add_argument("--schedule-profile", action="store_true")
    parser.add_argument("--font-diagnostics", action="store_true",
                        help="record font requested/resolved/source in stderr; excludes performance comparisons")
    parser.add_argument("--auto-dialog-ms", type=int)
    parser.add_argument("--tail-seconds", type=float, default=3)
    parser.add_argument("--timeout-grace", type=float, default=15)
    parser.add_argument("--ack-timeout", type=float, default=30)
    parser.add_argument("--transfer-timeout", type=float, default=600)
    args = parser.parse_args()
    for name, minimum, maximum in (("tail_seconds", 0, 3600), ("timeout_grace", 1, 120),
                                    ("ack_timeout", 1, 60), ("transfer_timeout", 1, 1800)):
        value = getattr(args, name)
        if not math.isfinite(value) or not minimum <= value <= maximum:
            parser.error(f"--{name.replace('_', '-')} must be within {minimum}..{maximum}")
    if not 1 <= args.adb_port <= 65535 or not args.serial.strip():
        parser.error("invalid adb port or serial")
    if args.auto_dialog_ms is not None and not 0 <= args.auto_dialog_ms <= 60000:
        parser.error("--auto-dialog-ms must be within 0..60000")
    if args.cpu_mask is not None and (not re.fullmatch(r"[0-9a-fA-F]{1,16}", args.cpu_mask) or int(args.cpu_mask, 16) == 0):
        parser.error("--cpu-mask must be a nonzero hexadecimal mask")
    return args


if __name__ == "__main__":
    try:
        run(parse_args())
    except Exception as failure:
        print(f"Replay failed: {failure}", file=sys.stderr)
        sys.exit(1)
