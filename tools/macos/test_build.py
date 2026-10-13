"""Validate the pinned decode-only FFmpeg configuration without building it."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('build', Path(__file__).with_name('build.py'))
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class BuildTests(unittest.TestCase):
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


if __name__ == '__main__':
    unittest.main()
