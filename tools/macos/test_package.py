"""Headless packaging checks; run with unittest discover -s tools/macos."""
import importlib.util
from pathlib import Path
import tempfile
import json
import io
import tarfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('package', Path(__file__).with_name('package.py'))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class PackageTests(unittest.TestCase):
    # spec: HOST-PKG-2
    def test_macho_load_commands_and_versions(self):
        self.assertEqual(package.dependencies('file:\n\t@rpath/liba.dylib (compatibility version 1.0.0, current version 2.0.0)\n'), ['@rpath/liba.dylib'])
        self.assertEqual(package.minimum_os('cmd LC_BUILD_VERSION\nminos 14.0\nsdk 27.0'), (14, 0))
        self.assertEqual(package.minimum_os('cmd LC_VERSION_MIN_MACOSX\ncmdsize 16\nversion 11.0'), (11, 0))
        self.assertFalse(package.system_library('/opt/homebrew/lib/a.dylib'))

    # spec: HOST-PKG-2
    def test_dynamic_runner_is_rejected(self):
        with patch.object(package, 'run', side_effect=['cmd LC_BUILD_VERSION\nminos 11.0',
                'runner:\n\t/opt/homebrew/lib/libavcodec.dylib (compatibility version 1.0.0)']):
            with self.assertRaisesRegex(ValueError, 'non-system dylibs'):
                package.package(Path('runner'), Path('unused'), '-', None)

    # spec: HOST-PKG-4
    def test_parent_license_from_exact_crate_revision(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            crate = root / 'crate'
            crate.mkdir()
            (crate / 'Cargo.toml').touch()
            (crate / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': 'abc'}, 'path_in_vcs': 'crates/child'}))
            cache = root / 'target/macos-license-cache'
            cache.mkdir(parents=True)
            with tarfile.open(cache / 'abc.tar.gz', 'w:gz') as archive:
                folder = tarfile.TarInfo('repo-abc')
                folder.type = tarfile.DIRTYPE
                archive.addfile(folder)
                text = b'Copyright upstream; test license'
                member = tarfile.TarInfo('repo-abc/LICENSE')
                member.size = len(text)
                archive.addfile(member, io.BytesIO(text))
            with patch.object(package, 'ROOT', root):
                count = package.rust_licenses({'manifest_path': str(crate / 'Cargo.toml'), 'repository': 'https://github.com/example/repo', 'name': 'child', 'license': 'MIT'}, root / 'notices')
            self.assertEqual(count, 1)
            self.assertEqual((root / 'notices/LICENSE').read_bytes(), text)

    # spec: HOST-PKG-3
    def test_only_jit_entitlement_and_no_adhoc_notarization(self):
        self.assertEqual(package.ENTITLEMENTS, {'com.apple.security.cs.allow-jit': True})
        with self.assertRaises(ValueError):
            package.package(Path('unused'), Path('unused'), '-', 'profile')


if __name__ == '__main__':
    unittest.main()
