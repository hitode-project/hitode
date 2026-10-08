"""Summarize adv-cpu test logs and judge them against the strict baseline.

Usage:
    python tools/cpu/acceptance.py [--strict] [--baseline FILE] [--json] PATH...

Each PATH is a captured `cargo test` log or a directory searched recursively
for `*.log` and `*.txt` (for example a run-cpu-tests.ps1 output directory).
Logs from one run may be split across files; results are pooled.

Output: passed/failed/ignored totals, crashed test processes, the number of
`tier verification:` lines and failure names. With --strict, failures listed
in the baseline are expected; anything else is reported as new.

Exit status: 0 on acceptance; 1 on any crashed process, tier verification
line, new failure (or any failure without --strict), or when no test result
was found at all.
"""

import argparse
import json
import re
import sys
from pathlib import Path

RESULT = re.compile(
    r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", re.M
)
FAILED = re.compile(r"^test (\S+) \.\.\. FAILED\s*$", re.M)
CRASH = re.compile(r"process didn't exit successfully: `([^`]*)`")
# run-cpu-tests.ps1 reports the device exit status; >= 128 is a signal.
DEVICE = re.compile(r"Device test failed \((\d+)\): (\S+?)[;\s]")
TIER = re.compile(r"tier verification:")
DEFAULT_BASELINE = Path(__file__).with_name("strict_baseline.txt")


def load_baseline(path):
    names = set()
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            names.add(line)
    return names


def log_files(paths):
    for path in map(Path, paths):
        if path.is_dir():
            yield from sorted(
                p for p in path.rglob("*") if p.suffix in (".log", ".txt")
            )
        else:
            yield path


def summarize(texts):
    passed = failed = ignored = tier = 0
    names, crashed = set(), []
    for text in texts:
        for match in RESULT.finditer(text):
            passed += int(match.group(1))
            failed += int(match.group(2))
            ignored += int(match.group(3))
        names.update(FAILED.findall(text))
        tier += len(TIER.findall(text))
        for match in CRASH.finditer(text):
            # Keep the test binary name only; drop directories and the hash.
            binary = re.split(r"[\\/]", match.group(1).split()[0])[-1]
            crashed.append(re.sub(r"-[0-9a-f]{16}(\.exe)?$", "", binary))
        for match in DEVICE.finditer(text):
            if int(match.group(1)) >= 128:
                crashed.append(match.group(2))
    return {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "crashed": sorted(crashed),
        "tier_verification": tier,
        "failures": sorted(names),
    }


def judge(summary, strict, baseline):
    expected = baseline if strict else set()
    new = sorted(set(summary["failures"]) - expected)
    summary["new_failures"] = new
    summary["accepted"] = (
        summary["passed"] + summary["failed"] > 0
        and not summary["crashed"]
        and summary["tier_verification"] == 0
        and not new
    )
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--baseline", default=DEFAULT_BASELINE)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    texts = [
        p.read_text(encoding="utf-8", errors="replace")
        for p in log_files(args.paths)
    ]
    summary = judge(summarize(texts), args.strict, load_baseline(args.baseline))
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print(
            f"{summary['passed']} passed, {summary['failed']} failed, "
            f"{summary['ignored']} ignored, {len(summary['crashed'])} crashed, "
            f"{summary['tier_verification']} tier verification lines"
        )
        for name in summary["crashed"]:
            print(f"crashed: {name}")
        for name in summary["new_failures"]:
            print(f"new failure: {name}")
        print("accepted" if summary["accepted"] else "NOT accepted")
    return 0 if summary["accepted"] else 1


if __name__ == "__main__":
    sys.exit(main())
