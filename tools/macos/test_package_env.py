"""Package environment checks without building; run with unittest discover -s tools/macos."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('package_env', Path(__file__).with_name('package_env.py'))
package_env = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_env)


def wine(root: Path, skip: str | None = None) -> Path:
    for required in package_env.WINE_REQUIRED:
        if required != skip:
            (root / required).parent.mkdir(parents=True, exist_ok=True)
            (root / required).write_bytes(b'')
    return root


class PackageEnvTests(unittest.TestCase):
    # spec: HOST-PKG-7
    def test_wine_requirements_match_the_launcher_build_script(self):
        script = (package_env.LAUNCHER / 'src-tauri/build/wine.rs').read_text()
        for required in package_env.WINE_REQUIRED:
            self.assertIn(f'"{required}"', script)

    # spec: HOST-PKG-7
    def test_missing_or_incomplete_wine_stops_the_build(self):
        with self.assertRaisesRegex(ValueError, 'set ADV_WINE_DIST'):
            package_env.environment('mac', {})
        with tempfile.TemporaryDirectory() as temporary:
            root = wine(Path(temporary), skip='share/hitode/prefix/system.reg')
            with self.assertRaisesRegex(ValueError, 'missing share/hitode/prefix/system.reg'):
                package_env.environment('mac', {'ADV_WINE_DIST': str(root)})
            wine(root)
            env = package_env.environment('mac', {'ADV_WINE_DIST': str(root)})
            self.assertEqual(env['ADV_WINE_DIST'], str(root.resolve()))

    # spec: HOST-PKG-7
    def test_platform_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = wine(Path(temporary) / 'wine')
            with self.assertRaisesRegex(ValueError, 'APPLE_DEVELOPMENT_TEAM'):
                package_env.environment('ios', {'ADV_WINE_DIST': str(root)})
            version = package_env.re.search(r'ndkVersion = "([^"]+)"',
                                            package_env.GRADLE.read_text()).group(1)
            sdk = Path(temporary) / 'sdk'
            java = Path(temporary) / 'jdk'
            java.mkdir()
            sdk.mkdir()
            given = {'ADV_WINE_DIST': str(root), 'JAVA_HOME': str(java), 'ANDROID_HOME': str(sdk)}
            with self.assertRaisesRegex(ValueError, f'NDK {version}'):
                package_env.environment('android', given)
            (sdk / 'ndk' / version).mkdir(parents=True)
            env = package_env.environment('android', given)
            self.assertEqual(env['NDK_HOME'], str(sdk / 'ndk' / version))
            self.assertEqual(env['ANDROID_NDK_HOME'], env['NDK_HOME'])


if __name__ == '__main__':
    unittest.main()
