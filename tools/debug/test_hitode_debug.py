import unittest

from hitode_debug import frame_gaps, parse_modules, place, readable, slices, table


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


if __name__ == "__main__":
    unittest.main()
