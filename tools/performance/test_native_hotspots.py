import json
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from native_hotspots import compilation_time, kind, merge, summarize

X64 = "elf64-x86-64"


class KindTests(unittest.TestCase):
    def test_frame_operands_are_stack_traffic(self):
        self.assertEqual(kind("mov rax, qword ptr [rsp + 0x18]", X64), "stack")
        self.assertEqual(kind("mov dword ptr [rbp - 0x8], ecx", X64), "stack")
        self.assertEqual(kind("ldr x1, [sp, #0x10]", "elf64-littleaarch64"), "stack")

    def test_other_operands(self):
        self.assertEqual(kind("movzx eax, byte ptr [r12 + rax]", X64), "memory")
        self.assertEqual(kind("call qword ptr [r15 + 0x40]", X64), "call")
        self.assertEqual(kind("blr x8", "elf64-littleaarch64"), "call")
        self.assertEqual(kind("lea rsi, [rdx + 4]", X64), "memory")
        self.assertEqual(kind("add eax, ecx", X64), "other")


class SummaryTests(unittest.TestCase):
    def test_counts_by_part_class_and_guest_instruction(self):
        instructions = [(0, "push rbp"), (1, "mov rax, qword ptr [rsp + 8]"),
                        (6, "cmp eax, 3"), (9, "mov ecx, dword ptr [r12 + rax]"),
                        (13, "add ecx, 1"), (16, "call qword ptr [r15]")]
        labels = {
            0: {"part": "entry", "pc": 0, "class": "overhead"},
            1: {"part": "pure", "pc": 0x10, "class": "int.mem"},
            6: {"part": "check", "pc": 0x10, "class": "int.mem"},
            9: {"part": "body", "pc": 0x10, "class": "int.mem"},
            13: {"part": "body", "pc": 0x14, "class": "int"},
            16: {"part": "slow", "pc": 0x10, "class": "int.mem"},
        }
        # Two samples inside the five-byte reload, one on the add.
        summary = summarize(instructions, 20, labels.__getitem__, Counter({2: 1, 5: 1, 13: 1}), X64)
        self.assertEqual((summary["host"], summary["samples"]), (6, 3))
        self.assertEqual(summary["parts"]["pure"]["static"], {"stack": 1, "all": 1})
        self.assertEqual(summary["parts"]["pure"]["sampled"], {"stack": 2, "all": 2})
        self.assertEqual(summary["parts"]["slow"]["static"]["call"], 1)
        self.assertNotIn("overhead", summary["classes"])
        memory = summary["classes"]["int.mem"]
        self.assertEqual(memory["guest"], 1)
        self.assertEqual(memory["static"], {"pure": 1, "check": 1, "body": 1, "slow": 1})
        self.assertEqual(memory["sampled"], {"pure": 2, "check": 0, "body": 0, "slow": 0})
        self.assertEqual(summary["classes"]["int"]["sampled"], {"body": 1})

    def test_merge_adds_regions(self):
        one = {"host": 2, "samples": 1, "parts": {"body": {"static": {"all": 2}, "sampled": {"all": 1}}},
               "classes": {"int": {"guest": 1, "static": {"body": 2}, "sampled": {"body": 1}}}}
        total = merge([one, one])
        self.assertEqual((total["host"], total["samples"]), (4, 2))
        self.assertEqual(total["parts"]["body"]["static"]["all"], 4)
        self.assertEqual(total["classes"]["int"], {"guest": 2, "static": {"body": 4}, "sampled": {"body": 2}})


class CompilationTimeTests(unittest.TestCase):
    def test_recorded_time_wins_and_old_dumps_take_the_newest_body(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            (directory / "jit-1.tsv").write_text(
                "100\t4096\t64\t65536\tspeed\n"
                "300\t4096\t64\t65536\tspeed\n"
                "200\t8192\t64\t65536\tspeed\n", encoding="utf-8")
            dump = {"guest": 65536, "base": 4096}
            self.assertEqual(compilation_time(directory, dump), 300)
            self.assertEqual(compilation_time(directory, {**dump, "ns": 100}), 100)
            self.assertIsNone(compilation_time(directory, {"guest": 1, "base": 4096}))


if __name__ == "__main__":
    unittest.main()
