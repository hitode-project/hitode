#!/usr/bin/env python3
"""Extract the guest raster and sampling rows from the Windows D3D11 probe."""
import argparse
from pathlib import Path

FIXTURE = (
    Path(__file__).resolve().parents[2]
    / "crates/adv-dx/src/d3d11/glue/tests/pixels.tsv"
)


def fixture(capture: str) -> str:
    rows = []
    for line in capture.splitlines():
        match line.split("\t"):
            case ["px", "hw", name, y, *pixels] if name.startswith(
                ("quad-", "tex-", "viewport-", "shared-edges-")
            ):
                rows.append("\t".join((name, y, *pixels)) + "\n")
    return "".join(rows)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = fixture(args.capture.read_text())
    if args.check:
        if result != FIXTURE.read_text():
            raise SystemExit("D3D11 pixel fixture differs")
    else:
        FIXTURE.write_text(result)
