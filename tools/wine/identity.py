#!/usr/bin/env python3
"""Write the code identity table of a Wine distribution.

usage: identity.py DIST STRIPPED_DIST

DIST is the installation with debug information, as build-media.sh leaves
it; STRIPPED_DIST is the runtime distribution. For every PE module in
lib/wine/i386-windows the table records a hash of the stripped image's
headers, which the runtime compares with the headers of a mapped image,
and every function from the COFF symbol table of the unstripped image
except import thunks: its RVA, size, a hash of its bytes with base
relocations masked, and its undecorated name. Each module's table is
written to STRIPPED_DIST/share/hitode/identity/<module>.tsv, so the runtime
reads only the tables of the modules it maps.
"""

import re
import struct
import sys
from pathlib import Path

MODULES = "lib/wine/i386-windows"
OUTPUT = "share/hitode/identity"
IMAGE_SCN_CNT_CODE = 0x20
IMAGE_REL_BASED_HIGHLOW = 3
DT_FUNCTION = 0x20


def fnv1a64(data: bytes) -> int:
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


class Image:
    def __init__(self, data: bytes):
        self.data = data
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        if data[pe : pe + 4] != b"PE\0\0":
            raise ValueError("not a PE image")
        count, self.symbols, self.symbol_count = struct.unpack_from("<HxxxxII", data, pe + 6)
        optional = struct.unpack_from("<H", data, pe + 20)[0]
        self.headers = struct.unpack_from("<I", data, pe + 24 + 60)[0]
        reloc_rva, reloc_size = struct.unpack_from("<II", data, pe + 24 + 96 + 5 * 8)
        table = pe + 24 + optional
        self.sections = []
        for i in range(count):
            _, vsize, va, raw_size, raw = struct.unpack_from("<8sIIII", data, table + 40 * i)
            flags = struct.unpack_from("<I", data, table + 40 * i + 36)[0]
            self.sections.append((va, vsize, raw, raw_size, flags))
        self.relocated = set()
        offset = self.offset(reloc_rva) if reloc_size else None
        end = offset + reloc_size if offset is not None else 0
        while offset is not None and offset + 8 <= end:
            page, size = struct.unpack_from("<II", data, offset)
            if size < 8:
                break
            for (entry,) in struct.iter_unpack("<H", data[offset + 8 : offset + size]):
                if entry >> 12 == IMAGE_REL_BASED_HIGHLOW:
                    self.relocated.add(page + (entry & 0xFFF))
            offset += size

    def offset(self, rva: int):
        for va, vsize, raw, raw_size, _ in self.sections:
            if va <= rva < va + min(vsize, raw_size):
                return raw + rva - va
        return None

    def masked(self, rva: int, size: int) -> bytes:
        """The bytes at `rva`, with every relocated dword zeroed."""
        offset = self.offset(rva)
        code = bytearray(self.data[offset : offset + size])
        for at in range(rva - 3, rva + size):
            if at in self.relocated:
                for i in range(max(at, rva), min(at + 4, rva + size)):
                    code[i - rva] = 0
        return bytes(code)

    def functions(self):
        """(rva, name) of every function symbol, by address."""
        data, strings = self.data, self.symbols + 18 * self.symbol_count
        found, index = {}, 0
        while index < self.symbol_count:
            at = self.symbols + 18 * index
            name, value, section, kind, _, aux = struct.unpack_from("<8sIhHBB", data, at)
            index += 1 + aux
            if kind != DT_FUNCTION or section <= 0:
                continue
            if name[:4] == b"\0\0\0\0":
                start = strings + struct.unpack_from("<I", name, 4)[0]
                name = data[start : data.index(b"\0", start)]
            va, _, _, _, flags = self.sections[section - 1]
            if flags & IMAGE_SCN_CNT_CODE:
                found.setdefault(va + value, name.rstrip(b"\0").decode())
        return sorted(found.items())

    def code_end(self, rva: int) -> int:
        for va, vsize, _, _, _ in self.sections:
            if va <= rva < va + vsize:
                return va + vsize
        return rva


def undecorate(name: str) -> str:
    """`_name@8`, `@name@8` and `_name` as `name`."""
    if name[:1] in "_@":
        name = name[1:]
    return re.sub(r"@\d+$", "", name)


def module_rows(debug: Image, stripped: Image) -> list[str]:
    rows = [f"{fnv1a64(stripped.data[: stripped.headers]):016x}"]
    functions = debug.functions()
    for i, (rva, symbol) in enumerate(functions):
        end = functions[i + 1][0] if i + 1 < len(functions) else stripped.code_end(rva)
        end = min(end, stripped.code_end(rva))
        code = stripped.masked(rva, end - rva) if end > rva else b""
        # jmp [import]: the thunk of an imported function.
        if not code or code[:2] == b"\xff\x25" and end - rva <= 8:
            continue
        digest = fnv1a64(code) & 0xFFFFFFFF
        rows.append(f"{rva:x}\t{end - rva:x}\t{digest:08x}\t{undecorate(symbol)}")
    return rows


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    dist, stripped = Path(argv[0]), Path(argv[1])
    debug_root = next(p for p in (dist / "usr/local" / MODULES, dist / MODULES) if p.is_dir())
    output = stripped / OUTPUT
    output.mkdir(parents=True, exist_ok=True)
    for path in sorted((stripped / MODULES).iterdir()):
        try:
            image = Image(path.read_bytes())
            debug = Image((debug_root / path.name).read_bytes())
        except (ValueError, struct.error, FileNotFoundError):
            continue
        rows = module_rows(debug, image)
        (output / f"{path.name.lower()}.tsv").write_text("\n".join(rows) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
