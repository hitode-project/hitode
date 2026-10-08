"""Test export metadata projection using authored PE and API-set byte layouts."""
import struct
import unittest
from system_exports import PE


def fixture():
    data = bytearray(0x5000)
    data[:2] = b"MZ"
    word = lambda at, value: struct.pack_into("<I", data, at, value)
    short = lambda at, value: struct.pack_into("<H", data, at, value)
    word(0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    short(0x86, 4)
    short(0x94, 224)
    short(0x98, 0x10B)
    word(0x98 + 96, 0x2000)
    word(0x98 + 100, 0x200)
    for index, (name, rva, flags) in enumerate([
            (b".edata", 0x2000, 0x40000000), (b".text", 0x3000, 0x60000000),
            (b".data", 0x4000, 0xC0000000), (b".apiset", 0x1000, 0x40000000)]):
        at = 0x178 + index * 40
        data[at:at + len(name)] = name
        for offset, value in [(8, 4096), (12, rva), (16, 4096), (20, rva), (36, flags)]:
            word(at + offset, value)
    for offset, value in [(16, 7), (20, 3), (24, 3), (28, 0x2040), (32, 0x2050), (36, 0x2060)]:
        word(0x2000 + offset, value)
    for index, (name, target) in enumerate([(b"Code", 0x3000), (b"Data", 0x4000), (b"Forward", 0x2100)]):
        at = 0x2080 + index * 16
        data[at:at + len(name) + 1] = name + b"\0"
        word(0x2040 + index * 4, target)
        word(0x2050 + index * 4, at)
        short(0x2060 + index * 2, index)
    data[0x2100:0x2100 + 13] = b"OTHER.Target\0"
    return data


class SystemExportTests(unittest.TestCase):
    def test_names_ordinals_kinds_and_forwarders_only(self):
        self.assertEqual(PE(fixture()).exports(), [
            (7, "Code", "code", ""), (8, "Data", "data", ""),
            (9, "Forward", "forwarder", "OTHER.Target")])

    def test_api_set_hash_prefix_and_importer_override(self):
        data = fixture()
        section = bytearray(4096)
        word = lambda at, value: struct.pack_into("<I", section, at, value)
        for at, value in [(0, 6), (4, 4096), (12, 1), (16, 32), (32 + 4, 256),
                          (32 + 8, 0), (32 + 12, 0), (32 + 16, 64), (32 + 20, 2)]:
            word(at, value)
        cursor = 256
        def text(at, length_at, value):
            nonlocal cursor
            encoded = value.encode("utf-16-le")
            word(at, cursor)
            word(length_at, len(encoded))
            section[cursor:cursor + len(encoded)] = encoded
            cursor += len(encoded)
        text(36, 40, "api-ms-test-l1-1-8")
        word(44, len("api-ms-test-l1-1".encode("utf-16-le")))
        text(64 + 12, 64 + 16, "kernel32.dll")
        text(84 + 4, 84 + 8, "kernel32.dll")
        text(84 + 12, 84 + 16, "kernelbase.dll")
        data[0x1000:0x2000] = section
        self.assertEqual(PE(data).api_sets(), {"api-ms-test-l1-1": {
            "": "kernel32.dll", "kernel32.dll": "kernelbase.dll"}})

    def test_malformed_and_pe64_exports_are_rejected(self):
        data = fixture()
        struct.pack_into("<H", data, 0x98, 0x20B)
        with self.assertRaises(ValueError):
            PE(data).exports()
        with self.assertRaises(ValueError):
            PE(b"invalid")


if __name__ == "__main__":
    unittest.main()
