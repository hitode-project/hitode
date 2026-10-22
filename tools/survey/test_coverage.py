"""Coverage follows the optional-DLL SDK ordinal aliases used by the registry."""
import unittest

import coverage


class OrdinalCoverage(unittest.TestCase):
    def test_sdk_alias_needs_its_implemented_target(self):
        names = {"DirectSoundEnumerateA": {"dsound.dll"},
                 "DirectSoundEnumerateW": {"dsound.dll"},
                 "DirectSoundCreate8": {"dsound.dll"},
                 "InitCommonControls": {"comctl32.dll"}}
        for dll, ordinal in [("dsound.dll", "#2"), ("dsound.dll", "#3"),
                             ("dsound.dll", "#11"), ("comctl32.dll", "#17")]:
            with self.subTest(dll=dll, ordinal=ordinal):
                self.assertTrue(coverage.covered(names, dll, ordinal))
                self.assertFalse(coverage.covered({}, dll, ordinal))
        self.assertFalse(coverage.covered(names, "dsound.dll", "#12"))
        self.assertFalse(coverage.covered(names, "comctl32.dll", "#2"))

    def test_explicit_ordinal_front_stays_covered(self):
        self.assertTrue(coverage.covered({"#1": {"dsound.dll"}}, "dsound.dll", "#1"))

    def test_core_sdk_ordinals_do_not_override_the_pe_catalog(self):
        self.assertNotIn(("kernel32.dll", "#1"), coverage.ordinal_names())


if __name__ == "__main__":
    unittest.main()
