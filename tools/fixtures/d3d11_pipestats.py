#!/usr/bin/env python3
"""Derive compact counter oracles from the authored D3D11 statistics probes."""

import argparse
from pathlib import Path

COUNTERS = "IAV IAP VS GS GSP C CP PS HS DS CS".split()
CASES = {
    "full": "full 64x64 quad, 4-vertex strip",
    "repeat": "same quad twice",
    "offscreen": "quad fully off screen (clipped)",
    "half": "quad half off screen",
    "far": "quad beyond far plane",
    "no_ps": "null pixel shader",
    "depth_fail": "depth LESS fails every pixel (depth 0)",
    "depth_output": "SV_Depth PS, LESS fails every pixel",
    "samples": "4x MSAA, SV_SampleIndex PS",
    "instances": "3 vertices x 5 instances",
    "incomplete": "list of 5 vertices (incomplete triangle)",
    "index_instances": "indexed: 54 indices x 2 instances",
    "geometry": "GS x2 instances, 2 triangles out each, 2 input triangles",
    "tessellation": "2 tri patches, factor 2",
    "dispatch": "dispatch 3x2x1 of 8x4x1",
    "empty_dispatch": "dispatch 0x2x1",
    "empty": "empty query",
    "early_uav": "1x OM UAV store earlydepthstencil, depth LESS fails",
    "late_uav": "1x OM UAV store, depth LESS fails",
    "stream_only": "GS stream output, no rasterized stream",
}


def load(paths):
    rows = {}
    for path in paths:
        for line in path.read_text().splitlines():
            fields = line.split("\t")
            if fields[0] == "stats":
                _, driver, name, values = fields
                words = values.split()
                rows[name, driver] = dict(zip(words[::2], map(int, words[1::2]), strict=True))
    return rows


def fixture(rows):
    lines = ["case\t" + "\t".join(COUNTERS)]
    for key, name in CASES.items():
        hw, warp = (rows[name, d] for d in ("hw", "warp"))
        # IA and compute follow the issue counts. VS uses the no-cache value.
        # The hardware DS count is the number of unique domain points.
        # Clipping uses the guard band (one output for each surviving input).
        values = {**dict.fromkeys(COUNTERS, 0), **hw}
        values["VS"] = values["IAV"]
        values["CP"] = min(hw["CP"], warp["CP"])
        if key in ("early_uav", "late_uav", "stream_only"):
            values["IAP"] = hw["IAV"] - 2  # four vertices in one triangle strip
        # HW reports non-helper invocations, including samples at sample
        # frequency. WARP supplies the observed helper-inclusive counts.
        ps = hw["PS"]
        values["PS"] = str(ps)
        if key == "depth_fail":
            values["PS"] = f"{ps}..4096"
        lines.append("\t".join([key, *(str(values[c]) for c in COUNTERS)]))
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pipestats", type=Path)
    parser.add_argument("pipestats2", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    out = (Path(__file__).resolve().parents[2]
           / "crates/adv-dx/src/shader/expand/pipestats.tsv")
    generated = fixture(load((args.pipestats, args.pipestats2)))
    if args.check:
        if out.read_text() != generated:
            parser.exit(1, "pipeline statistics fixture differs\n")
    else:
        out.write_text(generated)


if __name__ == "__main__":
    main()
