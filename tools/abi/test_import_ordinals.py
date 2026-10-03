import struct
import unittest
from import_ordinals import imports


def archive(flags=0, ordinal=17, symbol=b"_Example@12"):
    payload = symbol + b"\0EXAMPLE.dll\0"
    body = struct.pack("<HHHHIIHH", 0, 0xffff, 0, 0x14c, 0, len(payload), ordinal, flags) + payload
    header = b"example/        " + b"0           " + b"0     " + b"0     " + b"0       "
    header += f"{len(body):<10}".encode() + b"`\n"
    return b"!<arch>\n" + header + body + (b"\n" if len(body) & 1 else b"")


class ImportTests(unittest.TestCase):
    def test_ordinal_not_name_hint(self):
        self.assertEqual(list(imports(archive())), [("example.dll", 17, "Example")])
        self.assertEqual(list(imports(archive(flags=4))), [])
        self.assertEqual(list(imports(archive(flags=1))), [])

    def test_c_symbol_underscore_is_removed_once(self):
        self.assertEqual(list(imports(archive(symbol=b"__stricmp")))[0][2], "_stricmp")

    def test_truncated_member_is_rejected(self):
        with self.assertRaises(ValueError):
            list(imports(archive()[:-4]))


if __name__ == "__main__":
    unittest.main()
