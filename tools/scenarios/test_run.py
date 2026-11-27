"""The scenario gate's judgment, on recorded results."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402
import work  # noqa: E402


def scenario(spec, digest="d", min_counters=None, steady_min=None):
    data = {"spec": spec, "digest": digest}
    if min_counters is not None:
        data["min"] = min_counters
    if steady_min is not None:
        data["steady"] = {"min": steady_min}
    return data


def result(passed=True, outputs=None, steady=None, levels=None, min_counters=None, steady_min=None):
    return {"passed": passed, "exit": 0 if passed else 64, "stop": "guest_exit",
            "outputs": outputs or {"out.bin": "a"}, "steady": steady or {}, "levels": levels or {},
            "min": min_counters or {}, "steady_min": steady_min or {}}


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

    def test_negative_steady_baseline_clamps_to_zero(self):
        base = {"s": scenario(["A64-1"])}
        self.assertEqual(judge(base, base, {"s": result(steady={"helper_exits": -0.125})},
                               {"s": result(steady={"helper_exits": 0.0})}), [])
        failures = judge(base, base, {"s": result(steady={"helper_exits": -0.125})},
                         {"s": result(steady={"helper_exits": 0.125})})
        self.assertIn("steady helper_exits rose", failures[0])

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

    def test_unstable_steady_or_level_counters_cannot_enter_or_claim(self):
        base = {"s": scenario(["A64-1"])}
        head = {"s": base["s"], "t": scenario(["A64-1"])}
        failures = judge(
            base, head,
            {"s": result()}, {"s": result(False)},
            changed_on_base={"t": result(False)},
            changed_on_head={"t": result(steady={"owner_retired": 200.0})},
            again={"t": result(steady={"owner_retired": 260.0})},
        )
        self.assertEqual(len(failures), 2)
        self.assertIn("steady owner_retired differs between two head runs", failures[0])
        self.assertIn("s: passed on base", failures[1])

        failures = judge(
            {}, {"t": scenario(["A64-1"])}, {}, {},
            changed_on_base={"t": result(False)},
            changed_on_head={"t": result(levels={"peak_heap_bytes": 100.0})},
            again={"t": result(levels={"peak_heap_bytes": 120.0})},
        )
        self.assertIn("levels peak_heap_bytes differs between two head runs", failures[0])

    def test_a_new_scenario_must_pass_on_head(self):
        head = {"t": scenario(["GFX-1"])}
        failures = judge({}, head, {}, {}, changed_on_base={"t": result(False)},
                         changed_on_head={"t": result(False)})
        self.assertIn("fails on head", failures[0])

    def test_floor_invariants_must_hold_on_head(self):
        base = {"s": scenario(["A64-1"], min_counters={"split_proofs": 1}, steady_min={"splits": 0.5})}
        self.assertEqual(
            judge(
                base, base,
                {"s": result(min_counters={"split_proofs": 1}, steady_min={"splits": 1.0})},
                {"s": result(min_counters={"split_proofs": 1}, steady_min={"splits": 0.75})},
            ),
            [],
        )
        failures = judge(
            base, base,
            {"s": result(min_counters={"split_proofs": 1}, steady_min={"splits": 1.0})},
            {"s": result(min_counters={"split_proofs": 0}, steady_min={"splits": 0.25})},
        )
        self.assertEqual(len(failures), 2)
        self.assertIn("split_proofs fell below 1.000", failures[0])
        self.assertIn("steady splits fell below 0.500", failures[1])

    def test_new_scenario_floor_must_hold_on_both_head_runs(self):
        head = {"t": scenario(["A64-1"], steady_min={"splits": 0.5})}
        failures = judge(
            {}, head, {}, {},
            changed_on_base={"t": result(False)},
            changed_on_head={"t": result(steady_min={"splits": 1.0})},
            again={"t": result(steady_min={"splits": 0.0})},
        )
        self.assertIn("steady splits fell below 0.500", failures[0])
        self.assertIn("second head run", failures[0])

    def test_counters_parses_fp_diagnostics_fields(self):
        report = {
            "a64_diagnostics": (
                "Diagnostics { first_visits: 1, split: SplitCounts { splits: 2 }, "
                "fp: FpDiagnostics { fpcr_writes: 4, fpsr_reads: 7, fpsr_folds: 3, "
                "fpsr_isolations: 1, fp_speculation_failures: 5, fp_speculation_replays: 4, "
                "fp_replayed_instructions: 18, fp_validation_failures: 2, fp_versions: 6, "
                "fp_version_misses: 9, fp_version_joins: 1, fp_opt_spills: 3, "
                "fp_opt_vector_spills: 2, fp_opt_spill_cells: 4, fp_fast_completions: 12, "
                "fp_vector_strips: 10 } } "
                "tier1 versions region_speculation 0x00401000=[failures 5, replays 4, "
                "replayed_instructions 18, validation_failures 2]"
            ),
            "compilations": 3,
        }
        got = run.counters(report)
        expected = {
            "fpcr_writes": 4,
            "fpsr_reads": 7,
            "fpsr_folds": 3,
            "fpsr_isolations": 1,
            "fp_speculation_failures": 5,
            "fp_speculation_replays": 4,
            "fp_replayed_instructions": 18,
            "fp_validation_failures": 2,
            "fp_versions": 6,
            "fp_version_misses": 9,
            "fp_version_joins": 1,
            "fp_opt_spills": 3,
            "fp_opt_vector_spills": 2,
            "fp_opt_spill_cells": 4,
            "fp_fast_completions": 12,
            "fp_vector_strips": 10,
            "compilations": 3,
        }
        for key, value in expected.items():
            self.assertEqual(got.get(key), value, key)


class Work(unittest.TestCase):
    def test_site_counts_exclude_addresses_outside_the_kernel(self):
        counts = work.site_counts(
            " access 0x00401000=3 access 0x7b080000=6"
            " interpreter 0x00401004 HostFault=1 interpreter 0x00401008 OperandFault=2",
            [(0x401000, 0x401010)],
        )
        self.assertEqual(counts, {"accesses": 3, "faults": 3})

    def test_split_choice_uses_pace_and_helper_availability(self):
        self.assertEqual(work.split_choice(10000, 25, 100.0, 20000, 0, 0, 0, 2000000, 2),
                         (0, 0))
        self.assertEqual(work.split_choice(10000, 25, 100.0, 20000, 0, 0, 0, 900000, 2),
                         (1, 1250))
        self.assertEqual(work.split_choice(10000, 25, 100.0, 20000, 0, 0, 0, 100000, 2),
                         (2, 834))
        self.assertEqual(work.split_choice(10000, 25, 100.0, 20000, 0, 0, 0, 100000, 0),
                         (0, 0))

    def test_resident_trips_falls_back_to_backedge_without_tier1_counts(self):
        self.assertEqual(work.resident_trips(1440, 2, 1375, None), 1375)
        self.assertEqual(work.resident_trips(1440, 2, 0, None), 0)

    def test_resident_trips_prices_backedge_entries(self):
        # 65 warmup calls ran both rows in Tier-1 (130), 1375 calls ran Row 0 in Tier-1.
        self.assertEqual(work.resident_trips(1440, 2, 1375, 1505), 1375)

    def test_resident_trips_prices_direct_entries(self):
        # 65 warmup calls ran both rows in Tier-1 (128), 1375 calls entered resident directly.
        self.assertEqual(work.resident_trips(1440, 2, 1375, 128), 2750)
        # Steady delta across 8 iterations (1440 -> 2880 calls) credits all 2880 rows.
        low = work.resident_trips(1440, 2, 1375, 128)
        high = work.resident_trips(2880, 2, 2815, 128)
        self.assertEqual(high - low, 2880)

    def test_resident_trips_prices_mixed_direct_and_backedge_entries(self):
        # 65 warmup calls (130) + 375 backedge calls (375) + 1000 direct calls (0).
        self.assertEqual(work.resident_trips(1440, 2, 1375, 505), 1000 * 2 + 375)


if __name__ == "__main__":
    unittest.main()
