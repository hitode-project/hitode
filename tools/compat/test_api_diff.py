"""Synthetic trace tests. Temporary inputs use the caller's redirected TEMP."""
import tempfile
from pathlib import Path
import unittest

import api_diff as diff
import generate_hooks

ROOT = Path(__file__).resolve().parents[2]
SIGNATURES = diff.load_signatures(ROOT / "data/abi/signatures.tsv")


class ApiDiffTests(unittest.TestCase):
    def traces(self, native, adv, ignore=()):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "native.tsv").write_text(native, encoding="utf-8")
            (path / "adv.tsv").write_text(adv, encoding="utf-8")
            a, sa = diff.read_trace(path / "native.tsv", SIGNATURES, ignore)
            b, sb = diff.read_trace(path / "adv.tsv", SIGNATURES, ignore)
            return diff.compare(a, b), a, b, sa, sb

    def test_handles_pointers_strings_and_threads(self):
        native = ("1\t0\tkernel32!CreateEventA\tnull,1,0,s8:66697874757265\th:44\t1\n"
                  "1\t0\tkernel32!SetEvent\th:44\t1\t2\n"
                  "2\t0\tkernel32!CloseHandle\th:44\t1\t1\n")
        adv = ("7\t3\tkernel32.dll!CreateEventA\t0,1,0,800000\t100\n"
               "7\t3\tkernel32.dll!SetEvent\t100\t1\n"
               "9\t3\tkernel32.dll!CloseHandle\t100\t1\n")
        report, a, _, _, _ = self.traces(native, adv)
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(a[0].args, ["null", "0x1", "0x0", "string:present"])
        self.assertIn("arg3:string_content", a[0].unobserved)

    def test_first_return_divergence_and_context(self):
        report, *_ = self.traces(
            "1\t0\tkernel32!SetEvent\th:20\t0\t1\n",
            "1\t0\tkernel32!SetEvent\t100\t1\n")
        first = report["first_divergence"]
        self.assertEqual(first["fields"], ["return"])
        self.assertEqual(first["native"]["call"], 1)
        self.assertEqual(len(first["adv_context"]), 1)

    def test_insert_delete_ignore_and_ordinals(self):
        native = ("1\t0\tkernel32!Sleep\t1\t0\n"
                  "1\t0\tkernel32!SetEvent\t20\t1\n"
                  "1\t0\tkernel32!ResetEvent\t20\t1\n")
        adv = ("1\t0\tkernel32!SetEvent\t100\t1\n"
               "1\t0\tkernel32!ResetEvent\t100\t1\n")
        report, *_ = self.traces(native, adv)
        self.assertEqual(report["first_divergence"]["kind"], "native_only")
        self.assertIsNone(report["first_divergence"]["adv"])
        self.assertEqual(report["threads"][0]["counts"]["matched"], 2)
        report, a, _, sa, _ = self.traces(native, adv, ["*!Sleep"])
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(a[0].ordinal, 2)
        self.assertEqual(sa["ignored"], 1)

    def test_order_and_arguments(self):
        report, *_ = self.traces("1\t0\tkernel32!SetEvent\t20\t1\n",
                                "1\t0\tkernel32!ResetEvent\t100\t1\n")
        self.assertEqual(report["first_divergence"]["kind"], "order")
        report, *_ = self.traces("1\t0\tkernel32!Sleep\t1\t0\n",
                                "1\t0\tkernel32!Sleep\t2\t0\n")
        self.assertEqual(report["first_divergence"]["fields"], ["arg0"])

    def test_handle_relation_and_lifetime(self):
        native = ("1\t0\tkernel32!SetEvent\th:20\t1\t1\n"
                  "1\t0\tkernel32!ResetEvent\th:20\t1\t2\n")
        adv = ("1\t0\tkernel32!SetEvent\t100\t1\n"
               "1\t0\tkernel32!ResetEvent\t101\t1\n")
        report, *_ = self.traces(native, adv)
        self.assertEqual(report["first_divergence"]["fields"], ["arg0"])
        report, *_ = self.traces(native.replace("ResetEvent", "CloseHandle") +
                                "1\t0\tkernel32!SetEvent\th:20\t1\t3\n",
                                adv.replace("101", "100").replace("ResetEvent", "CloseHandle") +
                                "1\t0\tkernel32!SetEvent\t200\t1\n")
        self.assertIsNone(report["first_divergence"])

    def test_nondeterminism_and_64_bit_stack(self):
        report, a, b, *_ = self.traces(
            "1\t0\tkernel32!GetTickCount\t\t1234\t1\n"
            "1\t0\tkernel32!OpenProcess\t10,0,111\th:44\t2\n"
            "1\t0\tkernel32!SetFilePointerEx\th:44,100000002,ptr,0\t1\t3\n",
            "1\t0\tkernel32!GetTickCount\t\t9999\n"
            "1\t0\tkernel32!OpenProcess\t10,0,222\t88\n"
            "1\t0\tkernel32!SetFilePointerEx\t88,2,1,900000,0\t1\n")
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(a[-1].args[1], b[-1].args[1])

    def test_process_comments_terminals_and_partial_capture(self):
        report, _, _, sa, sb = self.traces(
            "# child_untraced\tCreateProcessW\t1234\n"
            "# capture_stop\tseconds\t1\n"
            "# terminal\texit\t1\t0\tkernel32!ExitProcess\t0\t0\n",
            "# process\t00000400\n"
            "# terminal\texit\t8\t0\tkernel32!ExitProcess\t0\t0\tOriginEsp=ignored\n")
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(sa["children_untraced"], 1)
        self.assertTrue(sa["capture_stopped"])
        self.assertEqual(sb["processes"], 1)
        report, *_ = self.traces("1\t0\tkernel32!Sleep\t1\t0\n",
                                "1\t0\tkernel32!Sleep\t1\t0\n1\t0\tkernel32!Sleep\t2\t0\n")
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(report["divergences"][0]["kind"], "capture_tail")

    def test_malformed_and_unknown_do_not_echo_text(self):
        report, a, _, sa, _ = self.traces(
            "invalid\n1\t0\tfixture!Unknown\tprivate_text\tprivate_text\n",
            "1\t0\tfixture!Unknown\tother_text\tother_text\n")
        self.assertEqual(sa["malformed"], 1)
        self.assertEqual(sa["unknown_signatures"], 1)
        self.assertNotIn("private_text", str(a[0].view()))
        self.assertIsNone(report["first_divergence"])

    def test_generator_coverage_and_abi(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "abi.tsv").write_text(
                "kernel32.dll\tOwned\tstdcall\tu64\tu64\ta:u64:u64:2|b:void*:ptr:1\n"
                "user32.dll\tVariadic\tvariadic\tu32\tu32\t\n"
                "user32.dll\tAggregate\tstdcall\taggregate\tstruct\t\n", encoding="utf-8")
            coverage = generate_hooks.generate(path / "abi.tsv", path / "hooks.inc")
            code = (path / "hooks.inc").read_text()
            self.assertEqual(coverage["generated"], 1)
            self.assertEqual(len(coverage["excluded"]), 2)
            self.assertIn("unsigned long long __stdcall hook_0(DWORD a0, DWORD a1, DWORD a2)", code)
            self.assertIn("{7,0,2},{4,2,1}", code)

    def test_coverage_and_sign_extension(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.tsv"
            path.write_text("1\t0\tkernel32!Sleep\t1\t0\n"
                            "1\t0\tkernel32!GetThreadPriority\t100\tffffffffffffffff\n", encoding="utf-8")
            rows, stats = diff.read_trace(path, SIGNATURES, covered={"kernel32.dll!GetThreadPriority"})
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0].ret, "0xffffffff")
            self.assertEqual(stats["outside_native_coverage"], 1)

    def test_native_main_thread_is_not_first_worker(self):
        report, a, *_ = self.traces("2\t0\tkernel32!Sleep\t1\t0\t1\n"
                                   "1\t0\tkernel32!Sleep\t1\t0\t1\n",
                                   "0\t0\tkernel32!Sleep\t1\t0\n")
        self.assertEqual(a[0].thread, 2)
        self.assertEqual(report["threads"][0]["native_thread"], [1, 1])

    def test_boolean_path_length_alias_and_initial_process(self):
        report, _, b, *_ = self.traces(
            "1\t0\tntdll!RtlGetCurrentDirectory_U\t208,s16:0041\t44\t1\n"
            "1\t0\tntdll!RtlLockHeap\th:20\t7ffe0301\t2\n"
            "1\t0\tntdll!ZwClose\th:20\t0\t3\n",
            "0\t0\tntdll!RtlGetCurrentDirectory_U\t208,800000\te\n"
            "# process\t00020000\n# process\t00001000\n"
            "0\t0\tntdll!RtlLockHeap\t100\t1\n"
            "0\t0\tntdll!NtClose\t100\t0\n")
        self.assertIsNone(report["first_divergence"])
        self.assertEqual({r.process for r in b}, {1})

    def test_sdk_supplemental_process_info(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "abi.tsv").write_text(
                "kernel32.dll\tCreateProcessInternalW\tstdcall\ti32\tBOOL\t"
                "a:HANDLE:handle:1|info:PROCESS_INFORMATION*:ptr:1\n", encoding="utf-8")
            generate_hooks.generate(path / "abi.tsv", path / "hooks.inc")
            self.assertIn("0,0,1,64}", (path / "hooks.inc").read_text())

    def test_tls_zero_is_valid_and_encoded_null_is_opaque(self):
        report, *_ = self.traces(
            "1\t0\tkernel32!TlsAlloc\t\t5\t1\n"
            "1\t0\tkernel32!TlsSetValue\t5,ptr\t1\t2\n"
            "1\t0\tkernel32!EncodePointer\tnull\tptr\t3\n"
            "1\t0\tkernel32!DecodePointer\tptr\tnull\t4\n",
            "0\t0\tkernel32!TlsAlloc\t\t0\n"
            "0\t0\tkernel32!TlsSetValue\t0,800000\t1\n"
            "0\t0\tkernel32!EncodePointer\t0\t0\n"
            "0\t0\tkernel32!DecodePointer\t0\t0\n")
        self.assertIsNone(report["first_divergence"])

    def test_long_repeat_gap_keeps_next_anchor(self):
        native = "1\t0\tkernel32!Sleep\t1\t0\n" * 30 + "1\t0\tkernel32!SetEvent\t20\t1\n"
        adv = "1\t0\tkernel32!Sleep\t1\t0\n1\t0\tkernel32!SetEvent\t100\t1\n"
        report, *_ = self.traces(native, adv)
        self.assertEqual(report["threads"][0]["counts"]["matched"], 2)
        self.assertEqual(report["threads"][0]["counts"]["native_only"], 29)

    def test_public_lookup_symbols_and_include(self):
        report, *_ = self.traces(
            "1\t0\tkernel32!GetProcAddress\th:20,s8:4765745469636b436f756e74\tptr\t1\n",
            "# resolve\tignored\tkernel32.dll!GetCurrentThreadId\tignored\n"
            "0\t0\tkernel32!GetProcAddress\t100,800000\t900000\n")
        self.assertEqual(report["first_divergence"]["fields"], ["symbol"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.tsv"
            path.write_text("1\t0\tkernel32!Sleep\t1\t0\n"
                            "1\t0\tkernel32!SetEvent\t20\t1\n", encoding="utf-8")
            rows, stats = diff.read_trace(path, SIGNATURES, include=["*!SetEvent"])
            self.assertEqual(len(rows), 1)
            self.assertEqual(stats["ignored"], 1)

    def test_message_carriers_and_window_code_slots_are_categories(self):
        report, a, *_ = self.traces(
            "1\t0\tuser32!SetWindowLongA\th:20,fffffffc,ptr\tptr\t1\n"
            "1\t0\tuser32!SendMessageA\th:20,111,ptr,ptr\tptr\t2\n",
            "0\t0\tuser32!SetWindowLongA\t100,fffffffc,12345000\t22345000\n"
            "0\t0\tuser32!SendMessageA\t100,111,32345000,42345000\t52345000\n")
        self.assertIsNone(report["first_divergence"])
        self.assertEqual(a[0].args[2], "pointer:present")
        self.assertEqual(a[1].ret, "pointer:present")


if __name__ == "__main__":
    unittest.main()
