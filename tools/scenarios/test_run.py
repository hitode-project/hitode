"""The scenario gate's judgment, on recorded results."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402


def scenario(spec, digest="d"):
    return {"spec": spec, "digest": digest}


def result(passed=True, outputs=None, steady=None, levels=None):
    return {"passed": passed, "exit": 0 if passed else 64, "stop": "guest_exit",
            "outputs": outputs or {"out.bin": "a"}, "steady": steady or {}, "levels": levels or {}}


def judge(base, head, base_on_base, base_on_head, changed_on_base=None, changed_on_head=None, again=None):
    changed_on_head = changed_on_head or {}
    return run.judge(base, head, {
        "base_on_base": base_on_base,
        "base_on_head": base_on_head,
        "changed_on_base": changed_on_base or {},
        "changed_on_head": changed_on_head,
        "changed_again": again if again is not None else changed_on_head,
    })


class Judge(unittest.TestCase):
    def test_unchanged_behavior_passes(self):
        base = {"s": scenario(["GFX-1"])}
        self.assertEqual(judge(base, base, {"s": result()}, {"s": result()}), [])

    def test_a_base_scenario_failing_on_head_fails(self):
        base = {"s": scenario(["GFX-1"])}
        failures = judge(base, base, {"s": result()}, {"s": result(False)})
        self.assertIn("passed on base, fails on head", failures[0])

    def test_an_unclaimed_output_change_fails(self):
        base = {"s": scenario(["GFX-1"])}
        failures = judge(base, base, {"s": result()}, {"s": result(outputs={"out.bin": "b"})})
        self.assertIn("no claiming scenario", failures[0])

    def test_a_new_scenario_failing_on_base_claims_its_spec(self):
        base = {"s": scenario(["GFX-1"])}
        head = {"s": base["s"], "t": scenario(["GFX-1"])}
        failures = judge(base, head, {"s": result()}, {"s": result(outputs={"out.bin": "b"})},
                         changed_on_base={"t": result(False)}, changed_on_head={"t": result()})
        self.assertEqual(failures, [])

    def test_a_claim_covers_only_its_own_spec(self):
        base = {"s": scenario(["MEDIA-1"])}
        head = {"s": base["s"], "t": scenario(["GFX-1"])}
        failures = judge(base, head, {"s": result()}, {"s": result(False)},
                         changed_on_base={"t": result(False)}, changed_on_head={"t": result()})
        self.assertIn("passed on base, fails on head", failures[0])

    def test_weakening_a_scenario_claims_nothing(self):
        # The edited scenario passes on both builds, so it is no flip and the
        # base version still judges head.
        base = {"s": scenario(["GFX-1"])}
        head = {"s": scenario(["GFX-1"], digest="weaker")}
        failures = judge(base, head, {"s": result()}, {"s": result(False)},
                         changed_on_base={"s": result()}, changed_on_head={"s": result()})
        self.assertIn("passed on base, fails on head", failures[0])

    def test_rewriting_a_scenario_for_new_semantics_claims_it(self):
        base = {"s": scenario(["GFX-1"])}
        head = {"s": scenario(["GFX-1"], digest="new rule")}
        failures = judge(base, head, {"s": result()}, {"s": result(False)},
                         changed_on_base={"s": result(False)}, changed_on_head={"s": result()})
        self.assertEqual(failures, [])

    def test_a_claim_takes_the_spec_from_the_base_version(self):
        base = {"s": scenario(["GFX-1"]), "u": scenario(["MEDIA-1"])}
        head = {"s": scenario(["GFX-1", "MEDIA-1"], digest="retagged"), "u": base["u"]}
        failures = judge(base, head, {"s": result(), "u": result()},
                         {"s": result(False), "u": result(False)},
                         changed_on_base={"s": result(False)}, changed_on_head={"s": result()})
        self.assertEqual(len(failures), 1)
        self.assertIn("u: passed on base", failures[0])

    def test_a_removed_scenario_still_judges_head(self):
        base = {"s": scenario(["GFX-1"])}
        failures = judge(base, {}, {"s": result()}, {"s": result(False)})
        self.assertIn("passed on base, fails on head", failures[0])

    def test_steady_rates_never_rise_even_when_claimed(self):
        base = {"s": scenario(["A64-1"])}
        head = {"s": base["s"], "t": scenario(["A64-1"])}
        failures = judge(base, head, {"s": result(steady={"accesses": 0.0})},
                         {"s": result(steady={"accesses": 3.0})},
                         changed_on_base={"t": result(False)}, changed_on_head={"t": result()})
        self.assertIn("steady accesses rose", failures[0])

    def test_steady_rates_may_fall(self):
        base = {"s": scenario(["A64-1"])}
        self.assertEqual(judge(base, base, {"s": result(steady={"accesses": 3.0})},
                               {"s": result(steady={"accesses": 0.0})}), [])

    def test_levels_never_rise_beyond_their_slack(self):
        base = {"s": scenario(["A64-1"])}
        mib = 1 << 20
        self.assertEqual(judge(base, base, {"s": result(levels={"peak_rss": 335 * mib})},
                               {"s": result(levels={"peak_rss": 340 * mib})}), [])
        failures = judge(base, base, {"s": result(levels={"peak_rss": 335 * mib})},
                         {"s": result(levels={"peak_rss": 565 * mib})})
        self.assertIn("levels peak_rss rose", failures[0])

    def test_a_nondeterministic_scenario_cannot_enter(self):
        head = {"t": scenario(["GFX-1"])}
        failures = judge({}, head, {}, {}, changed_on_base={"t": result(False)},
                         changed_on_head={"t": result()}, again={"t": result(outputs={"out.bin": "c"})})
        self.assertIn("differ between two head runs", failures[0])

    def test_a_new_scenario_must_pass_on_head(self):
        head = {"t": scenario(["GFX-1"])}
        failures = judge({}, head, {}, {}, changed_on_base={"t": result(False)},
                         changed_on_head={"t": result(False)})
        self.assertIn("fails on head", failures[0])


if __name__ == "__main__":
    unittest.main()
