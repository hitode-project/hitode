import unittest

from check_layers import TRANSITIONAL, violations

BASE = [(crate, dep, "normal") for crate, dep in TRANSITIONAL]
CRATES = {name for edge in BASE for name in edge[:2]}


def check(*edges):
    crates = CRATES | {name for edge in edges for name in edge[:2]}
    return violations(crates, BASE + list(edges))


class LayeringTest(unittest.TestCase):
    def test_downward_edges_pass(self):
        self.assertEqual(check(("adv-runtime", "adv-cpu", "normal"),
                               ("adv-run", "adv-machine", "normal"),
                               ("adv-tune-softpal", "adv-abi", "normal")), [])

    def test_upward_and_sibling_edges_fail(self):
        self.assertTrue(check(("adv-cpu", "adv-machine", "normal")))
        self.assertTrue(check(("adv-device", "adv-cpu", "normal")))
        self.assertTrue(check(("adv-win32", "adv-dx", "build")))

    def test_test_only_crates_stay_in_dev_dependencies(self):
        self.assertEqual(check(("adv-cpu", "adv-unicorn", "dev")), [])
        self.assertTrue(check(("adv-runtime", "adv-unicorn", "normal")))
        self.assertTrue(check(("adv-machine", "adv-samples", "normal")))

    def test_unknown_crate_fails(self):
        self.assertTrue(check(("adv-new", "adv-sync", "normal")))

    def test_stale_transitional_edge_fails(self):
        self.assertTrue(violations(CRATES, []))


if __name__ == "__main__":
    unittest.main()
