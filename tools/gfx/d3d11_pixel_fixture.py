#!/usr/bin/env python3
"""Extract the guest raster and sampling rows from the Windows D3D11 probe."""
import argparse
from pathlib import Path

FIXTURE = (
    Path(__file__).resolve().parents[2]
    / "crates/adv-dx/src/d3d11/glue/tests/pixels.tsv"
)
SAMPLING = FIXTURE.with_name("sampling.tsv")


def fixture(capture: str) -> str:
    rows = []
    for line in capture.splitlines():
        match line.split("\t"):
            case ["px", "hw", name, y, *pixels] if name.startswith(
                ("quad-", "tex-", "viewport-", "shared-edges-", "lines-", "points-")
            ):
                rows.append("\t".join((name, y, *pixels)) + "\n")
            case ["px", driver, name, y, *pixels] if name.startswith(
                ("filter-", "convert-", "clear-", "blend-", "msaa")
            ):
                rows.append("\t".join((f"{driver}:{name}", y, *pixels)) + "\n")
    return "".join(rows)


def sampling(capture: str) -> str:
    rows = []
    for line in capture.splitlines():
        match line.split("\t"):
            case ["row", driver, name, *pixels] if name.startswith(
                ("lod-", "address-", "srgb-", "depth-", "stencil-", "bc", "cmp-", "aniso")
            ):
                rows.append("\t".join((f"{driver}:{name}", *pixels)) + "\n")
    return "".join(rows)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--sampling", action="store_true")
    args = parser.parse_args()
    capture = args.capture.read_text()
    result, path = (sampling(capture), SAMPLING) if args.sampling else (fixture(capture), FIXTURE)
    if args.check:
        if result != path.read_text():
            raise SystemExit(f"D3D11 fixture differs: {path.name}")
    else:
        path.write_text(result)
