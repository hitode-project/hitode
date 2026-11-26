#!/usr/bin/env python3
"""Condense the Windows ddraw_blt.c output into one row per probe case."""

import argparse
from pathlib import Path

PAIRS = ("vid-vid", "sys-sys", "sys-vid", "vid-sys")
OUTPUT = (
    Path(__file__).resolve().parents[2]
    / "crates/adv-device/src/surface/blt/expectations.tsv"
)


def expectations(source):
    caps = []
    cases = {}
    for line in source.read_text().splitlines():
        kind, pair, name, value, *rows = line.split("\t")
        if kind == "caps":
            assert rows[0] == "ckey"
            caps.append(f"{pair}\t{rows[1]}")
            continue
        assert kind == "blt" and pair in PAIRS
        if name.startswith("getcolorkey"):
            result = f"{value}:{rows[0]}"
        elif rows:
            pixels = ".".join(pixel.lstrip("0") or "0" for row in rows for pixel in row.split("."))
            result = f"{value}:{pixels}"
        else:
            result = value
        case = cases.setdefault(name, {})
        assert pair not in case
        case[pair] = result
    header = ["# ddraw_blt.c: HRESULT[:row-major hex pixels], or low:high key.",
              "# Columns: vid-vid, sys-sys, sys-vid, vid-sys. =N repeats column N (zero-based)."]
    rows = []
    for name, case in cases.items():
        results = [case[pair] for pair in PAIRS]
        fields = [result if results.index(result) == i else f"={results.index(result)}"
                  for i, result in enumerate(results)]
        rows.append("\t".join([name, *fields]))
    return "\n".join([*header, *caps, *rows]) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    expected = expectations(args.dump)
    if args.check:
        if OUTPUT.read_text() != expected:
            raise SystemExit("Blt expectations differ from the probe")
    else:
        OUTPUT.write_text(expected)


if __name__ == "__main__":
    main()
