#!/usr/bin/env python3
"""Project the missing Direct3D 1–7 declarations from the pinned x86 SDK."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess
from tempfile import TemporaryDirectory
from uuid import UUID

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "data/abi"
HEADERS = ("d3d.h", "d3dtypes.h", "d3dcaps.h")
SDK_COM_MARKER = "# Direct3D 1–7 from the pinned SDK d3d.h."


def com_rows(header):
    header = re.sub(r"/\*.*?\*/|//[^\n]*", "", header, flags=re.S)
    for name, body in re.findall(r"DECLARE_INTERFACE_\((\w+),\s*\w+\)\s*\{(.*?)\n\};", header, re.S):
        for slot, method in enumerate(body.split(";")[:-1]):
            return_type, explicit_method, method_name, arguments = re.fullmatch(
                r"\s*STDMETHOD(?:_\((ULONG),\s*(\w+)\)|\((\w+)\))\(THIS_?\s*(.*?)\)\s*PURE\s*",
                method,
                re.S,
            ).groups()
            yield name, slot, return_type, explicit_method or method_name, arguments


def parameters(interface, arguments):
    yield f"this:{interface}*:ptr:1"
    for index, argument in enumerate(arguments.split(",") if arguments else []):
        param = f"arg{index}"
        argument = " ".join(argument.split()).replace(" *", "*")
        if argument in ("REFIID riid", "LPVOID* ppvObj"):
            argument, param = argument.split()
        kind = "ptr" if argument.endswith("*") or argument.lstrip().removeprefix("const ").startswith(("LP", "REF")) else {
            "BOOL": "i32",
            "D3DVALUE": "f32",
            "D3DLIGHTSTATETYPE": "i32",
            "D3DPRIMITIVETYPE": "i32",
            "D3DRENDERSTATETYPE": "i32",
            "D3DSTATEBLOCKTYPE": "i32",
            "D3DTEXTURESTAGESTATETYPE": "i32",
            "D3DTRANSFORMSTATETYPE": "i32",
            "D3DVERTEXTYPE": "i32",
            "D3DMATERIALHANDLE": "u32",
            "D3DMATRIXHANDLE": "u32",
            "D3DCOLOR": "u32",
            "DWORD": "u32",
            "WORD": "u32",
        }[argument]
        yield f"{param}:{argument}:{kind}:1"


def constants(work, headers, compiler, objcopy):
    source = work / "constants.c"
    include = '#define DIRECT3D_VERSION 0x0700\n#include "d3d.h"\n'
    source.write_text(include)
    expanded = subprocess.check_output(
        [compiler, "-I", headers, "-E", "-dD", source], text=True
    )
    # Preprocessor line markers restrict this projection to legacy SDK headers.
    sections = re.split(r'^# \d+ "([^"]+)"[^\n]*\n', expanded, flags=re.M)
    legacy = "\n".join(body for path, body in zip(sections[1::2], sections[2::2])
                       if Path(path).resolve().parent == headers and Path(path).name in HEADERS)
    groups = {}
    for body, name in re.findall(r"typedef enum\s+\w+\s*\{(.*?)\}\s*(D3D\w+)\s*;", legacy, re.S):
        groups.update((item, name) for item in re.findall(r"\b(D3D\w+)\s*(?==|,|$)", body))
    for name in re.findall(r"^#define (D3D\w+)[ \t]+\S", legacy, re.M):
        if name in {"D3DAPI", "D3DLIGHT_RANGE_MAX"}:  # Annotation and floating expression.
            continue
        prefix = next((p for p in ("D3DFVF", "D3DRENDERSTATE", "D3DTRANSFORMSTATE", "D3DVBLEND")
                       if name.startswith(p + "_")), None)
        group = {
            "D3DRENDERSTATE": "D3DRENDERSTATETYPE",
            "D3DTRANSFORMSTATE": "D3DTRANSFORMSTATETYPE",
            "D3DVBLEND": "D3DVERTEXBLENDFLAGS",
        }.get(prefix, prefix or "Apis")
        groups.setdefault(name, group)
    items = sorted(groups)
    source.write_text(include + '__attribute__((section(".gfx"))) const DWORD values[] = {\n'
                      + "".join(f"    (DWORD)({name}),\n" for name in items) + "};\n")
    obj = work / "constants.o"
    raw = work / "constants.bin"
    subprocess.run([compiler, "-I", headers, "-c", source, "-o", obj], check=True)
    subprocess.run([objcopy, "-O", "binary", "-j", ".gfx", obj, raw], check=True)
    values = struct.unpack(f"<{len(items)}I", raw.read_bytes()[:4 * len(items)])
    return [f"Windows.SDK.Direct3D7.{groups[name]}\t{name}\t{value:x}\tu32"
            for name, value in zip(items, values)]


def supplemental_records(headers):
    # These legacy records are absent from the pinned WinMD.
    names = {"D3DLINEPATTERN", "D3DDEVINFO_TEXTUREMANAGER", "D3DDEVINFO_TEXTURING"}
    yield "# Struct\tfields (name:primitive)"
    for header in ("d3dtypes.h", "d3dcaps.h"):
        source = re.sub(r"/\*.*?\*/|//[^\n]*", "", (headers / header).read_text(), flags=re.S)
        for body, name in re.findall(r"typedef struct \w+\s*\{([^{}]+)\}\s*(D3D\w+)[,;]", source):
            if name not in names:
                continue
            fields = []
            for field in body.strip().rstrip(";").split(";"):
                type_name, field_name = field.split()
                kind = {"WORD": "u16", "DWORD": "u32", "BOOL": "i32"}[type_name]
                fields.append(f"{field_name}:{kind}")
            yield name + "\t" + "|".join(fields)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("headers", type=Path, help="Windows SDK um include directory")
    parser.add_argument("--cc", default="i686-w64-mingw32-gcc")
    parser.add_argument("--objcopy", default="i686-w64-mingw32-objcopy")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    headers = args.headers.resolve()
    manifest = json.loads((DATA / "sdk-source.json").read_text())["direct3d7"]
    for name in HEADERS:
        if hashlib.sha256((headers / name).read_bytes()).hexdigest() != manifest["headers"][f"um/{name}"]:
            raise SystemExit(f"SDK header hash mismatch: {name}")
    header = (headers / "d3d.h").read_text()
    rows = []
    for interface, slot, explicit_return, method, arguments in com_rows(header):
        result = ("u32", explicit_return) if explicit_return else ("i32", "HRESULT")
        rows.append("\t".join((interface, str(slot), method, *result,
                               "|".join(parameters(interface, arguments)))))
    iids = []
    for name, fields in re.findall(r"DEFINE_GUID\(\s*IID_(IDirect3D\w*),\s*([^)]*)\)", header):
        a, b, c, *tail = (int(part, 16) for part in fields.split(","))
        value = UUID(bytes_le=struct.pack("<IHH8B", a, b, c, *tail))
        iids.append(f"{name}\t{value}")
    com = (DATA / "sdk-com-input.tsv").read_text().split(SDK_COM_MARKER)[0]
    with TemporaryDirectory() as path:
        outputs = {
            "sdk-com-input.tsv": com + SDK_COM_MARKER + "\n" + "\n".join(rows) + "\n",
            "gfx-records-input.tsv": "\n".join(supplemental_records(headers)) + "\n",
            "gfx-iids-input.tsv": "# Interface or device IID\tGUID\n" + "\n".join(sorted(iids)) + "\n",
            "gfx-constants-input.tsv": "# Legacy SDK type\tconstant\thex bits\tkind\n"
            + "\n".join(constants(Path(path), headers, args.cc, args.objcopy)) + "\n",
        }
    for name, contents in outputs.items():
        path = DATA / name
        if args.check:
            if path.read_text() != contents:
                raise SystemExit(f"Stale {name}")
        else:
            path.write_text(contents)


if __name__ == "__main__":
    main()
