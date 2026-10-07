import unittest

from stack_report import UNKNOWN, Namer, aggregate, short

MODULES = [
    {"base": 0x1000, "size": 0x1000, "path": r"C:\x\adv-run.exe"},
    {"base": 0x8000, "size": 0x1000, "path": r"C:\Windows\System32\ntdll.dll"},
]


class NamerTests(unittest.TestCase):
    def test_runner_system_and_unknown_frames(self):
        name = Namer(MODULES, {0x10: ["inner<T>", "outer"]},
                     {MODULES[1]["path"]: [(0x100, "NtWaitForSingleObject")]})
        self.assertEqual(name(0x1010), ["inner", "outer"])
        self.assertEqual(name(0x1020), ["adv-run+0x20"])
        self.assertEqual(name(0x8180), ["ntdll.dll!NtWaitForSingleObject"])
        self.assertEqual(name(0x20000), [UNKNOWN])

    def test_short_strips_nested_generics(self):
        self.assertEqual(short("a::<b<c>>::d"), "a::::d")


class AggregateTests(unittest.TestCase):
    STACKS = [
        ["ntdll.dll!Wait", "adv_win32::wait", "adv_runtime::run"],
        ["adv_cpu::helper", "adv_cpu::helper", UNKNOWN],
        ["adv_cpu::helper", "adv_runtime::run"],
        [],
    ]

    def test_self_inclusive_and_leaf(self):
        result = aggregate(self.STACKS)
        self.assertEqual(result["stacks"], 3)
        self.assertEqual(result["leaf"], {"ntdll.dll": 1, "adv-run": 2})
        self.assertEqual(result["self"]["adv_cpu::helper"], 2)
        self.assertEqual(result["inclusive"]["adv_runtime::run"], 2)
        self.assertEqual(result["inclusive"]["adv_cpu::helper"], 2)  # recursion counts once

    def test_focus_keeps_matching_stacks_and_counts_callers(self):
        result = aggregate(self.STACKS, focus="helper")
        self.assertEqual(result["stacks"], 2)
        self.assertEqual(result["callers"], {UNKNOWN: 1, "adv_runtime::run": 1})


if __name__ == "__main__":
    unittest.main()
