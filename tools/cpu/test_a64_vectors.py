import tempfile
import unittest
from pathlib import Path
from unittest import mock

import a64_vectors
from a64_vectors import main, parse, render


def fake_assemble(lines):
    return [f"{i:08x}" for i, _ in enumerate(lines, 1)]


class FormatTests(unittest.TestCase):
    def test_round_trip_drops_comments_and_blank_lines(self):
        rows = parse("# header\n\nd503201f\tnop\n\tret\n")
        self.assertEqual(rows, [("d503201f", "nop"), ("", "ret")])
        self.assertEqual(parse(render(rows)), rows)

    def test_a_line_without_assembly_is_rejected(self):
        with self.assertRaises(ValueError):
            parse("d503201f\n")


class CommandTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.file = Path(self.tmp.name) / "encode.tsv"
        patcher = mock.patch.object(a64_vectors, "assemble", side_effect=fake_assemble)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.tmp.cleanup)

    def test_add_then_check_is_clean(self):
        self.assertEqual(main(["--file", str(self.file), "--add", "nop", "ret"]), 0)
        self.assertEqual(parse(self.file.read_text()), [("00000001", "nop"), ("00000002", "ret")])
        self.assertEqual(main(["--file", str(self.file), "--check"]), 0)

    def test_check_reports_a_stale_word_without_writing(self):
        self.file.write_text("ffffffff\tnop\n")
        self.assertEqual(main(["--file", str(self.file), "--check"]), 1)
        self.assertEqual(self.file.read_text(), "ffffffff\tnop\n")

    def test_duplicate_assembly_is_rejected(self):
        self.file.write_text("\tnop\n\tnop\n")
        with self.assertRaises(SystemExit):
            main(["--file", str(self.file)])


if __name__ == "__main__":
    unittest.main()
