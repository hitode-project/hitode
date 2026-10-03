import unittest

from scan import scan_line


def found(line: str, path: str = "crates/x/src/lib.rs", terms: list[str] | None = None) -> list[str]:
    return scan_line(path, line, terms or [])


class ScanLine(unittest.TestCase):
    def test_host_drive_paths(self):
        self.assertTrue(found('let dir = "D:/games/title";'))
        self.assertTrue(found(r'Path::new("E:\samples\x")'))
        self.assertTrue(found(r"see D:\Code\adv-host"))

    def test_host_profile_paths(self):
        self.assertTrue(found(r'"C:\Users\someone\AppData"'))
        self.assertTrue(found("C:/Users/1001/AppData/Local/Temp"))
        self.assertTrue(found("cd /c/Users/me/work"))
        self.assertTrue(found("/home/alice/src"))
        self.assertTrue(found('"/Users/bob/Library"'))
        self.assertTrue(found("/mnt/d/games"))

    def test_guest_paths_are_allowed(self):
        self.assertFalse(found(r'format!("APPDATA=C:\Users\{}\AppData\Roaming", name)'))
        self.assertFalse(found('"C:/Users/Player/Documents/Title/save/slot.dat"'))
        self.assertFalse(found(r'"C:\Users\Public\Documents"'))
        self.assertFalse(found(r"[string]$LlvmBin = 'C:\Program Files\LLVM\bin'"))
        self.assertFalse(found(r'HostDir::new(game, "C:\Game")'))
        self.assertFalse(found("C:/Users"))
        self.assertFalse(found("/home/runner/work"))
        self.assertFalse(found("https://example.com/Users/x"))

    def test_emails(self):
        self.assertTrue(found("contact someone@gmail.com"))
        self.assertFalse(found("1+someone@users.noreply.github.com"))
        self.assertFalse(found("user@example.com"))
        self.assertFalse(found("a@b.com", path="vendor/cpal/Cargo.toml"))

    def test_terms_and_allow_marker(self):
        self.assertEqual(found("the Secret Title", terms=["secret title"]), ["local term #1"])
        self.assertFalse(found("D:/games  # privacy: allow"))

    def test_scanner_examples_only_check_terms(self):
        self.assertFalse(found("D:/games", path="tools/privacy/test_scan.py"))
        self.assertTrue(found("x", path="tools/privacy/test_scan.py", terms=["x"]))


if __name__ == "__main__":
    unittest.main()
