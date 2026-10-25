#!/usr/bin/env python3
"""Compile pinned upstream PNG row bodies for admission/execution checks.

Only the static linkage and type preamble are adapted. Function bodies and the
row-info layout come from libpng 1.6.43 unchanged. No generated code is vendored.
Run with clang (including cross-target support); artifacts stay under target/.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
BASE = "https://raw.githubusercontent.com/pnggroup/libpng/v1.6.43/"
SOURCES = {
    "pngrutil.c": "a606ae5b03b2c58a48096f6baba0816d54ac293fe65fdb146a43cb856a17ba3f",
    "png.h": "4be1482ccb03edbb14998345fa47cfe7ceca0463767032ab728c12f941ae0558",
    "LICENSE": "7317e078e2d3b5d7ba5a6159e650945153262b44b76f6700f8e9edb261c5143e",
}
FUNCTIONS = ["sub", "up", "avg", "paeth_1byte_pixel", "paeth_multibyte_pixel"]


def fetch(directory, name, digest):
    path = directory / name
    data = path.read_bytes() if path.exists() else urllib.request.urlopen(BASE + name).read()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError(f"source checksum mismatch: {name}")
    path.write_bytes(data)
    return data.decode()


def function(source, name):
    start = source.index("static void\n" + name + "(")
    end = source.index("\n}", start) + 2
    return source[start:end].replace("static void", "void", 1)


def text_section(path):
    data = path.read_bytes()
    machine, count = struct.unpack_from("<HH", data)
    if machine != 0x14C:
        raise ValueError("reference must be x86-32 COFF")
    optional = struct.unpack_from("<H", data, 16)[0]
    for n in range(count):
        offset = 20 + optional + 40 * n
        name = data[offset:offset + 8].rstrip(b"\0")
        if name == b".text":
            size, start = struct.unpack_from("<II", data, offset + 16)
            relocations = struct.unpack_from("<H", data, offset + 32)[0]
            if relocations:
                raise ValueError("row body unexpectedly requires relocations")
            return data[start:start + size]
    raise ValueError("missing .text")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cc", default="clang")
    parser.add_argument("--output", type=Path, default=ROOT / "target/codec-reference/png")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    source = {name: fetch(args.output, name, digest) for name, digest in SOURCES.items()}
    header = source["png.h"]
    start = header.index("typedef struct png_row_info_struct")
    end = header.index("} png_row_info;", start) + len("} png_row_info;")
    preamble = (
        "/*\n" + source["LICENSE"] + "\n*/\n"
        "/* Extracted reference; static linkage/type preamble adapted by Hitode. */\n"
        "/* See accompanying LICENSE and pinned pngrutil.c for provenance. */\n"
        "typedef unsigned char png_byte; typedef unsigned int png_uint_32;\n"
        "typedef __SIZE_TYPE__ size_t; typedef png_byte *png_bytep;\n"
        "typedef const png_byte *png_const_bytep;\n"
        "#define PNG_UNUSED(x) (void)(x);\n"
        + header[start:end] + "\ntypedef png_row_info *png_row_infop;\n"
    )
    report = {
        "upstream": BASE,
        "sources": SOURCES,
        "compiler": subprocess.check_output([args.cc, "--version"], text=True).splitlines()[0],
        "artifacts": [],
    }
    for name in FUNCTIONS:
        c = args.output / (name + ".c")
        c.write_text(preamble + function(source["pngrutil.c"], "png_read_filter_row_" + name) + "\n")
        # O1/O2 alter scheduling and allocation; Os is held out from O2 rule work.
        for optimization in ["O1", "O2", "Os"]:
            stem = name + "-" + optimization
            obj = args.output / (stem + ".obj")
            command = [args.cc, "--target=i686-pc-windows-msvc", "-" + optimization,
                       "-fno-vectorize", "-fno-slp-vectorize", "-fno-unroll-loops",
                       "-ffreestanding", "-fno-builtin", "-c", str(c), "-o", str(obj)]
            subprocess.run(command, check=True)
            code = text_section(obj)
            (args.output / (stem + ".bin")).write_bytes(code)
            report["artifacts"].append({"name": stem, "bytes": len(code), "sha256": hashlib.sha256(code).hexdigest()})
    (args.output / "build.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"built {len(report['artifacts'])} references in {args.output}")


if __name__ == "__main__":
    main()
