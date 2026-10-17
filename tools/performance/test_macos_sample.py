import unittest

from macos_sample import thread_leaves

TEXT = """Call graph:
    10 Thread_1: main
    + 10 mach_msg2_trap  (in libsystem_kernel.dylib) + 8  [0x18a]
    9 Thread_2: adv-execution
    + 5 ???  (in <unknown binary>)  [0x1000]
    + ! 3 ???  (in <unknown binary>)  [0x1010]
    + ! 1 _RNvadv_cpu_a64native9interpret  (in adv-launcher) + 4  [0x5000]
    + 4 poll  (in libsystem_kernel.dylib) + 8  [0x18b]

Total number in stack (recursive counted multiple times):
"""


class ThreadLeavesTests(unittest.TestCase):
    def test_leaves_subtract_children_and_keep_jit_addresses(self):
        leaves = thread_leaves(TEXT, "adv-execution")
        self.assertEqual(leaves, [("???", 0x1000, 1), ("???", 0x1010, 3),
                                  ("_RNvadv_cpu_a64native9interpret", 0x5000, 1), ("poll", 0x18b, 4)])
        self.assertEqual(thread_leaves(TEXT, "main"), [("mach_msg2_trap", 0x18a, 10)])


if __name__ == "__main__":
    unittest.main()
