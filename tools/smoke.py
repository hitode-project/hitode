#!/usr/bin/env python3
"""Headless game smoke runs: start each game, click into the story and capture it.

  python3 tools/smoke.py [NAME...]      run the named games (all without names) in parallel
  python3 tools/smoke.py --list         show the configured games
  python3 tools/smoke.py --dry-run ...  print each game's command and steps only

Games come from a local JSON file naming your own copies, never part of the
repository: $ADV_SMOKE, else $ADV_SAMPLES/smoke.json (tools/smoke.example.json
shows the shape). An entry's exe is relative to $ADV_SAMPLES; its steps are
separated by semicolons:
  wait S | capture TAG | click X Y | clicks N X Y GAP | key VK | text STRING

Every game runs with the full headless recipe: --headless --headless-gpu
--audio with the audio device muted, --auto-dialog-ms 1000 for dialogs shown
before the first frame, the Wine build tools/env.py finds, and a fresh data
directory, so saves of real play stay untouched. Unless --bin names a binary,
adv-run is built first (release, gpu and media).

Output: target/smoke/<name>/ holds run.log, report.json and TAG.png. A table
shows each game's exit, frames, first frame and failure; the exit status is 1
when a game panicked, failed or did not exit cleanly.
Tests: python3 -m unittest tools/test_smoke.py
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import env as toolchain  # noqa: E402

ROOT = toolchain.ROOT
FEATURES = "adv-shell/gpu,adv-shell/media"
# Seconds a game gets to exit after `close` before it is killed.
CLOSE_GRACE = 20


@dataclass
class Game:
    name: str
    exe: Path
    codepage: str
    steps: list[list[str]]


def parse_steps(text: str) -> list[list[str]]:
    """Semicolon-separated steps, each checked for its command and argument count."""
    arity = {"wait": 1, "capture": 1, "click": 2, "clicks": 4, "key": 1}
    steps = []
    for raw in text.split(";"):
        step = raw.strip()
        if not step:
            continue
        verb, _, rest = step.partition(" ")
        if verb == "text":
            steps.append([verb, rest])
            continue
        args = rest.split()
        if verb not in arity or len(args) != arity[verb]:
            raise ValueError(f"bad step {step!r}")
        for arg in args[1:] if verb == "capture" else args:
            float(arg) if verb in ("wait", "clicks") else int(arg)
        steps.append([verb, *args])
    return steps


def duration(steps: list[list[str]]) -> float:
    """The seconds a step list takes, clicks included."""
    total = 0.0
    for step in steps:
        if step[0] == "wait":
            total += float(step[1])
        elif step[0] == "clicks":
            total += int(step[1]) * (0.5 + float(step[4]))
        else:
            total += 1.0
    return total


def config_path(env: dict) -> Path:
    if env.get("ADV_SMOKE"):
        return Path(env["ADV_SMOKE"])
    if env.get("ADV_SAMPLES"):
        return Path(env["ADV_SAMPLES"]) / "smoke.json"
    raise toolchain.Missing(["missing games: set ADV_SAMPLES (with smoke.json in it) or ADV_SMOKE"])


def load(env: dict) -> list[Game]:
    path = config_path(env)
    if not path.is_file():
        raise toolchain.Missing([f"missing {path}: copy tools/smoke.example.json there and list your games"])
    samples = Path(env.get("ADV_SAMPLES", path.parent))
    games = []
    for name, entry in json.loads(path.read_text(encoding="utf-8"))["games"].items():
        games.append(Game(name, samples / entry["exe"], str(entry.get("codepage", "auto")),
                          parse_steps(entry["steps"])))
    return games


def command(binary: Path, wine: Path, game: Game, out: Path) -> list[str]:
    return [str(binary), "--wine", str(wine), "--headless", "--headless-gpu", "--audio",
            "--auto-dialog-ms", "1000", "--codepage", game.codepage, "--control-stdin",
            "--seconds", str(int(duration(game.steps)) + 2 * CLOSE_GRACE),
            "--data-dir", str(out / "data"), "--report", str(out / "report.json"), str(game.exe)]


def control(game: Game, out: Path) -> list[tuple[float, str]]:
    """Each control line with the pause after it."""
    lines = []
    for verb, *args in game.steps:
        if verb == "wait":
            lines.append((float(args[0]), ""))
        elif verb == "capture":
            lines.append((1.0, f"capture {out / (args[0] + '.png')}"))
        elif verb in ("click", "clicks"):
            count, (x, y), gap = (1, args, 0.0) if verb == "click" else (int(args[0]), args[1:3], float(args[3]))
            for _ in range(count):
                lines += [(0.3, f"move {x} {y}"), (0.12, f"mouse-down {x} {y}"), (gap, f"mouse-up {x} {y}")]
        elif verb == "key":
            lines.append((0.3, f"key {args[0]}"))
        else:
            lines.append((0.3, f"text {args[0]}"))
    return lines


def play(binary: Path, wine: Path, game: Game, env: dict, root: Path) -> dict:
    out = root / game.name
    shutil.rmtree(out, ignore_errors=True)
    (out / "data").mkdir(parents=True)
    with (out / "run.log").open("w") as log:
        process = subprocess.Popen(command(binary, wine, game, out), stdin=subprocess.PIPE,
                                   stdout=log, stderr=subprocess.STDOUT, text=True, env=env)
        try:
            for pause, line in control(game, out):
                if process.poll() is not None:
                    break
                if line:
                    process.stdin.write(line + "\n")
                    process.stdin.flush()
                time.sleep(pause)
            if process.poll() is None:
                process.stdin.write("close\n")
                process.stdin.flush()
            process.wait(CLOSE_GRACE)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            pass
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    return summarize(game.name, out, process.returncode)


def summarize(name: str, out: Path, code: int) -> dict:
    log = (out / "run.log").read_text(errors="replace")
    panic = next((line for line in log.splitlines() if "panicked at" in line), None)
    try:
        report = json.loads((out / "report.json").read_text())
    except (OSError, ValueError):
        report = {}
    failure = panic or report.get("failure") or (None if report else "no report")
    if not failure and (code != 0 or report.get("guest_exit_code") not in (0, None)):
        failure = f"exit {code}, guest {report.get('guest_exit_code')}"
    return {"name": name, "frames": report.get("frames"), "first_frame": report.get("first_frame_seconds"),
            "stop": report.get("stop_reason"), "failure": failure,
            "captures": sorted(p.name for p in out.glob("*.png"))}


def build(env: dict) -> Path:
    subprocess.run(["cargo", "build", "--release", "-p", "adv-run", "--features", FEATURES],
                   cwd=ROOT, env=env, check=True)
    return Path(env.get("CARGO_TARGET_DIR", ROOT / "target")) / "release/adv-run"


def table(results: list[dict]) -> str:
    rows = [("game", "frames", "first", "stop", "captures", "failure")]
    for r in results:
        first = f"{r['first_frame']:.1f}s" if r["first_frame"] is not None else "-"
        rows.append((r["name"], str(r["frames"] or 0), first, r["stop"] or "-",
                     str(len(r["captures"])), r["failure"] or "-"))
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]) - 1)]
    return "\n".join("  ".join(c.ljust(w) for c, w in zip(row, widths)) + "  " + row[-1] for row in rows)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("names", nargs="*", help="games to run (default: all)")
    parser.add_argument("--list", action="store_true", help="show the configured games")
    parser.add_argument("--dry-run", action="store_true", help="print commands and steps only")
    parser.add_argument("--bin", type=Path, help="adv-run to use instead of building it")
    parser.add_argument("--out", type=Path, default=ROOT / "target/smoke", help="output directory")
    args = parser.parse_args(argv)
    try:
        games = load(dict(os.environ))
        unknown = set(args.names) - {g.name for g in games}
        if unknown:
            raise toolchain.Missing([f"unknown game {n}; --list shows the configured ones" for n in sorted(unknown)])
        if args.list:
            for game in games:
                print(f"{game.name}: {game.exe} (codepage {game.codepage}, {duration(game.steps):.0f} s)")
            return 0
        games = [g for g in games if not args.names or g.name in args.names]
        env = toolchain.cargo("mac")
        wine = toolchain.wine_dist(env)
    except toolchain.Missing as missing:
        print(missing, file=sys.stderr)
        return 1
    env["ADV_AUDIO_MUTE_DEVICE"] = "1"
    out = args.out.resolve()
    if args.dry_run:
        binary = args.bin or Path(env.get("CARGO_TARGET_DIR", ROOT / "target")) / "release/adv-run"
        for game in games:
            print(f"{game.name}: {shlex.join(command(binary, wine, game, out / game.name))}")
            print("  " + "; ".join(" ".join(s) for s in game.steps))
        return 0
    binary = args.bin or build(env)
    with ThreadPoolExecutor(len(games) or 1) as pool:
        results = list(pool.map(lambda g: play(binary, wine, g, env, out), games))
    print(table(results))
    print(f"captures and logs: {out}")
    return 1 if any(r["failure"] for r in results) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
