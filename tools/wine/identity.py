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
reads only the tables of the modules it maps. Unix i386 ELF modules use
their ELF and program headers, STT_FUNC symbols (including local symbols),
and the dwords patched by dynamic relocations.
"""

import re
import struct
import sys
from pathlib import Path

MODULES = "lib/wine/i386-windows"
UNIX_MODULES = "lib/wine/i386-unix"
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


class Elf:
    def __init__(self, data: bytes):
        self.data = data
        if data[:6] != b"\x7fELF\x01\x01" or struct.unpack_from("<HH", data, 16) != (3, 3):
            raise ValueError("not an i386 ELF shared object")
        phoff, shoff = struct.unpack_from("<II", data, 28)
        phsize, phnum, shsize, shnum = struct.unpack_from("<HHHH", data, 42)
        self.headers = max(52, phoff + phsize * phnum)
        self.sections = [
            struct.unpack_from("<10I", data, shoff + i * shsize) for i in range(shnum)
        ]
        self.segments = [
            struct.unpack_from("<8I", data, phoff + i * phsize) for i in range(phnum)
        ]
        self.relocated = set()
        for _, kind, flags, _, offset, size, _, _, _, entsize in self.sections:
            if kind == 9 and flags & 2:  # allocated SHT_REL, not debug relocations
                for at in range(offset, offset + size, entsize):
                    address, info = struct.unpack_from("<II", data, at)
                    if info & 0xff in (1, 2, 6, 7, 8):
                        self.relocated.add(address)

    def offset(self, rva: int):
        for kind, offset, va, _, size, _, _, _ in self.segments:
            if kind == 1 and va <= rva < va + size:
                return offset + rva - va
        return None

    masked = Image.masked

    def functions(self):
        found = {}
        for _, kind, _, _, offset, size, link, _, _, entsize in self.sections:
            if kind != 2:  # SHT_SYMTAB, including static functions
                continue
            strings = self.sections[link][4]
            for at in range(offset, offset + size, entsize):
                name, value, length, info, _, section = struct.unpack_from("<IIIBBH", self.data, at)
                if info & 0xf != 2 or not length or not section:
                    continue
                start = strings + name
                name = self.data[start : self.data.index(b"\0", start)].decode()
                found.setdefault(value, (length, name))
        return sorted(found.items())


def elf_rows(debug: Elf, stripped: Elf) -> list[str]:
    rows = [f"{fnv1a64(stripped.data[: stripped.headers]):016x}"]
    for rva, (size, name) in debug.functions():
        code = stripped.masked(rva, size)
        rows.append(f"{rva:x}\t{size:x}\t{fnv1a64(code) & 0xffffffff:08x}\t{name}")
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
    unix_debug = debug_root.parent / "i386-unix"
    for path in sorted((stripped / UNIX_MODULES).glob("*.so")):
        image = Elf(path.read_bytes())
        debug = Elf((unix_debug / path.name).read_bytes())
        rows = elf_rows(debug, image)
        (output / f"{path.name.lower()}.tsv").write_text("\n".join(rows) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
