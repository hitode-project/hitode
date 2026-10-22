"""Test CRT export-name projection on authored module-definition lines."""
import unittest
from crt_exports import exports


class CrtExportTests(unittest.TestCase):
    def test_names_and_kinds_without_aliases_or_decoration(self):
        text = "\n".join([
            'LIBRARY "MSVCR90.dll"', "EXPORTS",
            "; Definition file of MSVCR90.dll",
            "; public: __thiscall std::exception::exception(char const *const &,int)",
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
            "_atoi64": "code", "___fls_getvalue": "code",
        })

    def test_exports_mingw_replaces_are_kept_unless_removed(self):
        text = "\n".join([
            "__msvcrt_exit DATA == exit ; mingw-w64 provides exit() as a wrapper",
            "_crt_atexit == atexit ; rename atexit to _crt_atexit",
            "; _onexit ; disable _onexit for compatibility with DLL builds",
            "; isprint ; replaced by emu, CRT version mishandles TAB character",
            "; tmpfile replaced by emu, CRT version may not use writable temporary directory",
            "; _environ_dll ; removed in Windows NT 3.51",
            "; __C_specific_handler ; disabled i386-chpe32",
            "; sinf ; i386 sinf provided by emu",
            "; _get_pgmptr ; i386 and x64 _get_pgmptr replaced by emu",
            " ; i386 _time64 replaced by emu",
            " ; i386 and x64 memcpy_s replaced by emu",
            "                                    ; i386 cosf provided by emu",
            "_environ DATA",
            "_environ_alias DATA == _environ",
        ])
        self.assertEqual(exports(text), {
            "exit": "code", "atexit": "code", "_onexit": "code", "isprint": "code",
            "tmpfile": "code", "_get_pgmptr": "code", "_time64": "code", "memcpy_s": "code",
            "_environ": "data",
        })

    def test_conflicting_kinds_are_rejected(self):
        with self.assertRaises(ValueError):
            exports("_iob DATA\n_iob\n")


if __name__ == "__main__":
    unittest.main()
