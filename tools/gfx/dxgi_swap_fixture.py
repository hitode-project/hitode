#!/usr/bin/env python3
"""Reduce the interactive DXGI swap-chain and occlusion measurements."""
import argparse
from pathlib import Path

FIXTURE = Path(__file__).resolve().parents[2] / "crates/adv-dx/src/dxgi/swap/windows.tsv"
MODELS = {"discard": "0", "sequential": "1", "flip-sequential": "3", "flip-discard": "4"}


def fixture(semantics, occlusion):
    rows = []
    for line in semantics.splitlines():
        match line.split("\t"):
            case ["swap", model, "count", count, "format", format_id, hr, "present", _]:
                rows.append(("create", MODELS[model], count, format_id, hr))
            case ["swap-buffer", model, "usage", _, "bind", _, "misc", _, "cpu", _, "buffer1", hr]:
                rows.append(("buffer", MODELS[model], hr))
    for line in occlusion.splitlines():
        fields = line.split("\t")
        if fields[0] == "present":
            _, model, state, _, hr = fields[:5]
            row = ("present", MODELS[model], state, hr)
            if row not in rows:
                rows.append(row)
    return "".join("\t".join(row) + "\n" for row in rows)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("semantics", type=Path)
    parser.add_argument("occlusion", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = fixture(args.semantics.read_text(), args.occlusion.read_text())
    if args.check:
        if generated != FIXTURE.read_text():
            raise SystemExit("DXGI swap fixture differs")
    else:
        FIXTURE.write_text(generated)
