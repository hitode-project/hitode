#!/usr/bin/env python3
"""Extract the Windows reference rows used by the modern GPU executor tests."""

import argparse
from pathlib import Path

OUTPUT = (Path(__file__).resolve().parents[1]
          / "crates/adv-device/src/gpu/native/modern/probes.tsv")


def rows(path):
    results = {}
    for line in path.read_text().splitlines():
        kind, driver, name, *values = line.split("\t")
        if kind not in {"px", "row"}:
            continue
        row = int(values.pop(0)) if kind == "px" else 0
        results[(name, row, driver)] = values
    return results


def main():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument("probes", type=Path)
    probes = args.parse_args().probes
    output = ["# D3D11 pixel and sampling probe expectations"]
    pixels = rows(probes / "d3d11_pixels.tsv")
    sampling = rows(probes / "d3d11_sampling.tsv")
    for (name, row, driver), warp in pixels.items():
        if driver != "warp":
            continue
        hw = pixels[(name, row, "hw")]
        if name.startswith(("quad-", "viewport-", "lines-", "points-")):
            if hw != warp or any(v not in {"00000000", "ffffffff"} for v in warp):
                raise ValueError(f"inconsistent coverage oracle {name} {row}")
            mask = sum(1 << i for i, value in enumerate(warp) if value == "ffffffff")
            output.append(f"{name}\t{row}\t{mask:x}")
        elif name.startswith(("tex-", "shared-edges-")):
            if hw != warp:
                raise ValueError(f"inconsistent texel mapping oracle {name} {row}")
            output.append("\t".join([name, str(row), *warp]))
        elif name.startswith(("convert-", "clear-127.5-", "msaa4-", "msaa8-", "filter-ramp-", "blend-")):
            if name == "convert-r8snorm":
                if hw[8] != "00":
                    raise ValueError("the hardware NaN-to-SNORM result must be zero")
                warp[8] = hw[8]
            output.append("\t".join([name, str(row), *warp]))
    for (name, row, driver), warp in sampling.items():
        if driver != "warp":
            continue
        depth = name.startswith(("depth-d16-", "depth-d32f-", "depth-d32fs8-"))
        if depth or name.startswith(("srgb-", "bc", "address-wrap", "address-mirror",
                                     "address-clamp", "address-border", "cmp-", "stencil-", "lod-")):
            hw = sampling[(name, row, "hw")]
            exact_lod = name.startswith("lod-mip-point")
            exact_address = name.startswith("address-") and not name.startswith("address-border-linear")
            exact = exact_lod or exact_address or name.startswith((
                "bc1-3color", "bc7-", "cmp-", "stencil-", "depth-d32f-", "depth-d32fs8-",
            ))
            if exact and hw != warp:
                raise ValueError(f"inconsistent exact sampling oracle {name}")
            output.append(f"{name}\t0\t" + "\t".join(warp))
    OUTPUT.write_text("\n".join(output) + "\n")


if __name__ == "__main__":
    main()
