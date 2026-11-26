#!/usr/bin/env python3
"""Reduce the Direct3D 11 Map matrix to accepted modes and flags."""
import argparse
from collections import defaultdict
from pathlib import Path

FIXTURE = (Path(__file__).resolve().parents[2] /
           "crates/adv-dx/src/d3d11/object/map/windows.tsv")
FLAGS = (0, 0x100000, 1)


def fixture(dump):
    results = defaultdict(dict)
    unavailable = defaultdict(set)
    for line in dump.splitlines():
        row = line.split("\t")
        match row:
            case ["device", driver, level, "00000000"]:
                pass
            case ["resource", name, "80070057"]:
                unavailable[level, name].add(driver)
            case ["map", name, mode, flags, result]:
                results[level, name, int(mode), int(flags)][driver] = int(result, 16)
            case _:
                raise ValueError(line)
    masks = defaultdict(int)
    for (level, name, mode, flags), by_driver in results.items():
        if set(by_driver) != {"hw", "warp"} or len(set(by_driver.values())) != 1:
            raise ValueError((level, name, mode, flags, by_driver))
        result = by_driver["hw"]
        if result not in {0, 0x80070057}:
            raise ValueError((level, name, mode, flags, result))
        bit = (mode - 1) * len(FLAGS) + FLAGS.index(flags)
        masks[level, name] |= (result == 0) << bit
    rows = defaultdict(list)
    for (level, name), mask in masks.items():
        rows[name, f"{mask:04x}"].append(level)
    for (level, name), drivers in unavailable.items():
        if drivers != {"hw", "warp"}:
            raise ValueError((level, name, drivers))
        rows[name, "-"].append(level)
    return "".join(f"{name}\t{','.join(levels)}\t{mask}\n" for (name, mask), levels in rows.items())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = fixture(args.dump.read_text())
    if args.check:
        if generated != FIXTURE.read_text():
            raise SystemExit("Direct3D 11 Map fixture differs")
    else:
        FIXTURE.write_text(generated)
