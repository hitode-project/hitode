#!/usr/bin/env python3
"""Cross-check feature-level and format data against the external Windows probes."""
import argparse
import csv
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "data/gfx"


def rows(path):
    return (line for line in path.read_text().splitlines() if line and not line.startswith("#"))


def table(path):
    return csv.DictReader(rows(path), delimiter="\t")


def constants(name):
    owner = f"Windows.Win32.Graphics.Direct3D11.{name}"
    return {
        constant.removeprefix(name + "_"): int(bits, 16)
        for group, constant, bits, _ in (line.split("\t") for line in rows(ROOT / "data/abi/constants.tsv"))
        if group == owner
    }


def support(mask, enums):
    return {name for name, bit in enums.items() if int(mask, 16) & bit}


def columns(row, column):
    return set(row[column].split(",")) - {"-"}


def check_formats(caps):
    support1 = constants("D3D11_FORMAT_SUPPORT")
    support2 = constants("D3D11_FORMAT_SUPPORT2")
    docs = {row["format"]: row for row in table(DATA / "format-support-fl11_0.tsv")}
    gaps = {(row["driver"], row["format"]): columns(row, "missing")
            for row in table(DATA / "format-oracle-gaps.tsv")}
    layouts = (ROOT / "crates/adv-dx/src/dxgi/formats.rs").read_text()
    layout_rows = {
        number: (name, int(bits), cast)
        for number, name, cast, bits in re.findall(
            r'format: (\d+),\s+name: "(\w+)",\s+parent: \d+,\s+cast_family: (\w+),'
            r"\s+component_bits: \[[^]]+\],\s+unit_bits: (\d+),", layouts
        )
    }
    for number, spec in docs.items():
        name, bits, cast = layout_rows[number]
        assert (name, bits) == (spec["name"], int(spec["bits_per_element"])), spec["name"]
        if "CAST_WITHIN_BIT_LAYOUT" in columns(spec, "required"):
            assert cast != "D3DFCS_UNKNOWN", name
    statuses = {"hardware": {}, "warp": {}}
    optional = {"hardware": set(), "warp": set()}
    for row in caps:
        if row[0] != "format":
            continue
        _, device, number, hr1, mask1, hr2, mask2, *samples = row
        driver, level = device.split("/")
        if level != "b000" or driver not in statuses:
            continue
        statuses[driver][number] = hr1 == "00000000"
        assert (hr1 == "00000000") == (int(mask1, 16) != 0), row
        assert (hr2 == "00000000") == (int(mask2, 16) != 0), row
        if number not in docs:
            continue
        observations = support(mask1, support1) | support(mask2, support2)
        for sample in samples:
            count, hr, quality = sample.split(":")
            if hr == "00000000" and int(quality) and "MULTISAMPLE_RENDERTARGET" in observations:
                observations.add(f"MSAA_{count}X" if count in ("4", "8") else "MSAA_OTHER")
        # The operation-wide rows do not have CheckFormatSupport flag bits.
        comparable = support1.keys() | support2.keys() | {"MSAA_4X", "MSAA_8X", "MSAA_OTHER"}
        optional[driver].update((number, bit) for bit in
                                columns(docs[number], "optional") & observations)
        required = columns(docs[number], "required") & comparable
        missing = gaps.pop((driver, number), set())
        expected = required if missing == {"*"} else missing
        assert required - observations == expected, (driver, docs[number]["name"], required - observations, expected)
    assert not gaps, gaps
    for driver, supported in (("hardware", 113), ("warp", 112)):
        assert len(statuses[driver]) == 192 and sum(statuses[driver].values()) == supported, driver
        print(f"{driver}: {supported} formats supported, {len(optional[driver])} published optional bits present.")


