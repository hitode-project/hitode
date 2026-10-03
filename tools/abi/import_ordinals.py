#!/usr/bin/env python3
"""Extract named ordinal aliases from official x86 Windows SDK import libraries.

Only IMPORT_OBJECT_HEADER records with IMPORT_ORDINAL are projected. The SDK
library SHA-256 is recorded; no implementation bytes or DLLs enter the output.
"""
from __future__ import annotations
import argparse
import hashlib
from pathlib import Path
import re
import struct


def imports(data: bytes):
    if not data.startswith(b"!<arch>\n"):
        raise ValueError("not a COFF archive")
    offset = 8
    while offset < len(data):
        header = data[offset:offset + 60]
        if len(header) != 60 or header[58:60] != b"`\n":
            raise ValueError("invalid archive member")
        length = int(header[48:58].decode("ascii").strip())
        body = data[offset + 60:offset + 60 + length]
        if len(body) != length:
            raise ValueError("truncated archive member")
        offset += 60 + length + (length & 1)
        if len(body) < 20 or body[:4] != b"\0\0\xff\xff":
            continue
        version, machine, _, size, ordinal, flags = struct.unpack_from("<HHIIHH", body, 4)
        if version != 0 or machine != 0x14c or flags & 3 != 0 or (flags >> 2) & 7 != 0:
            continue
        payload = body[20:20 + size]
        if len(payload) != size:
            raise ValueError("truncated short import")
        symbol, module, _ = payload.split(b"\0", 2)
        symbol = symbol.decode("ascii")
        # i386 C symbols have a leading underscore and optional stdcall suffix.
        name = re.sub(r"@\d+$", "", symbol.removeprefix("_"))
        yield module.decode("ascii").lower(), ordinal, name


def generate(sdk: Path, modules: Path) -> str:
    if not sdk.is_dir():
        raise ValueError("SDK import library directory does not exist")
    sources = []
    aliases = {}
    for module in modules.read_text(encoding="utf-8").splitlines():
        if not module or module.startswith("#"):
            continue
        library = sdk / (Path(module).stem + ".lib")
        if not library.exists():
            continue
        data = library.read_bytes()
        entries = list(imports(data))
        if entries:
            sources.append(f"# {library.name} SHA256 {hashlib.sha256(data).hexdigest()}")
        for dll, ordinal, name in entries:
            key = (dll, ordinal)
            if key in aliases and aliases[key] != name:
                raise ValueError(f"conflicting ordinal: {key}")
            aliases[key] = name
    header = ["# Official Windows SDK x86 short imports; dll\tordinal\texport", *sources]
    return "\n".join(header + [f"{dll}\t{ordinal}\t{name}" for (dll, ordinal), name in sorted(aliases.items())]) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-dir", type=Path, required=True)
    parser.add_argument("--modules", type=Path, default=Path("data/abi/modules.txt"))
    parser.add_argument("--output", type=Path, default=Path("data/abi/import-ordinals.tsv"))
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    output = generate(args.sdk_dir, args.modules)
    if args.check:
        if args.output.read_text(encoding="utf-8") != output:
            raise SystemExit("import ordinal projection differs")
    else:
        args.output.write_text(output, encoding="utf-8", newline="\n")
    print(f"import ordinals: {sum(not row.startswith('#') for row in output.splitlines())}")
