"""Test CRT export-name projection on authored module-definition lines."""
import unittest
from crt_exports import exports


class CrtExportTests(unittest.TestCase):
    def test_names_and_kinds_without_aliases_or_decoration(self):
        text = "\n".join([
            'LIBRARY "MSVCR90.dll"', "EXPORTS",
            "_CxxThrowException@8",
            "_NLG_Return ; callee pops like stdcall @12",
            "??0exception@std@@QAE@ABQBDH@Z ; has WINAPI (@8)",
            "_iob DATA",
            "_HUGE @42 DATA",
            "atoll == _atoi64",
            "__fls_getvalue@4 == ___fls_getvalue@4",
        ])
        self.assertEqual(exports(text), {
            "_CxxThrowException": "code", "_NLG_Return": "code",
            "??0exception@std@@QAE@ABQBDH@Z": "code", "_iob": "data", "_HUGE": "data",
        })

    def test_conflicting_kinds_are_rejected(self):
        with self.assertRaises(ValueError):
            exports("_iob DATA\n_iob\n")


if __name__ == "__main__":
    unittest.main()
