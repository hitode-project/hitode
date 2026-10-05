import tempfile
import unittest
from pathlib import Path

from analyze_schedule import labelled_gaps, trace_model
from attribute_pc import JitMap, attribute, component, label_samples, summarize
from test_analyze_schedule import row


class JitMapTests(unittest.TestCase):
    def test_reused_base_uses_the_body_live_at_the_sample(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            (directory / "jit-1.tsv").write_text(
                "100\t4096\t64\t65536\tspeed\t10\t3\n"
                "500\t4096\t32\t69632\tspeed\t8\t2\n", encoding="utf-8")
            (directory / "insn-1.tsv").write_text(
                "100\t4096\t0\t16\t65536\tcheck\tint.mem\n"
                "100\t4096\t16\t40\t65536\tbody\tint.mem\n"
                "100\t4096\t48\t60\t65540\tbody\tx87\n"
                "500\t4096\t8\t20\t69634\tbody\tx87\n", encoding="utf-8")
            jit = JitMap(directory)
        early = jit.lookup(4096 + 20, 200)
        self.assertEqual((early["region"], early["part"], early["class"]), (65536, "body", "int.mem"))
        pure = jit.lookup(4096 + 44, 200)
        self.assertEqual((pure["pc"], pure["part"], pure["class"]), (65540, "pure", "x87"))
        self.assertEqual(jit.lookup(4096 + 62, 200)["part"], "untagged")
        late = jit.lookup(4096 + 10, 600)
        self.assertEqual((late["region"], late["pc"], late["class"]), (69632, 69634, "x87"))
        self.assertEqual(jit.lookup(4096 + 4, 600)["part"], "prologue")
        self.assertEqual(jit.lookup(4096 + 25, 600)["part"], "untagged")
        self.assertIsNone(jit.lookup(4096 + 40, 600))  # Beyond the live 32-byte body.
        self.assertIsNone(jit.lookup(4096, 50))  # Before any compilation.


class ComponentTests(unittest.TestCase):
    def test_innermost_project_frame_names_the_component(self):
        self.assertEqual(component(["core::ptr::copy", "adv_cpu::floating::x87_call"]), "adv_cpu::floating")
        self.assertEqual(component(["<adv_cpu::Executor>::run"]), "adv_cpu")
        self.assertEqual(component(["alloc::vec::Vec<u8>::push"]), "rust-runtime")
        self.assertEqual(component(["cranelift_codegen::machinst::lower"]), "cranelift")
        self.assertEqual(component([]), "unsymbolized")


class AttributionTests(unittest.TestCase):
    def test_samples_split_by_gap_cause_context_and_part(self):
        rows = [
            row(0, "mark"), row(0, "frame", 0, 1),
            row(1, "run_start", 0), row(90, "run_end", 0, 0, 89_000_000, 89_000_000),
            row(100, "frame", 0, 2), row(100, "block", 0, 1), row(110, "frame", 0, 3), row(120, "observe"),
        ]
        model = trace_model(rows)
        gaps = labelled_gaps(rows, model)
        self.assertEqual([gap["cause"] for gap in gaps], ["execution", "idle"])
        modules = [{"base": 0x1000, "size": 0x100, "path": "runner.exe"},
                   {"base": 0x9000, "size": 0x100, "path": "C:/x/NTDLL.dll"}]

        class Jit:
            def lookup(self, rip, unix_ns):
                if rip == 0x5000:
                    return {"region": 1, "mode": "speed", "pc": 7, "part": "check", "class": "x87.mem"}
                return None

        samples = [
            {"rip": 0x5000, "unix_ns": 0, "trace_ns": 10_000_000},
            {"rip": 0x1010, "unix_ns": 0, "trace_ns": 20_000_000},
            {"rip": 0x9010, "unix_ns": 0, "trace_ns": 105_000_000},
            {"rip": 0x7777, "unix_ns": 0, "trace_ns": 130_000_000},  # After the window.
        ]
        labels = label_samples(samples, modules, Jit(), {0x10: ["adv_cpu::memory::read"]})
        windows = attribute(samples, labels, model, gaps, long_ms=50)
        self.assertEqual(windows["all"]["samples"], 3)
        execution = summarize(windows["execution"], 5)
        self.assertEqual(execution["buckets_pct"], {"jit/x87.mem/check": 50.0, "host/adv_cpu::memory": 50.0})
        self.assertEqual(execution["contexts_pct"], {"ctx0": 100.0})
        self.assertEqual(execution["guest_instructions"], [{"pc": "0x7", "class": "x87.mem", "part": "check", "pct": 50.0}])
        self.assertEqual(summarize(windows["short"], 5)["buckets_pct"], {"module/ntdll.dll": 100.0})
        self.assertEqual(summarize(windows["short"], 5)["contexts_pct"], {"outside-run": 100.0})


if __name__ == "__main__":
    unittest.main()
