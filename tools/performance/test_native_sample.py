import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from native_sample import OUTSIDE, Names, function_table, key_code, report
from test_module_profile import tiny_dll  # tools/survey, put on the path by native_sample


class NamesTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.dll = Path(self.directory.name) / "t.dll"
        self.dll.write_bytes(tiny_dll())

    def tearDown(self):
        self.directory.cleanup()

    def test_function_table_from_exports(self):
        self.assertEqual(function_table(self.dll, disassemble=False), [(0x1010, "Run")])
        self.assertEqual(function_table(Path(self.directory.name) / "missing.dll"), [])

    def test_addresses_name_module_and_function(self):
        names = Names([{"base": 0x10000000, "size": 0x3000, "path": str(self.dll)}],
                      disassemble=False)
        self.assertEqual(names.function(0x10001018), ("t.dll", "t.dll!Run+0x8"))
        self.assertEqual(names.function(0x10000800), ("t.dll", "t.dll+0x800"))
        self.assertEqual(names.function(0x20000000), (OUTSIDE, OUTSIDE))

    def test_report_splits_threads(self):
        run = Path(self.directory.name) / "run"
        run.mkdir()
        modules = [{"base": 0x10000000, "size": 0x3000, "path": str(self.dll)}]
        (run / "native-sample.json").write_text(json.dumps({
            "pid": 7, "wow64": True, "wall_seconds": 2.0, "modules": modules,
            "threads": [{"tid": 1, "cpu_seconds": 1.8, "sampled": True},
                        {"tid": 2, "cpu_seconds": 0.5, "sampled": True},
                        {"tid": 3, "cpu_seconds": 0.0, "sampled": False}]}))
        (run / "native-samples.tsv").write_text(
            "perf_ns\ttid\tpc\n1\t1\t268439568\n2\t1\t268439572\n3\t2\t536870912\n")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            report(run, 5, Names(modules, disassemble=False))
        text = output.getvalue()
        self.assertIn("90.0%  sampled", text)
        self.assertIn("all      115.0%", text)
        self.assertIn("100.0%  t.dll!Run", text)
        self.assertIn(f"100.0%  {OUTSIDE}", text)
        self.assertNotIn("     3 ", text)  # idle threads are left out


class KeyTests(unittest.TestCase):
    def test_names_and_codes(self):
        self.assertEqual(key_code("Ctrl"), 0x11)
        self.assertEqual(key_code("0x20"), 0x20)
        self.assertIsNone(key_code(None))


if __name__ == "__main__":
    unittest.main()
