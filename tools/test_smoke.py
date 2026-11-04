"""Tests for tools/smoke.py."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import smoke  # noqa: E402


def game(steps):
    return smoke.Game('g', Path('/games/g.exe'), '932', smoke.parse_steps(steps))


class SmokeTests(unittest.TestCase):
    def test_steps_are_checked(self):
        self.assertEqual(smoke.parse_steps('wait 2; capture t; text a b'),
                         [['wait', '2'], ['capture', 't'], ['text', 'a b']])
        for bad in ('jump 1', 'click 1', 'wait x', 'clicks 2 1 1'):
            with self.assertRaises(ValueError, msg=bad):
                smoke.parse_steps(bad)

    def test_clicks_press_and_release_at_the_point(self):
        lines = [line for _, line in smoke.control(game('clicks 2 10 20 1.5'), Path('/o'))]
        self.assertEqual(lines, ['move 10 20', 'mouse-down 10 20', 'mouse-up 10 20'] * 2)
        self.assertEqual(smoke.duration(game('wait 3; clicks 2 1 1 1').steps), 6)

    def test_command_carries_the_full_headless_recipe(self):
        args = smoke.command(Path('/adv-run'), Path('/wine'), game('wait 10'), Path('/o'))
        for flag in ('--headless', '--headless-gpu', '--audio', '--control-stdin'):
            self.assertIn(flag, args)
        self.assertEqual(args[args.index('--wine') + 1], '/wine')
        self.assertEqual(args[args.index('--auto-dialog-ms') + 1], '1000')
        self.assertEqual(args[args.index('--data-dir') + 1], '/o/data')
        self.assertEqual(args[-1], '/games/g.exe')

    def test_panics_and_missing_reports_fail(self):
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            (out / 'run.log').write_text("thread 'main' panicked at x.rs:1:2:\n")
            (out / 'report.json').write_text(json.dumps({'frames': 3, 'guest_exit_code': 0}))
            self.assertIn('panicked', smoke.summarize('g', out, 0)['failure'])
            (out / 'run.log').write_text('')
            self.assertIsNone(smoke.summarize('g', out, 0)['failure'])
            self.assertEqual(smoke.summarize('g', out, 1)['failure'], 'exit 1, guest 0')
            (out / 'report.json').unlink()
            self.assertEqual(smoke.summarize('g', out, 0)['failure'], 'no report')

    def test_the_example_configuration_parses(self):
        example = json.loads((smoke.ROOT / 'tools/smoke.example.json').read_text())
        for entry in example['games'].values():
            smoke.parse_steps(entry['steps'])


if __name__ == '__main__':
    unittest.main()
