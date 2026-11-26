#!/usr/bin/env python3
"""Extract compact Device7 oracle rows from the authored Windows probes."""
import argparse
from pathlib import Path


def rows(source, name):
    for line in (source / f"{name}.tsv").read_text().splitlines():
        yield line.split("\t")


def expectations(source):
    raster = {
        "quad-int-1-1-5-5", "quad-half-1.5-1.5-5.5-5.5",
        "quad-0.49-0.49-4.49-4.49", "quad-0.51-0.51-4.51-4.51",
        "quad-thin-1-1-5-1.4", "shared-edge-additive-0x40",
        "line-1,1-6,4-points-1.5,6.5-3,6", "tex-1to1-int-point",
        "tex-1to1-minus-half-point", "tex-1to1-int-linear",
        "tex-1to1-minus-half-linear", "tex-2x-int-linear",
        "tex-wrap-uv-minus-0.5-1.5", "subtract-half-0x40",
    }
    for row in rows(source, "d3d7_pixels"):
        if row[0] == "px" and row[2] in raster:
            if row[1] == "rgb" and row[2] not in {
                "line-1,1-6,4-points-1.5,6.5-3,6", "subtract-half-0x40"
            }:
                continue
            yield [*row[:4], "".join(row[4:])]
    for row in rows(source, "d3d7_tnl"):
        if row[0] == "vx" and row[2] == "directional-diffuse-6deg-steps":
            yield [*row[:3], "".join(row[3:])]
        elif row[0] == "vx" and row[2].startswith("vertex-blend-1weight-"):
            yield row
        elif row[0] == "vx" and row[1] == "hal" and row[2] == "texgen-cameraspaceposition-u=x/32":
            yield ["stage", *row[1:3], "0", "".join(row[3:])]
        if row[0] == "px" and row[2] in {
            "z-coplanar-less", "z-coplanar-less-zbias1", "z-coplanar-less-zbias16",
            "texture-565-0x8410-expanded", "texture-1555-0x4210-expanded",
            "texture-4444-0x8fff-srcalpha-over-404040",
        } and row[1] == "hal":
            yield ["stage", *row[1:3], "0", row[3]]
    for row in rows(source, "d3d7_colorkey"):
        if row[0] == "setkey":
            yield row
        elif row[0] == "ck":
            yield [*row[:4], "".join(row[4:])]
    for row in rows(source, "d3d7_semantics"):
        if row[0] in {"device7", "zformat"}:
            yield row
        elif row[0] == "colorkey" and row[2].startswith("mode"):
            yield [*row[:5], "".join(row[5:])]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    for row in expectations(args.source):
        print("\t".join(row))


if __name__ == "__main__":
    main()
