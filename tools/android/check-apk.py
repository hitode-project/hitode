#!/usr/bin/env python3
"""Check native libraries in an APK for AND-APP-2, using only the standard library.

Usage: python tools/android/check-apk.py <apk>
Input: an APK ZIP containing ELF shared libraries (32/64-bit, either byte order).
Output: library/LOAD-segment counts; exit 1 for misaligned libraries,
invalid ELF/ZIP data, or an APK without shared libraries. No extraction is needed.
"""
import argparse
import struct
import sys
import zipfile

PAGE = 16 * 1024


def check_elf(data):
    """Return the number of LOAD segments, rejecting incompatible page alignment."""
    if data[:4] != b"\x7fELF" or data[4:6] not in (b"\x01\x01", b"\x01\x02", b"\x02\x01", b"\x02\x02"):
        raise ValueError("invalid ELF identification")
    order = "<" if data[5] == 1 else ">"
    wide = data[4] == 2
    phoff = struct.unpack_from(order + ("Q" if wide else "I"), data, 32 if wide else 28)[0]
    entsize, count = struct.unpack_from(order + "HH", data, 54 if wide else 42)
    layout = struct.Struct(order + ("IIQQQQQQ" if wide else "IIIIIIII"))
    if entsize < layout.size:
        raise ValueError("invalid ELF program header size")
    loads = 0
    for index in range(count):
        header = layout.unpack_from(data, phoff + index * entsize)
        if header[0] != 1:  # PT_LOAD
            continue
        offset, address, alignment = (header[2], header[3], header[7]) if wide else (header[1], header[2], header[7])
        if alignment < PAGE or alignment & (alignment - 1) or (address - offset) % alignment:
            raise ValueError("ELF LOAD segment is not aligned for 16 KiB pages")
        loads += 1
    if not loads:
        raise ValueError("ELF has no LOAD segments")
    return loads


def check_apk(path):
    """Check ELF alignment, and ZIP offsets of stored libraries; return (libraries, LOADs)."""
    libraries = loads = 0
    with zipfile.ZipFile(path) as archive, open(path, "rb") as raw:
        for entry in archive.infolist():
            if not entry.filename.endswith(".so"):
                continue
            libraries += 1
            # The installer extracts compressed libraries; stored ones map in place.
            if entry.compress_type == zipfile.ZIP_STORED:
                raw.seek(entry.header_offset)
                header = raw.read(30)
                if header[:4] != b"PK\x03\x04":
                    raise ValueError("invalid ZIP local header")
                name_size, extra_size = struct.unpack_from("<HH", header, 26)
                start = entry.header_offset + 30 + name_size + extra_size
                if start % PAGE:
                    raise ValueError("shared library ZIP data is not 16 KiB aligned")
            loads += check_elf(archive.read(entry))
    if not libraries:
        raise ValueError("APK has no shared libraries")
    return libraries, loads


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apk")
    args = parser.parse_args()
    try:
        libraries, loads = check_apk(args.apk)
    except (OSError, ValueError, struct.error, zipfile.BadZipFile) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"OK: {libraries} shared libraries, {loads} LOAD segments; 16 KiB aligned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
