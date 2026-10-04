import struct
import unittest

from import_sst386 import chunks, parse_moo, registers


def chunk(tag, payload):
    return tag + struct.pack("<I", len(payload)) + payload


def counted(data):
    return struct.pack("<I", len(data)) + data


def vector(extra=b"", final_extra=b""):
    initial = chunk(b"RG32", struct.pack("<III", (1 << 2) | (1 << 17), 0xffffffff, 0x202))
    final = chunk(b"RG32", struct.pack("<II", 1 << 2, 0)) + final_extra
    test = struct.pack("<I", 7) + chunk(b"NAME", counted(b"inc eax"))
    test += chunk(b"BYTS", counted(bytes.fromhex("6640f4")))
    test += chunk(b"INIT", initial) + chunk(b"FINA", final)
    test += chunk(b"HASH", bytes(range(20))) + extra
    return chunk(b"MOO ", bytes((1, 1, 0, 0)) + struct.pack("<I", 1) + b"386E") + chunk(b"TEST", test)


class FormatTests(unittest.TestCase):
    def test_sparse_final_state_and_identity_are_preserved(self):
        result, = parse_moo(vector())
        self.assertEqual(result["initial"]["regs"], {"eax": 0xffffffff, "eflags": 0x202})
        self.assertEqual(result["final"]["regs"], {"eax": 0})
        self.assertEqual(result["hash"], "000102030405060708090a0b0c0d0e0f10111213")
        self.assertEqual(result["bytes"], [0x66, 0x40, 0xf4])

    def test_common_and_per_test_undefined_register_masks_are_not_discarded(self):
        masks = chunk(b"RM32", struct.pack("<II", 1 << 17, 0xffffffef))
        parts = list(chunks(vector(final_extra=masks)))
        data = chunk(*parts[0]) + masks + chunk(*parts[1])
        result, = parse_moo(data)
        self.assertEqual(result["masks"], {"eflags": 0xffffffef})
        self.assertEqual(result["final"]["masks"], result["masks"])

    def test_exception_stays_distinct_from_success(self):
        result, = parse_moo(vector(chunk(b"EXCP", struct.pack("<BI", 13, 0x4321))))
        self.assertEqual(result["exception"], {"number": 13, "flag_address": 0x4321})

    def test_truncated_data_invalid_count_and_new_semantics_fail_closed(self):
        for data in [vector()[:-1], vector() + b"x", vector(chunk(b"NEW!", b"")),
                     vector().replace(struct.pack("<I", 1) + b"386E", struct.pack("<I", 2) + b"386E")]:
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                parse_moo(data)
        with self.assertRaises(ValueError):
            registers(struct.pack("<I", 1 << 20))


if __name__ == "__main__":
    unittest.main()
