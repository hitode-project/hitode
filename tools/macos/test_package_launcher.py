"""Launcher packaging checks without building; run with unittest discover -s tools/macos."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
import unittest.mock

spec = importlib.util.spec_from_file_location('package_launcher', Path(__file__).with_name('package_launcher.py'))
package_launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_launcher)


def wine(root: Path, skip: str | None = None) -> Path:
    for required in package_launcher.WINE_REQUIRED:
        if required != skip:
            (root / required).parent.mkdir(parents=True, exist_ok=True)
            (root / required).write_bytes(b'')
    return root


class PackageLauncherTests(unittest.TestCase):
    # spec: HOST-PKG-7
    def test_wine_requirements_match_the_launcher_build_script(self):
        script = (package_launcher.LAUNCHER / 'src-tauri/build/wine.rs').read_text()
        for required in package_launcher.WINE_REQUIRED:
            self.assertIn(f'"{required}"', script)

    # spec: HOST-PKG-7
    def test_incomplete_wine_is_reported_before_building(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = wine(Path(temporary), skip='share/hitode/prefix/system.reg')
            env = {'ADV_WINE_DIST': str(root), 'PATH': os.environ['PATH']}
            self.assertIn('incomplete ADV_WINE_DIST: missing share/hitode/prefix/system.reg',
                          package_launcher.preflight(['mac'], env))
            with self.assertRaisesRegex(ValueError, 'missing share/hitode/prefix/system.reg'):
                package_launcher.wine_dist(env)
            wine(root)
            self.assertEqual(package_launcher.wine_dist(env), root.resolve())

    # spec: HOST-PKG-7
    def test_every_missing_platform_input_is_reported_at_once(self):
        with tempfile.TemporaryDirectory() as temporary:
            version = package_launcher.re.search(r'ndkVersion = "([^"]+)"',
                                                 package_launcher.GRADLE.read_text()).group(1)
            sdk, java = Path(temporary) / 'sdk', Path(temporary) / 'jdk'
            java.mkdir()
            sdk.mkdir()
            env = {'JAVA_HOME': str(java), 'ANDROID_HOME': str(sdk), 'PATH': os.environ['PATH']}
            problems = package_launcher.preflight(['ios', 'android'], env)
            self.assertIn('missing APPLE_DEVELOPMENT_TEAM: set it to your Apple signing team ID', problems)
            self.assertTrue(any(p.startswith(f'missing NDK {version}: sdkmanager') for p in problems), problems)
            self.assertTrue(any(p.startswith('missing Android build tools') for p in problems), problems)
            (sdk / 'ndk' / version).mkdir(parents=True)
            (sdk / 'build-tools/36.0.0').mkdir(parents=True)
            (sdk / 'build-tools/36.0.0/apksigner').write_bytes(b'')
            self.assertFalse(package_launcher.android(env)[1])
            built = package_launcher.environment('android', env)
            self.assertEqual(built['NDK_HOME'], str(sdk / 'ndk' / version))
            self.assertEqual(built['ANDROID_NDK_HOME'], built['NDK_HOME'])

    # spec: HOST-PKG-7
    def test_platform_selection(self):
        _, platforms, command = package_launcher.parse(['android', 'mac'])
        self.assertEqual(platforms, ['mac', 'android'])
        self.assertIsNone(command)
        self.assertEqual(package_launcher.parse(['all'])[1], ['mac', 'ios', 'android'])
        args, platforms, command = package_launcher.parse(['ios', '--install', '--', 'true'])
        self.assertEqual((platforms, args.install, command), (['ios'], '', ['true']))
        for wrong in (['mac', '--side-by-side', 'ab'], ['all', '--', 'true'],
                      ['ios', 'android', '--install', 'serial'], ['android', '--side-by-side', 'A-B']):
            with self.assertRaises(SystemExit), unittest.mock.patch('sys.stderr'):
                package_launcher.parse(wrong)


if __name__ == '__main__':
    unittest.main()
