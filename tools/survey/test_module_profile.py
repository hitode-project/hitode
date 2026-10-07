import struct
import unittest

from module_profile import Module, category, function_starts, parse, profile, rtti, shaders

BASE = 0x400000


def tiny_dll() -> bytes:
    """A two-section PE32 DLL importing kernel32!CreateThread and exporting Run."""
    data = bytearray(0x600)
    data[0:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x44, 0x14C, 2, 1234, 0, 0, 0xE0, 0x2102)
    optional = 0x58
    struct.pack_into("<H", data, optional, 0x10B)
    struct.pack_into("<I", data, optional + 16, 0x1000)
    struct.pack_into("<I", data, optional + 28, BASE)
    struct.pack_into("<II", data, optional + 96, 0x2100, 0x60)      # exports
    struct.pack_into("<II", data, optional + 104, 0x2000, 0x28)     # imports
    for index, (name, rva, raw, flags) in enumerate(((b".text", 0x1000, 0x200, 0x60000020),
                                                      (b".rdata", 0x2000, 0x400, 0x40000040))):
        at = optional + 0xE0 + 40 * index
        data[at:at + 8] = name.ljust(8, b"\0")
        struct.pack_into("<IIII", data, at + 8, 0x200, rva, 0x200, raw)
        struct.pack_into("<I", data, at + 36, flags)

    def put(rva, value):
        data[0x400 + rva - 0x2000:0x400 + rva - 0x2000 + len(value)] = value

    put(0x2000, struct.pack("<IIIII", 0x2040, 0, 0, 0x2080, 0x2060))
    put(0x2040, struct.pack("<II", 0x20A0, 0))
    put(0x2060, struct.pack("<II", 0x20A0, 0))
    put(0x2080, b"KERNEL32.dll\0")
    put(0x20A0, b"\0\0CreateThread\0")
    put(0x2100, struct.pack("<IIIIIIIIII", 0, 0, 0, 0x2180, 1, 1, 1, 0x2140, 0x2150, 0x2160))
    put(0x2140, struct.pack("<I", 0x1010))
    put(0x2150, struct.pack("<I", 0x2190))
    put(0x2160, struct.pack("<H", 0))
    put(0x2180, b"t.dll\0")
    put(0x2190, b"Run\0")
    put(0x21C0, b".?AVPlayer@@\0\0\0\0.?AUState@@\0")
    put(0x2200, bytes([0, 2, 0xFF, 0xFF, 0xFE, 0xFF, 0x0A, 0]) + bytes([0, 3, 0xFE, 0xFF, 0xFE, 0xFF]))
    put(0x2220, bytes([0, 2, 0xFF, 0xFF, 0x1F, 0, 0, 0]))  # a version token without a comment
    return bytes(data)


CODE = [
    (0x401000, "ucomiss", "xmm0, xmm1"),
    (0x401003, "lahf", ""),
    (0x401004, "test", "ah, 0x44"),
    (0x401007, "call", "0x401010 <Run>"),
    (0x40100c, "call", "dword ptr [0x402060]"),
    (0x401012, "call", "dword ptr [eax + 0x4]"),
    (0x401015, "call", "edx"),
    (0x401017, "jmp", "dword ptr [4*ecx + 0x401100]"),
    (0x40101e, "rep movsd", "dword ptr es:[edi], dword ptr [esi]"),
    (0x401020, "int3", ""),
    (0x401021, "movss", "xmm0, dword ptr [esp + 0x8]"),
    (0x401027, "lea", "eax, [ebp - 0xc]"),
    (0x40102a, "call", "0x405000"),
]


class ModuleTests(unittest.TestCase):
    def test_headers_imports_and_exports(self):
        module = Module(tiny_dll())
        self.assertEqual((module.architecture, module.kind, module.image_base), ("x86", "DLL", BASE))
        self.assertEqual(module.imports(), {"KERNEL32.dll": [(0x2060, "CreateThread")]})
        self.assertEqual(module.exports(), [(0x1010, "Run")])

    def test_rejects_other_files(self):
        with self.assertRaises(ValueError):
            Module(b"\0" * 128)


class CodeTests(unittest.TestCase):
    def test_parse_keeps_prefixes_and_skips_headers(self):
        text = ("\nfile.dll:\tfile format coff-i386\n\nDisassembly of section .text:\n\n"
                "10001000 <.text>:\n"
                "10001000:      \tmov\tecx, 0x101063e8\n"
                "1000ddab:      \trep\t\tmovsd\tdword ptr es:[edi], dword ptr [esi]\n"
                "1000103e:      \tint3\n")
        self.assertEqual(parse(text), [(0x10001000, "mov", "ecx, 0x101063e8"),
                                       (0x1000DDAB, "rep movsd", "dword ptr es:[edi], dword ptr [esi]"),
                                       (0x1000103E, "int3", "")])

    def test_categories(self):
        self.assertEqual(category("movss", "xmm0, dword ptr [eax]"), "sse.scalar")
        self.assertEqual(category("mulps", "xmm0, xmm1"), "sse.packed")
        self.assertEqual(category("pxor", "xmm0, xmm0"), "sse.int")
        self.assertEqual(category("movsd", "dword ptr es:[edi], dword ptr [esi]"), "string")
        self.assertEqual(category("paddw", "mm0, mm1"), "mmx")
        self.assertEqual(category("fstp", "dword ptr [eax]"), "x87")
        self.assertEqual(category("jne", "0x1000"), "control")
        self.assertEqual(category("int3", ""), "padding")
        self.assertEqual(category("add", "eax, 1"), "int")

    def test_profile(self):
        module = Module(tiny_dll())
        figures = profile(module, CODE, classes=5)
        self.assertEqual(figures["starts_threads"], ["CreateThread"])
        self.assertEqual(figures["instructions"], len(CODE) - 1)
        self.assertEqual(figures["calls"], {"direct": 2, "import": 1, "memory": 1, "register": 1,
                                            "jump table": 1})
        self.assertEqual(figures["imported_call_sites"], {"CreateThread": 1})
        self.assertEqual(figures["patterns"]["sse compare via lahf"], 1)
        self.assertEqual(figures["memory_operands"]["sse.scalar"], 1)
        self.assertNotIn("int", figures["memory_operands"])  # lea is not an access
        self.assertEqual(figures["shaders"], {"ps_2_0": 1, "vs_3_0": 1})
        self.assertEqual(figures["rtti_names"], ["Player", "State"])

    def test_function_starts_stay_inside_code(self):
        self.assertEqual(function_starts(Module(tiny_dll()), CODE), [0x1000, 0x1010])

    def test_data_scanners(self):
        self.assertEqual(shaders(b"\0\x01\xfe\xff\xfe\xff"), {"vs_1_0": 1})
        self.assertEqual(rtti(b"x.?AVa@b@@\0"), ["a@b"])


if __name__ == "__main__":
    unittest.main()
