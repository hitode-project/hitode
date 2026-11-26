"""Reduce the authored D3D11 ALU probe to compact, reproducible edge vectors."""

import argparse
import csv
import math
import re
import struct
from pathlib import Path

ROUNDING = [
    0x477FEFFF, 0x477FF000, 0x477FE000, 0x3F801000, 0x3F803000, 0x3F801001,
    0xC77FF000, 0x33800000, 0x33000000, 0x33C00000, 0x33400000, 0x387FC000,
    0x38800000, 0x3F800FFF, 0x477FFFFF, 0x7F7FFFFF,
]
FLOAT_BINARY = {
    "add", "mul", "div", "mad-a*b+1", "min", "max", "eq", "ne", "lt", "ge",
    "dp2-(a,b)(b,a)",
}
FLOAT_UNARY = {
    "mov-sat", "neg-abs", "rcp", "rsq", "sqrt", "exp", "log", "sin", "cos", "frc",
    "round_ne", "round_ni", "round_pi", "round_z", "ftoi", "ftou", "f32tof16",
}
INT_BINARY = {
    "ishl", "ishr", "ushr", "imul", "udiv", "urem", "imin", "umax", "ilt", "ult",
    "iadd", "bfi-8-at-4",
}
INT_UNARY = {
    "itof", "utof", "f16tof32", "countbits", "firstbit_hi", "firstbit_lo", "firstbit_shi",
    "bfrev", "ineg", "not", "ubfe-8-at-4", "ibfe-8-at-4",
}


def bits_float(bits):
    return struct.unpack("<f", struct.pack("<I", bits))[0]


def probe_values(source, name):
    declaration = re.search(rf"DWORD {name}\[N\] = \{{(.*?)\}};", source, re.S)
    return [int(word, 0) for word in re.findall(r"0x[0-9a-f]+|\d+", declaration[1])]


def cases(rows, floats, integers):
    for name, hardware in rows["hw"].items():
        warp = rows["warp"][name]
        if name in FLOAT_BINARY or name in INT_BINARY:
            values = floats if name in FLOAT_BINARY else integers
            for i, a in enumerate(values):
                # Diagonal and boundary combinations, including the degenerate denominators.
                for j in sorted({0, 1, i, (i * 11 + 3) % 32, 31 - i}):
                    yield name, [a, values[j]], hardware[i][j], warp[i][j]
        elif name in FLOAT_UNARY or name in INT_UNARY:
            values = floats if name in FLOAT_UNARY else integers
            for i, a in enumerate(values):
                yield name, [a], hardware[i][0], warp[i][0]
        elif name == "f32tof16-rounding":
            for j, a in enumerate(ROUNDING):
                yield "f32tof16", [a], hardware[0][j], warp[0][j]


def vectors(rows, floats, integers):
    float_output = (FLOAT_BINARY - {"eq", "ne", "lt", "ge"}) | {
        "mov-sat", "neg-abs", "rcp", "rsq", "sqrt", "exp", "log", "sin", "cos", "frc",
        "round_ne", "round_ni", "round_pi", "round_z", "itof", "utof", "f16tof32",
    }
    for name, args, hardware, warp in cases(rows, floats, integers):
        mode = "bits"
        if name in {"eq", "ne", "lt", "ge", "ilt", "ult"}:
            hardware = 0xFFFFFFFF if hardware else 0
            warp = 0xFFFFFFFF if warp else 0
        if name in {"firstbit_hi", "firstbit_shi"} and hardware != 0xFFFFFFFF:
            hardware = warp = 31 - hardware
        if name == "div":
            denominator = abs(bits_float(args[1]))
            if not 2**-126 <= denominator <= 2**126:
                continue
            mode = "ulp2"
        if name in {"sin", "cos"}:
            x = bits_float(args[0])
            if math.isfinite(x) and abs(x) > 100 * math.pi:
                continue
            mode = "trig"
        if name in {"rcp", "rsq", "sqrt", "exp", "log"}:
            mode = "ulp2"
        if name == "mad-a*b+1":
            mode = "either"
        if name in {"min", "max"} and hardware != warp:
            mode = "either"
        if name == "f32tof16" and hardware & 0x7FFF > 0x7C00:
            mode = "half_nan"
        if name in float_output and math.isnan(bits_float(hardware)) and math.isnan(bits_float(warp)):
            mode = "nan"
        yield [name, mode, f"{hardware:08x}", f"{warp:08x}", *(f"{word:08x}" for word in args)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump", type=Path)
    parser.add_argument("probe", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    source = args.probe.read_text()
    rows = {"hw": {}, "warp": {}}
    with args.dump.open() as stream:
        for tag, *fields in csv.reader(stream, delimiter="\t"):
            if tag == "op" and not fields[1].startswith("d:"):
                driver, name, _, row, *results = fields
                rows[driver].setdefault(name, {})[int(row)] = [int(value, 16) for value in results]
    with args.output.open("w", newline="") as stream:
        csv.writer(stream, delimiter="\t", lineterminator="\n").writerows(
            vectors(rows, probe_values(source, "fvals"), probe_values(source, "ivals"))
        )


if __name__ == "__main__":
    main()
