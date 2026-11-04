"""Tests for tools/env.py."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import env  # noqa: E402


class EnvTests(unittest.TestCase):
    def test_unknown_platform_is_rejected(self):
        with self.assertRaises(ValueError):
            env.cargo('windows', {})

    def test_mobile_platforms_need_an_apple_silicon_mac(self):
        with mock.patch.object(env, 'apple', return_value=False):
            self.assertEqual(env.cargo('mac', {'A': '1'}), {'A': '1'})
            with self.assertRaisesRegex(env.Missing, 'android builds on an Apple silicon Mac'):
                env.cargo('android', {})

    def test_set_variables_win_and_missing_ffmpeg_is_reported(self):
        with mock.patch.object(env, 'apple', return_value=True), \
                mock.patch.object(env, 'output', return_value=None):
            built = env.cargo('mac', {'FFMPEG_DIR': '/set', 'RUSTC_WRAPPER': 'none'})
            self.assertEqual((built['FFMPEG_DIR'], built['RUSTC_WRAPPER']), ('/set', 'none'))
            with self.assertRaisesRegex(env.Missing, 'brew install ffmpeg@8'):
                env.cargo('mac', {'RUSTC_WRAPPER': 'none'})

    def test_incomplete_wine_lists_every_missing_file(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(env.Missing) as raised:
                env.wine_dist({'ADV_WINE_DIST': root})
            self.assertEqual(len(raised.exception.problems), len(env.WINE_REQUIRED))

    def test_exports_name_only_what_changed(self):
        self.assertEqual(env.exports({'A': '1', 'B': 'x y'}, {'A': '1'}), "export B='x y'\n")

    def test_usage_without_a_platform(self):
        with mock.patch('sys.stderr'):
            self.assertEqual(env.main([]), 2)
            self.assertEqual(env.main(['mac', '--']), 2)


if __name__ == '__main__':
    unittest.main()
