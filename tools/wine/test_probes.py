"""Check probe build lines and the single reader of the Wine installation."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import probes  # noqa: E402

GCC = probes.COMPILER


class ProbeTests(unittest.TestCase):
    def source(self, directory: Path, text: str) -> Path:
        path = directory / "probe.c"
        path.write_text(text)
        return path

    def test_each_build_line_names_one_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = self.source(Path(temporary), f"""/* Two variants.
 * Build: {GCC} -O2 probe.c -o one.exe -lgdi32
 * Build: {GCC} -O2 -DTWO -o two.exe probe.c */
int main(void) {{ return 0; }}
""")
            found, errors = probes.commands(path)
        self.assertEqual(errors, [])
        self.assertEqual([output for output, _ in found], ["one.exe", "two.exe"])
        self.assertEqual(found[1][1][:3], [GCC, "-O2", "-DTWO"])

    def test_malformed_probes_are_reported(self):
        cases = {
            "/* No recipe. */\n": "no `Build:",
            f"/* Build: {GCC} -O2 probe.c */\n": "must name -o OUTPUT",
            f"/* Build: {GCC} -O2 -o out/x.exe probe.c */\n": "not a path",
            "/* Build: clang probe.c -o x.exe */\n": f"must start with {GCC}",
        }
        for text, message in cases.items():
            with tempfile.TemporaryDirectory() as temporary:
                _, errors = probes.commands(self.source(Path(temporary), text))
            self.assertEqual(len(errors), 1, text)
            self.assertIn(message, errors[0])

    def test_only_the_helper_reads_the_installation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            helper = root / probes.HELPER
            helper.parent.mkdir(parents=True)
            helper.write_text('std::env::var_os("ADV_WINE_DIST")\n')
            test = root / "crates/adv-x/tests/t.rs"
            test.parent.mkdir(parents=True)
            test.write_text('let a = 1;\nstd::env::var_os("ADV_WINE_PROBES");\n')
            self.assertEqual(probes.direct_reads(root), [
                f"crates/adv-x/tests/t.rs:2: read the Wine installation through {probes.HELPER}"
            ])


if __name__ == "__main__":
    unittest.main()
