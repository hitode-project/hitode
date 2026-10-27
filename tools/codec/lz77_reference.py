#!/usr/bin/env python3
"""Compile pinned zlib 1.1.4 forward copies for admission/execution checks.

The zmemcpy function is unchanged apart from its type preamble. The inflate_fast
copy loop is isolated in a harness: this does not build or replace inflate_fast.
Artifacts, license notices and provenance stay under target/codec-reference/lz77.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import urllib.request

from png_reference import text_section

ROOT = Path(__file__).resolve().parents[2]
BASE = "https://raw.githubusercontent.com/madler/zlib/v1.1.4/"
SOURCES = {
    "inffast.c": "1043e4300e9a81e7897ebac507f4cbd75bdcdabd0a77410538404d5076f151f9",
    "zutil.c": "9ef7ed0e467652c1116864dd9b20a2a0602cd68d92312c55f4d6899ffca8ed18",
    "zlib.h": "7388aeb8cc550dcbc158fd43dc90ee900f5fc6a3d055ceaf4c83aa441e6d387f",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cc", default="clang")
    parser.add_argument("--output", type=Path, default=ROOT / "target/codec-reference/lz77")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    sources = {}
    for name, digest in SOURCES.items():
        path = args.output / name
        data = path.read_bytes() if path.exists() else urllib.request.urlopen(BASE + name).read()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"source checksum mismatch: {name}")
        path.write_bytes(data)
        sources[name] = data.decode()
    notice = sources["zlib.h"].split("*/", 1)[0] + "*/\n"
    preamble = (notice + "/* Extracted reference with type preamble / loop harness added by Hitode. */\n"
                "typedef unsigned char Bytef; typedef unsigned int uInt;\n")
    start = sources["zutil.c"].index("void zmemcpy(")
    end = sources["zutil.c"].index("\n}", start) + 2
    loop = re.search(r"do \{\s+\*q\+\+ = \*r\+\+;\s+\} while \(--c\);", sources["inffast.c"])[0]
    functions = {
        "zmemcpy": sources["zutil.c"][start:end],
        "match_loop": "void match_loop(Bytef *q, Bytef *r, uInt c) {\n"
                      "if (c == 0) return;\n" + loop + "\n}",
    }
    report = {
        "upstream": BASE,
        "sources": SOURCES,
        "compiler": subprocess.check_output([args.cc, "--version"], text=True).splitlines()[0],
        "scope": "unchanged zmemcpy; isolated inflate_fast copy body, not whole inflate_fast",
        "artifacts": [],
    }
    for name, body in functions.items():
        c = args.output / (name + "-reference.c")
        c.write_text(preamble + body + "\n")
        for optimization in ["O1", "O2", "Os"]:
            stem = name + "-" + optimization
            obj = args.output / (stem + ".obj")
            subprocess.run([args.cc, "--target=i686-pc-windows-msvc", "-std=c89", "-" + optimization,
                            "-Wno-deprecated-non-prototype", "-fno-vectorize", "-fno-slp-vectorize",
                            "-fno-unroll-loops", "-ffreestanding", "-fno-builtin",
                            "-c", str(c), "-o", str(obj)], check=True)
            code = text_section(obj)
            (args.output / (stem + ".bin")).write_bytes(code)
            report["artifacts"].append({"name": stem, "bytes": len(code), "sha256": hashlib.sha256(code).hexdigest()})
    (args.output / "build.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"built {len(report['artifacts'])} forward-copy references in {args.output}")


if __name__ == "__main__":
    main()
