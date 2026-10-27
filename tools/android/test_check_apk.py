"""Authored ELF/ZIP fixtures for check-apk.py; run with unittest, no SDK needed."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("check_apk", Path(__file__).with_name("check-apk.py"))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


def elf(alignment=16384, *, wide=True, order="<", address=0):
    data = bytearray(128)
    data[:7] = b"\x7fELF" + bytes([2 if wide else 1, 1 if order == "<" else 2, 1])
    struct.pack_into(order + ("Q" if wide else "I"), data, 32 if wide else 28, 64)
    struct.pack_into(order + "HH", data, 54 if wide else 42, 56 if wide else 32, 1)
    if wide:
        struct.pack_into(order + "IIQQQQQQ", data, 64, 1, 5, 0, address, address, 128, 128, alignment)
    else:
        struct.pack_into(order + "IIIIIIII", data, 64, 1, 0, address, address, 128, 128, 5, alignment)
    return data


def add_library(archive, name, data, *, aligned=True, compressed=False):
    entry = zipfile.ZipInfo(name)
    entry.compress_type = zipfile.ZIP_DEFLATED if compressed else zipfile.ZIP_STORED
    if aligned:
        start = archive.fp.tell() + 30 + len(name.encode()) + 4
        padding = (-start) % checker.PAGE
        entry.extra = struct.pack("<HH", 0xCAFE, padding) + bytes(padding)
    archive.writestr(entry, data)


class ApkTests(unittest.TestCase):
    # spec: AND-APP-2
    def test_all_libraries_are_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.apk"
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("classes.dex", b"fixture")
                add_library(archive, "lib/arm64-v8a/first.so", elf())
                add_library(archive, "lib/arm64-v8a/second.so", elf(65536))
                add_library(archive, "lib/arm64-v8a/third.so", elf(), aligned=False, compressed=True)
            self.assertEqual(checker.check_apk(path), (3, 3))

    # spec: AND-APP-2
    def test_stored_zip_alignment_and_compressed_elf_alignment_are_checked(self):
        for library, options in ((elf(), {"aligned": False}), (elf(4096), {"compressed": True})):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "fixture.apk"
                with zipfile.ZipFile(path, "w") as archive:
                    add_library(archive, "lib/arm64-v8a/game.so", library, **options)
                with self.assertRaises(ValueError):
                    checker.check_apk(path)

    # spec: AND-APP-2
    def test_load_alignment_and_elf_formats(self):
        for wide in (True, False):
            for order in ("<", ">"):
                with self.subTest(wide=wide, order=order):
                    self.assertEqual(checker.check_elf(elf(wide=wide, order=order)), 1)
                    with self.assertRaises(ValueError):
                        checker.check_elf(elf(4096, wide=wide, order=order))
        with self.assertRaises(ValueError):
            checker.check_elf(elf(address=4096))
        with self.assertRaises((ValueError, struct.error)):
            checker.check_elf(b"\x7fELF\x02\x01")

    # spec: AND-APP-2
    def test_apk_without_native_libraries_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.apk"
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("classes.dex", b"fixture")
            with self.assertRaises(ValueError):
                checker.check_apk(path)


if __name__ == "__main__":
    unittest.main()
