#!/usr/bin/env python3
"""Reduce D3D11 creation and ownership probes to matching hardware and WARP rows."""
import argparse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "crates/adv-dx/src/d3d11"
FIXTURES = {"creation": ROOT / "caps/creation.tsv", "objects": ROOT / "object/windows.tsv"}


def fixture(dump, probe):
    drivers = {}
    for line in dump.splitlines():
        kind, *fields = line.split("\t")
        if kind == "driver":
            driver, = fields
            drivers[driver] = []
        elif probe == "creation" and kind == "create":
            drivers[driver].append("\t".join(fields))
        elif probe == "objects":
            drivers[driver].append(line)
        else:
            raise ValueError(line)
    if set(drivers) != {"hw", "warp"} or drivers["hw"] != drivers["warp"]:
        raise ValueError("hardware and WARP results differ")
    return "\n".join(drivers["hw"]) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump", type=Path)
    parser.add_argument("--probe", choices=FIXTURES, default="creation")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = fixture(args.dump.read_text(), args.probe)
    path = FIXTURES[args.probe]
    if args.check:
        if generated != path.read_text():
            raise SystemExit("Direct3D 11 creation fixture differs")
    else:
        path.write_text(generated)
