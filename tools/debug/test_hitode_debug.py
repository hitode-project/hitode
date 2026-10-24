import unittest

import tempfile
from collections import Counter
from pathlib import Path

from hitode_debug import Snapshot, analyze, code_parts, form_lines, forms, explain_stall, frame_gaps, instructions, parse_slow, regions, parse_modules, place, readable, slices, table


class ReplyShapeTests(unittest.TestCase):
    def test_timeline_gaps_report_busy_share_of_each_gap(self):
        rows = table(
            "unix_us\tframe\tbusy_us\thost_calls\tnative_entries\tinterpreter_entries\n"
            "1000000\t1\t10000\t5\t1\t0\n"
            "1016000\t2\t12000\t5\t1\t0\n"
            "3016000\t3\t1500000\t9\t4\t2\n")
        gaps = frame_gaps(rows, 50)
        self.assertEqual(len(gaps), 1)
        end, gap, busy, row = gaps[0]
        self.assertEqual((end, gap, busy, row["frame"]), (3016000, 2000.0, 75.0, "3"))

    def test_modules_place_guest_addresses(self):
        modules = parse_modules("0x00400000 0x00100000 Image    aqua.exe\n"
                                "0x62000000 0x00020000 Image    krmovie.dll\n")
        self.assertEqual(place(0x620096e4, modules), "krmovie.dll+0x96e4")
        self.assertEqual(place(0x10, modules), "0x00000010")

    # spec: WINE-UI-6
    def test_wine_samples_are_not_named_from_another_process_module_table(self):
        modules = parse_modules("pid 0x20 (presented):\n"
                                "0x00400000 0x00100000 Image    parent.exe\n"
                                "pid 0x24:\n"
                                "0x00400000 0x00100000 Image    child.exe\n"
                                "modules unavailable: unreadable loader module name\n")
        self.assertEqual(modules, [])
        self.assertEqual(place(0x00401000, modules), "0x00401000")

    def test_samples_fall_into_slices_by_kind(self):
        samples = [{"unix_us": "1000", "kind": "Guest"}, {"unix_us": "1200", "kind": "Idle"},
                   {"unix_us": "251000", "kind": "Guest"}]
        self.assertEqual(slices(samples, 250_000),
                         {0: {"Guest": 1, "Idle": 1}, 250_000: {"Guest": 1}})

    def test_rust_symbols_read_as_paths(self):
        self.assertEqual(readable("_RNvMs_NtCsiHs9LzUTZfi_12adv_cpu_core6memoryNtB4_6Memory5write"),
                         "adv_cpu_core::memory::Memory::write")
        self.assertEqual(readable("_RNvCs9QuqQGgVFFA_9adv_shell13run_execution"),
                         "adv_shell::run_execution")
        self.assertEqual(readable("mach_absolute_time"), "mach_absolute_time")

    def test_snapshot_answers_from_its_sections(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "snap.txt"
            path.write_text("== info\npid=42\n\n== samples (last 30 s)\nunix_us\tkind\n7\tGuest\n\n"
                            "== modules\n0x00400000 0x00100000 Image    aqua.exe\n")
            snapshot = Snapshot(path)
            self.assertEqual(snapshot.pid, 42)
            self.assertEqual(table(snapshot.text("samples 60")), [{"unix_us": "7", "kind": "Guest"}])
            self.assertEqual(parse_modules(snapshot.text("modules"))[0][2], "aqua.exe")
            with self.assertRaises(RuntimeError):
                snapshot.text("timeline 30")

    def test_nearby_units_of_one_module_merge_into_regions(self):
        modules = parse_modules("0x00400000 0x00100000 Image    aqua.exe\n"
                                "0x14f80000 0x00010000 Image    layer.dll\n")
        units = Counter({0x14f81887: 5, 0x14f8176c: 4, 0x14f89000: 1, 0x0043df90: 2})
        self.assertEqual([(c, label) for c, label, _, _ in regions(units, modules)],
                         [(9, "layer.dll+0x176c..0x1887"), (2, "aqua.exe+0x3df90"), (1, "layer.dll+0x9000")])

    def test_slow_rows_parse_with_their_form(self):
        rows = parse_slow(["   total interpreted  faulted   helper  eip        place / form",
                           "  709444           0        0   709444  0x0043df90 aqua.exe+0x3df90 Some(Shr_rm32_CL)"])
        self.assertEqual(rows, [(709444, 0, 0, 709444, 0x43df90, "aqua.exe+0x3df90 Some(Shr_rm32_CL)")])

    def test_verdicts_name_the_dominant_cause(self):
        shares = {"Guest": 0.7, "HostFromGuest": 0.1, "Host": 0.2, "Idle": 0.0}
        verdicts = explain_stall(4500, 99, shares, [(30, "layer.dll+0x176c..0x1887", 0, 0)], [],
                                 [("adv_cpu_a64::translate::Trace::build", 9)])
        self.assertEqual(verdicts, ["generated code: layer.dll+0x176c..0x1887 holds 30 samples",
                                    "translation: adv_cpu_a64::translate::Trace::build (9 samples)"])
        self.assertTrue(explain_stall(500, 10, {"Guest": 0, "HostFromGuest": 0, "Host": 0, "Idle": 0.9},
                                      [], [], [])[0].startswith("waiting"))

    def test_samples_rank_instructions_and_overhead_parts(self):
        modules = parse_modules("0x14f80000 0x00010000 Image    layer.dll\n")
        samples = [{"inst": "0x14f81c10", "part": "body"}] * 5 + \
            [{"inst": "0x14f81c10", "part": "x87-publish"}] * 3 + \
            [{"inst": "-", "part": "stub:lookup"}] * 2 + [{"unix_us": "1"}]
        self.assertEqual(code_parts(samples), Counter({"body": 5, "x87-publish": 3, "stub:lookup": 2}))
        self.assertEqual(instructions(samples, modules).most_common(2),
                         [(("layer.dll+0x1c10", "body"), 5), (("layer.dll+0x1c10", "x87-publish"), 3)])
        verdicts = explain_stall(4000, 99, {"Guest": 0.9, "HostFromGuest": 0, "Host": 0.1, "Idle": 0},
                                 [], [], [], code_parts(samples))
        self.assertEqual(verdicts, ["generated-code overhead: x87-publish takes 30% of the samples in generated code",
                                    "generated-code overhead: stub:lookup takes 20% of the samples in generated code"])

    def test_samples_group_by_instruction_family(self):
        hot = table("samples\teip\tplace\tform\tbody\tcold\tx87-entry\tx87-publish\thost\tstub\t"
                    "body_bytes\tcold_bytes\tx87-entry_bytes\tx87-publish_bytes\n"
                    "6\t0x00401000\ta+0x1000\tFdiv_m64fp\t6\t0\t0\t0\t0\t0\t400\t0\t0\t0\n"
                    "2\t0x00401010\ta+0x1010\tFdivrp_sti_st0\t2\t0\t0\t0\t0\t0\t200\t0\t0\t0\n")
        samples = [{"inst": "0x00401000"}] * 6 + [{"inst": "0x00401010"}] * 2 + [{"inst": "0x00402000"}]
        grouped = forms(samples, hot)
        self.assertEqual(grouped["Fdiv"][0], 6)
        self.assertEqual(grouped["Fdivrp"][:3], (2, Counter({"Fdivrp_sti_st0": 2}), 1))
        self.assertEqual(grouped["other"][0], 1)
        self.assertEqual(grouped["Fdiv"][3], 100)
        self.assertTrue(form_lines(samples, hot, 9)[0].startswith("     6  66.7% Fdiv"))

    def test_analysis_attributes_a_stall_to_its_samples(self):
        snapshot = Snapshot(text="\n".join([
            "== timeline (last 30 s)",
            "unix_us\tframe\tbusy_us\thost_calls\tnative_entries\tinterpreter_entries",
            "1000000\t1\t1000\t1\t1\t0", "1050000\t2\t1000\t1\t1\t0", "3050000\t3\t1990000\t5\t9\t0",
            "== samples (last 30 s)", "unix_us\tkind\teip\tpc\timage\tsymbol",
            *[f"{2000000 + i}\tGuest\t0x00401000\t-\t\t" for i in range(8)],
            "2000100\tHost\t-\t0x10\tlibsystem_kernel.dylib\tread",
            "== modules", "0x00400000 0x00100000 Image    aqua.exe"]))
        report = analyze(snapshot)
        self.assertIn("stall t=0.05..2.05s frame 3: 2000 ms, busy 100%", report)
        self.assertIn("=> generated code: aqua.exe+0x1000 holds 8 samples", report)


if __name__ == "__main__":
    unittest.main()
