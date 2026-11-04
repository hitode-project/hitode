"""Tests for tools/lint.py."""
from pathlib import Path
import re
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lint  # noqa: E402


class LintTests(unittest.TestCase):
    def test_ci_runs_the_host_gate_through_this_script(self):
        workflow = (lint.ROOT / '.github/workflows/core-performance.yml').read_text()
        self.assertIn('python3 tools/lint.py host', workflow)
        self.assertIsNone(re.search(r'cargo clippy --workspace', workflow))

    def test_every_target_denies_warnings(self):
        for target in lint.TARGETS:
            platform, args = lint.command(target)
            self.assertEqual(args[-2:], ['-D', 'warnings'], target)
        self.assertIn('aarch64-apple-ios', lint.command('ios')[1])
        self.assertEqual(lint.command('android')[1][:3], ['cargo', 'ndk', '-t'])


if __name__ == '__main__':
    unittest.main()
