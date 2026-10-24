"""Check template extraction from an authored wineboot installation tree."""
import subprocess
import tempfile
import unittest
from pathlib import Path


class PrefixTemplateTests(unittest.TestCase):
    # spec: WINE-SES-6
    def test_system32_subdirectories_keep_installed_modules_and_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            install = root / "install"
            binaries = install / "bin"
            modules = install / "lib/wine/i386-windows"
            binaries.mkdir(parents=True)
            modules.mkdir(parents=True)
            (modules / "wbemprox.dll").write_bytes(b"authored module fixture")
            wine = binaries / "wine"
            wine.write_text(r'''#!/bin/sh
set -eu
[ "$1" = wineboot ] || exit 0
modules="$(dirname "$0")/../lib/wine/i386-windows"
mkdir -p "$WINEPREFIX/drive_c/windows/system32/wbem"
mkdir -p "$WINEPREFIX/drive_c/windows/system32/catroot"
mkdir -p "$WINEPREFIX/drive_c/windows/syswow64"
mkdir -p "$WINEPREFIX/drive_c/windows/globalization"
for file in system.reg user.reg userdef.reg; do
    printf 'WINE REGISTRY Version 2\n' >"$WINEPREFIX/$file"
done
cp "$modules/wbemprox.dll" "$WINEPREFIX/drive_c/windows/system32/wbem/wbemprox.dll"
cp "$modules/wbemprox.dll" "$WINEPREFIX/drive_c/windows/system32/wbemprox.dll"
printf 'installed data' >"$WINEPREFIX/drive_c/windows/system32/wbem/data"
printf 'excluded' >"$WINEPREFIX/drive_c/windows/syswow64/data"
printf 'excluded' >"$WINEPREFIX/drive_c/windows/globalization/data"
''')
            wine.chmod(0o755)
            server = binaries / "wineserver"
            server.write_text("#!/bin/sh\nexit 0\n")
            server.chmod(0o755)
            output = root / "template"
            subprocess.run(
                ["sh", str(Path(__file__).with_name("prefix-template.sh")),
                 str(install), str(output)], check=True,
            )
            self.assertEqual(
                (output / "links.tsv").read_text(),
                "drive_c/windows/system32/wbem/wbemprox.dll\twbemprox.dll\n",
            )
            system = output / "drive_c/windows/system32"
            self.assertEqual((system / "wbem/data").read_text(), "installed data")
            self.assertTrue((system / "catroot").is_dir())
            self.assertFalse((system / "wbemprox.dll").exists())
            self.assertFalse((output / "drive_c/windows/syswow64").exists())
            self.assertFalse((output / "drive_c/windows/globalization").exists())


if __name__ == "__main__":
    unittest.main()
