#!/usr/bin/env python3
"""Compare child CPU per presented frame on the old and Wine stacks."""

import argparse
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


EXIT = re.compile(r"\bexit (?P<code>\d+) after (?P<elapsed>[\d.]+)s, (?P<frames>\d+) frames presented\b")
OLD_EXIT = re.compile(
    r"^(?:time_limit|frame_limit|guest_exit): exit=(?P<code>none|\d+) "
    r"frames=(?P<frames>\d+) elapsed=(?P<elapsed>[\d.]+)s\b"
)


def run(command):
    """Reap this child alone; a file keeps verbose runner output off the pipe."""
    env = dict(os.environ, ADV_AUDIO_MUTE_DEVICE="1")
    with tempfile.TemporaryFile() as output:
        with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                              stderr=subprocess.STDOUT, env=env) as child:
            _, status, usage = os.wait4(child.pid, 0)
            child.returncode = os.waitstatus_to_exitcode(status)
        if child.returncode != 0:
            raise ValueError(f"runner exited with status {child.returncode}")
        output.seek(0)
        summary = None
        for line in output:
            line = line.decode("utf-8", errors="replace")
            match = EXIT.search(line) or OLD_EXIT.search(line)
            if match:
                summary = match
        if summary is None:
            raise ValueError("runner output has no exit summary")
        if summary["code"] not in ("0", "none"):
            raise ValueError(f"guest exited with status {summary['code']}")
    frames = int(summary["frames"])
    cpu = usage.ru_utime + usage.ru_stime
    return {
        "user_seconds": usage.ru_utime,
        "system_seconds": usage.ru_stime,
        "cpu_seconds": cpu,
        "peak_rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
        "elapsed_seconds": float(summary["elapsed"]),
        "frames": frames,
        "cpu_per_frame_seconds": cpu / frames if frames else None,
    }


def compare(old, wine, threshold):
    baseline = old["cpu_per_frame_seconds"]
    candidate = wine["cpu_per_frame_seconds"]
    ratio = candidate / baseline if baseline and candidate is not None else None
    frame_ratio = wine["frames"] / old["frames"] if old["frames"] else None
    return {
        "old": old,
        "wine": wine,
        "threshold_percent": threshold,
        "cpu_per_frame_ratio": ratio,
        "frame_ratio": frame_ratio,
        "passed": (ratio is not None and ratio <= 1 + threshold / 100
                   and frame_ratio >= 0.9),
    }


def nonnegative(value):
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise argparse.ArgumentTypeError("expected a finite nonnegative number")
    return number


def positive(value):
    number = nonnegative(value)
    if number == 0:
        raise argparse.ArgumentTypeError("expected a positive number")
    return number


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adv-run", required=True, type=Path)
    parser.add_argument("--wine", required=True, type=Path, help="Wine distribution")
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--seconds", type=positive, default=40)
    parser.add_argument("--old-prefix", required=True, type=Path,
                        help="old stack install root (adv-run --install-dir)")
    parser.add_argument("--old-data-dir", required=True, type=Path)
    parser.add_argument("--wine-prefix", required=True, type=Path)
    parser.add_argument("--wine-data-dir", required=True, type=Path)
    parser.add_argument("--threshold", type=nonnegative, default=5,
                        help="allowed CPU per frame increase, percent (default: 5)")
    parser.add_argument("--json", type=Path, help="also write comparison numbers")
    args = parser.parse_args(argv)
    common = [str(args.adv_run.resolve()), str(args.exe.resolve()),
              "--headless", "--headless-gpu", "--audio", "--auto-dialog-ms", "1000",
              "--seconds", str(args.seconds)]
    try:
        old = run(common + ["--install-dir", str(args.old_prefix),
                            "--data-dir", str(args.old_data_dir)])
        wine = run(common + ["--wine", str(args.wine), "--wine-prefix", str(args.wine_prefix),
                             "--data-dir", str(args.wine_data_dir)])
        result = compare(old, wine, args.threshold)
        if args.json:
            args.json.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n",
                                 encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"parity: {error}", file=sys.stderr)
        return 2

    def display(value):
        return "n/a" if value is None else f"{value:.6g}"

    print(f"{'PASS' if result['passed'] else 'FAIL'} "
          f"old_cpu/frame={display(old['cpu_per_frame_seconds'])}s "
          f"wine_cpu/frame={display(wine['cpu_per_frame_seconds'])}s "
          f"ratio={display(result['cpu_per_frame_ratio'])} "
          f"frames={old['frames']}/{wine['frames']}")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
