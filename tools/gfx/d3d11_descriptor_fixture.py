#!/usr/bin/env python3
"""Reduce d3d11_descriptors probe output to guest descriptions and expected results."""
import argparse
from collections import defaultdict
from pathlib import Path

FIXTURE = (Path(__file__).resolve().parents[2] /
           "crates/adv-dx/src/d3d11/desc/windows.tsv")


def fixture(dump):
    drivers = defaultdict(set)
    request = None
    for line in dump.splitlines():
        kind, *fields = line.split("\t")
        if kind == "device":
            driver, level, result = fields
            if result != "00000000":
                raise ValueError(line)
        elif kind == "request":
            request = tuple(fields)
        else:
            name, result, *output = fields
            if kind == "view" and output:
                format_id, dimension, *words = output
                used_words = {
                    ("0", "4"): 2,
                    ("0", "5"): 4,
                    ("1", "4"): 1,
                    ("1", "5"): 3,
                    ("2", "5"): 3,
                }[request[0], dimension]
                output = [format_id, dimension, *words[:used_words]]
            drivers[kind, name, level, request, result, tuple(output)].add(driver)
    rows = defaultdict(list)
    for (kind, name, level, request, result, output), measured in drivers.items():
        if measured != {"hw", "warp"}:
            raise ValueError(f"hardware and WARP differ: {kind} {name} {level}")
        rows[kind, name, request, result, output].append(level)
    return "".join(
        "\t".join((kind, name, ",".join(levels), result, " ".join(request), " ".join(output))).rstrip() + "\n"
        for (kind, name, request, result, output), levels in rows.items()
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = fixture(args.dump.read_text())
    if args.check:
        if generated != FIXTURE.read_text():
            raise SystemExit("Direct3D 11 descriptor fixture differs")
    else:
        FIXTURE.write_text(generated)
