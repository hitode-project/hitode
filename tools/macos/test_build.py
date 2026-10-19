"""Validate pinned FFmpeg configuration and release path mapping without building FFmpeg."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import unittest.mock

spec = importlib.util.spec_from_file_location('build', Path(__file__).with_name('build.py'))
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class BuildTests(unittest.TestCase):
    # spec: HOST-PKG-6
    def test_release_rust_file_metadata_uses_the_checkout_mapping(self):
        output = build.ROOT / 'target'
        output.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=output) as temporary:
            source = Path(temporary) / 'probe.rs'
            binary = Path(temporary) / 'probe'
            source.write_text('fn main() { println!("{}", file!()); }')
            subprocess.run(['rustc', *build.remap_flags(), str(source), '-o', str(binary)], check=True)
            result = subprocess.check_output([str(binary)], text=True).strip()
            self.assertTrue(result.startswith('/hitode/target/'), result)
            self.assertNotIn(str(Path.home()), result)

    # spec: HOST-PKG-1
    def test_decode_only_configuration_covers_media_contract(self):
        profile = json.loads(build.PROFILE.read_text())
        args = build.configure(profile, Path('/build/install'))
        for option in ['--disable-everything', '--disable-autodetect', '--disable-network',
                       '--disable-shared', '--enable-static', '--disable-encoders',
                       '--disable-muxers', '--disable-gpl', '--disable-nonfree']:
            self.assertIn(option, args)
        self.assertTrue({'mpeg1video', 'mpeg4', 'wmv2', 'wmapro', 'h264', 'pcm_s16le'} <= set(profile['decoders']))
        self.assertTrue({'asf', 'avi', 'mpegps', 'wav'} <= set(profile['demuxers']))
        self.assertIn('mpeg4video', profile['parsers'])
        self.assertFalse(any('--enable-lib' in arg or '--enable-encoder=' in arg for arg in args))
        self.assertEqual(len(profile['sha256']), 64)

    # spec: HOST-PKG-1
    def test_android_configuration_cross_compiles_with_the_ndk(self):
        profile = json.loads(build.PROFILE.read_text())
        with unittest.mock.patch.dict(build.os.environ, {'ANDROID_NDK_HOME': '/ndk'}):
            args = build.configure(profile, Path('/build/install'), 'android')
        self.assertIn('--target-os=android', args)
        self.assertIn('--cc=/ndk/toolchains/llvm/prebuilt/darwin-x86_64/bin/'
                      f"aarch64-linux-android{profile['minimum_android']}-clang", args)
        self.assertIn('--disable-shared', args)


if __name__ == '__main__':
    unittest.main()