def check_levels(caps, lowlevel, io_regs):
    levels = {row["requirement"]: row for row in table(DATA / "feature-levels.tsv")}
    level_names = {code: level for level, code in levels["feature_level"].items()
                   if level != "requirement"}
    for driver in ("hardware", "warp"):
        created = {row[2]: row[3:] for row in caps if row[:2] == ["create", driver]}
        for code, level in level_names.items():
            assert created[f"only-{code}"] == ["00000000", code], (driver, level)
            (compute,) = [row[3:] for row in caps if row[:3] == ["feature", f"{driver}/{code}", "4"]]
            if not levels["compute_profile"][level].startswith("optional"):
                assert compute == ["00000000", str(int(levels["compute_profile"][level] != "-"))], (driver, level)
    for driver, level, mirror, dual in lowlevel:
        level = level.removeprefix("FL ")
        for feature, result in (("mirror_once", mirror), ("dual_source_blend", dual)):
            assert (result.split()[-1] == "00000000") == (levels[feature][level] == "1"), (driver, level, feature)
    for level, profile, varyings, _, result, *_ in io_regs:
        level = level.removeprefix("FL ")
        if profile != "ps_" + levels["vs_ps_profile"][level]:
            continue
        registers = 1 + int(varyings.split()[2])
        assert (result.split()[-1] == "00000000") == (registers <= int(levels["ps_input_registers"][level])), (level, registers)


def check_feature_sizes(caps):
    layouts = ROOT / "data/abi/gfx-layout.tsv"
    sizes = {name: int(size) for name, field, size in (row.split("\t") for row in rows(layouts))
             if field == "size"}
    features = {str(value): name for name, value in constants("D3D11_FEATURE").items()}
    for row in caps:
        if row[0] != "feature" or row[1] not in ("hardware/b000", "warp/b000"):
            continue
        _, _, index, status, *values = row
        if status == "00000000":
            name = "D3D11_FEATURE_DATA_" + features[index]
            assert sizes[name] == len(values) * 4, (name, values)


def downlevel_formats(caps):
    levels = ("9100", "9200", "9300", "a000", "a100")
    measured = {}
    for row in caps:
        if row[0] != "format":
            continue
        _, device, number, _, mask1, _, mask2, *samples = row
        driver, level = device.split("/")
        if driver not in ("hardware", "warp") or level not in levels:
            continue
        sample_mask = 0
        for sample in samples:
            count, hr, quality = sample.split(":")
            if int(count) in (2, 4, 8) and hr == "00000000" and int(quality):
                sample_mask |= int(count)
        old = measured.get((int(number), level), (0, 0, 0))
        measured[int(number), level] = (old[0] | int(mask1, 16), old[1] | int(mask2, 16), old[2] | sample_mask)
    lines = [
        "# D3D11 caps.tsv Win11 x86 hardware/WARP union. Each cell: FORMAT_SUPPORT, FORMAT_SUPPORT2, sample mask (hex).",
        "format\t" + "\t".join(levels),
    ]
    for number in sorted({number for number, _ in measured}):
        cells = [measured[number, level] for level in levels]
        if any(any(cell) for cell in cells):
            lines.append(f"{number}\t" + "\t".join(",".join(f"{mask:x}" for mask in cell) for cell in cells))
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("probes", nargs="?", type=Path,
                        default=Path.home() / "adv-wine-assets/probes/gfx")
    parser.add_argument("--write-downlevel", action="store_true")
    args = parser.parse_args()
    if not (args.probes / "caps.tsv").is_file():
        print("Skipped Windows capability checks (caps.tsv is unavailable).")
        return
    caps = [line.split("\t") for line in rows(args.probes / "caps.tsv")]
    lowlevel_path = args.probes / "d3d11_lowlevel.tsv"
    io_regs_path = args.probes / "d3d11_io_regs.tsv"
    lowlevel = [line.split("\t") for line in rows(lowlevel_path)] if lowlevel_path.is_file() else []
    io_regs = [line.split("\t") for line in rows(io_regs_path)
               if line.startswith("FL ")] if io_regs_path.is_file() else []
    check_formats(caps)
    check_levels(caps, lowlevel, io_regs)
    check_feature_sizes(caps)
    downlevel = DATA / "format-support-downlevel.tsv"
    if args.write_downlevel:
        downlevel.write_text(downlevel_formats(caps))
    assert downlevel.read_text() == downlevel_formats(caps), "downlevel format mask mismatch"
    print("Windows capability checks passed for hardware and WARP.")


if __name__ == "__main__":
    main()
