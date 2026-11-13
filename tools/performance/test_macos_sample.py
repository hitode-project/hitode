import unittest

from macos_sample import host_kind, loop_bodies, loop_kinds, thread_leaves

TEXT = """Call graph:
    10 Thread_1: main
    + 10 mach_msg2_trap  (in libsystem_kernel.dylib) + 8  [0x18a]
    9 Thread_2: adv-execution
    + 5 ???  (in <unknown binary>)  [0x1000]
    + ! 3 ???  (in <unknown binary>)  [0x1010]
    + ! 1 _RNvadv_cpu_a64native9interpret  (in adv-launcher) + 4  [0x5000]
    + 4 poll  (in libsystem_kernel.dylib) + 8  [0x18b]
    3 Thread_3: guest computation
    + 3 ???  (in <unknown binary>)  [0x2000]
    2 Thread_4: guest computation
    + 2 ???  (in <unknown binary>)  [0x2004]

Total number in stack (recursive counted multiple times):
"""


class ThreadLeavesTests(unittest.TestCase):
    def test_leaves_subtract_children_and_keep_jit_addresses(self):
        leaves = thread_leaves(TEXT, "adv-execution")
        self.assertEqual(leaves, [("???", 0x1000, 1), ("???", 0x1010, 3),
                                  ("_RNvadv_cpu_a64native9interpret", 0x5000, 1), ("poll", 0x18b, 4)])
        self.assertEqual(thread_leaves(TEXT, "main"), [("mach_msg2_trap", 0x18a, 10)])

    def test_every_thread_with_the_name_counts(self):
        self.assertEqual(thread_leaves(TEXT, "guest computation"),
                         [("???", 0x2000, 3), ("???", 0x2004, 2)])



class Operand:
    def __init__(self, base=None):
        self.type = 3 if base else 1
        self.mem = type("Mem", (), {"base": base})()


class Insn:
    """The parts of a capstone instruction host_kind reads; registers are names."""

    def __init__(self, mnemonic, read=(), written=(), base=None):
        self.mnemonic = mnemonic
        self.operands = [Operand(base)]
        self.regs = (list(read), list(written))

    def regs_access(self):
        return self.regs

    def reg_name(self, name):
        return name


class HostKindTests(unittest.TestCase):
    def test_register_roles_name_the_work(self):
        self.assertEqual(host_kind(Insn("ldr", ["x28", "w6"], ["d1"], base="x28"), True),
                         "guest memory")
        self.assertEqual(host_kind(Insn("str", ["w6", "x5"], base="x5"), True), "CpuState")
        self.assertEqual(host_kind(Insn("ldr", ["fp"], ["x5"], base="fp"), True), "Runtime")
        self.assertEqual(host_kind(Insn("b.ne", ["nzcv"]), True), "branch")
        self.assertEqual(host_kind(Insn("sub", ["x9"], ["x9"]), False), "countdown")
        self.assertEqual(host_kind(Insn("and", ["w13"], ["w13"]), True), "x87 state")
        self.assertEqual(host_kind(Insn("and", ["w13"], ["w13"]), False), "integer")
        self.assertEqual(host_kind(Insn("fmul", ["d29", "d1"], ["d30"]), True), "fp arithmetic")
        self.assertEqual(host_kind(Insn("fmov", ["d1"], ["x16"]), True), "fp move")
        self.assertEqual(host_kind(Insn("cmp", ["w17"], ["nzcv"]), False), "compare")


class Coded:
    """A disassembled instruction at an address, with a branch target or a memory base."""

    def __init__(self, address, mnemonic, target=None, base=None):
        self.address = address
        self.mnemonic = mnemonic
        self.operands = []
        if target is not None:
            self.operands.append(type("Imm", (), {"type": 2, "imm": target})())
        if base is not None:
            self.operands.append(type("Mem", (), {"type": 3, "mem": type("M", (), {"base": base})()})())

    def reg_name(self, name):
        return name


class LoopTests(unittest.TestCase):
    def test_backward_branches_close_loops_innermost_first(self):
        code = [Coded(0, "ldr", base="sp"), Coded(4, "ldr", base="x28"), Coded(8, "add"),
                Coded(12, "b.ne", target=4), Coded(16, "str", base="sp"), Coded(20, "cbnz", target=0),
                Coded(24, "b", target=40)]
        bodies = loop_bodies(code)
        self.assertEqual([[i.address for i in body] for body in bodies],
                         [[4, 8, 12], [0, 4, 8, 12, 16, 20]])
        self.assertEqual(loop_kinds(bodies[1]),
                         {"stack": 2, "memory": 1, "branch": 2, "other": 1})


if __name__ == "__main__":
    unittest.main()
