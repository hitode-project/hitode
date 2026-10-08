#!/usr/bin/env python3
"""Synthetic tests for JIT lifetimes, exclusive attribution, and anonymous output.

Run: python -m unittest discover -s tools/performance -p test_arm64_hotspots.py
No device, game files, raw profile, or NDK is required.
"""
import json
from pathlib import Path
import tempfile
import unittest

from arm64_hotspots import RangeEvent, Ranges, classify, host_symbol, load_ranges, summarize


class HotspotsTests(unittest.TestCase):
    def test_range_boundaries_retirement_and_reuse(self):
        ranges = Ranges([RangeEvent(10, "load", 4096, 16, "Tier 1"),
                         RangeEvent(20, "unload", 4096, 16, "Tier 1"),
                         RangeEvent(30, "load", 4096, 8, "Tier 2 Cranelift")])
        self.assertIsNone(ranges.lookup(4096, 9))
        self.assertEqual(ranges.lookup(4096, 10), "Tier 1")
        self.assertIsNone(ranges.lookup(4112, 10))
        self.assertIsNone(ranges.lookup(4096, 20))
        self.assertEqual(ranges.lookup(4103, 30), "Tier 2 Cranelift")
        self.assertIsNone(ranges.lookup(4104, 30))
        with self.assertRaises(ValueError):
            ranges.lookup(4096, 29)

    def test_static_map_rejects_reuse(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "perf.map"
            path.write_text("1000 20 adv-jit-tier1\n1000 10 adv-jit-tier2\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "overlapping"):
                load_ranges(path, static=True)

    def test_android_top_byte_tag_is_not_an_instruction_address(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.tsv"
            path.write_text("10 load b400000000001000 20 adv-jit-tier2\n", encoding="utf-8")
            self.assertEqual(Ranges(load_ranges(path)).lookup(4096, 10), "Tier 2 Cranelift")
        self.assertIsNone(host_symbol("unknown"))

    def test_invalid_lifetime_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "retirement"):
            Ranges([RangeEvent(0, "unload", 4096, 8, "Tier 1")]).lookup(4096, 1)
        with self.assertRaisesRegex(ValueError, "overlapping"):
            Ranges([RangeEvent(0, "load", 4096, 16, "Tier 1"),
                    RangeEvent(1, "load", 4100, 8, "Tier 1")]).lookup(4096, 1)

    def test_leaf_jit_and_host_helpers_do_not_double_count(self):
        self.assertEqual(classify("memcpy", "libc.so", "Tier 1"), "Tier 1")
        self.assertEqual(classify("memcpy", "libc.so", None), "Memory copy/set")
        self.assertEqual(classify("adv_cpu::tier1::sse::call", "adv-run", None), "JIT helper SSE")
        self.assertEqual(classify("cranelift_codegen::compile", "adv-run", None), "Compilation")
        self.assertEqual(classify("adv_cpu::fault_recovery::recover_current", "adv-run", None), "JIT helper memory")
        self.assertEqual(classify("<adv_device::ring::RingVoice>::render", "adv-run", None), "Audio mixing")
        self.assertEqual(classify("pthread_getspecific", "libc.so", None), "Other host")
        self.assertEqual(classify("__read_chk", "libc.so", None), "Kernel/syscall")

    def test_period_weighting_threads_and_no_private_identity(self):
        def sample(tid, comm, ip, period, symbol):
            return dict(pid=123, tid=tid, comm=comm, ip=ip, time=10, period=period,
                        symbol=symbol, dso="/private/device/adv-run", event="cpu-clock:u")
        report = summarize([sample(123, "adv-run", 4096, 9, "guest_private"),
                            sample(124, "adv-background", 8192, 1, "cranelift_codegen::compile")],
                           [RangeEvent(0, "load", 4096, 16, "Tier 2 Cranelift")])
        self.assertEqual(sum(row["period"] for row in report["categories"]), 10)
        self.assertEqual(report["categories"][0]["percent"], 90)
        self.assertEqual(report["top_host_symbols"][0]["percent"], 10)
        self.assertEqual({row["role"] for row in report["threads"]}, {"Guest execution", "Compilation worker"})
        serialized = json.dumps(report)
        for private in ("guest_private", "/private/", '"pid"', '"tid"', '"ip"', '"dso"'):
            self.assertNotIn(private, serialized)
        with self.assertRaisesRegex(ValueError, "no samples"):
            summarize([], [])

    def test_mixed_events_and_guest_labels_are_rejected(self):
        self.assertIsNone(host_symbol("guest_00401234"))
        self.assertIsNone(host_symbol("0xabcdef"))
        with self.assertRaisesRegex(ValueError, "one sampling event"):
            summarize([dict(event="cpu-clock:u"), dict(event="cpu-cycles:u")], [])


if __name__ == "__main__":
    unittest.main()
