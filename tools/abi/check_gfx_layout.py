#!/usr/bin/env python3
"""Check the generated graphics layouts against x86 sizeof and offsetof results."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
from tempfile import TemporaryDirectory

DATA = Path(__file__).resolve().parents[2] / "data/abi"


def compile_probe(source, sdk, output):
    manifest = json.loads((DATA / "sdk-source.json").read_text())
    metadata = manifest["graphics_layout_probe"]
    sdk_header = sdk / Path(metadata["header"]).name
    raw = sdk_header.read_bytes()
    if hashlib.sha256(raw).hexdigest() != metadata["sha256"]:
        raise SystemExit("SDK d3d11.h hash mismatch")
    # MinGW 14 supplies the other records and the shared resource tier enum.
    name = "D3D11_FEATURE_DATA_DISPLAYABLE"
    typedef = re.search(rf"typedef struct {name}\s*\{{[^}}]*\}}\s*{name}\s*;", raw.decode())[0]
    with TemporaryDirectory() as directory:
        headers = Path(directory)
        (headers / "gfx-layout-sdk.h").write_text(typedef + "\n")
        # Use the pinned legacy SDK headers. Modern dependencies come from MinGW.
        for path, expected in manifest["direct3d7"]["headers"].items():
            name = Path(path).name
            data = (sdk / name).read_bytes()
            if hashlib.sha256(data).hexdigest() != expected:
                raise SystemExit(f"SDK header hash mismatch: {name}")
            (headers / name).write_bytes(data)
        subprocess.run(["i686-w64-mingw32-gcc", "-I", headers, source, "-o", output], check=True)


def check(path):
    source = (DATA / "structs.rs").read_text().split("// Graphics records.\n")[1]
    expected = {
        (name, kind): int(value)
        for kind, name, value in re.findall(r"(size|align)_of::<(\w+)>\(\) == (\d+)", source)
    }
    expected.update({
        (name, field): int(value)
        for name, field, value in re.findall(r"offset_of!\((\w+), (\S+)\) == (\d+)", source)
    })
    measured = {}
    for row in path.read_text().splitlines():
        if row.startswith("#"):
            continue
        name, field, value = row.split("\t")
        measured[name, field] = int(value)
    if expected != measured:
        for name, field in sorted(expected.keys() | measured.keys()):
            generated = expected.get((name, field))
            native = measured.get((name, field))
            if generated != native:
                print(f"{name}.{field}: Rust={generated}, x86 Windows={native}")
        raise SystemExit("Graphics layout mismatch")
    count = sum(field == "size" for _, field in measured)
    print(f"Verified {count} x86 records ({len(expected)} sizes, alignments and offsets).")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", nargs="?", type=Path, default=DATA / "gfx-layout.tsv")
    parser.add_argument("--compile", nargs=3, type=Path, metavar=("SOURCE", "SDK_UM", "EXE"))
    args = parser.parse_args()
    if args.compile:
        compile_probe(*args.compile)
    else:
        check(args.results)


if __name__ == "__main__":
    main()
