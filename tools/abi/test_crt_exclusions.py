"""Verify exclusion boundaries on authored decorated export names."""
import unittest
from crt_exclusions import manifest, reason


class CrtExclusionTests(unittest.TestCase):
    def test_old_stream_exclusion_is_limited_to_two_legacy_modules(self):
        for name in ["??0filebuf@@QAE@XZ", "?ws@@YAAAVistream@@AAV1@@Z",
                     "??_7ios@@6B@", "?xsputn@streambuf@@UAEHPBDH@Z"]:
            for module in ["msvcrt20.dll", "msvcrt40.dll"]:
                self.assertEqual(reason(module, name), "legacy iostream family")
            self.assertIsNone(reason("msvcrt.dll", name))

    def test_exception_and_allocation_exports_remain_in_scope(self):
        for name in ["??0exception@@QAE@XZ", "??0bad_cast@@QAE@ABQBD@Z",
                     "??2@YAPAXI@Z", "?terminate@@YAXXZ", "?name@type_info@@QBEPBDXZ",
                     "_CxxThrowException", "__CxxFrameHandler", "_except_handler3"]:
            self.assertIsNone(reason("msvcrt40.dll", name))

    def test_private_set_and_concurrency_namespace_are_exact(self):
        self.assertEqual(reason("api-ms-win-crt-private-l1-1-0.dll", "helper"), "private API set")
        self.assertEqual(reason("msvcr120.dll", "?Alloc@Concurrency@@YAPAXI@Z"), "ConcRT namespace")
        self.assertIsNone(reason("msvcr120.dll", "Concurrency_helper"))

    def test_manifest_retains_data_kind_and_exact_spelling(self):
        self.assertEqual(manifest("# catalog\nmsvcrt20.dll\t??_7ios@@6B@\tdata\n"
                                  "msvcrt20.dll\tmalloc\tcode\n"),
                         [("msvcrt20.dll", "??_7ios@@6B@", "data", "legacy iostream family")])


if __name__ == "__main__":
    unittest.main()
