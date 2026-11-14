import unittest

from analyze_schedule import analyze


def row(time, event, context=0, a=0, b=0, c=0):
    return dict(time_ns=time * 1_000_000, event=event, context=context, a=a, b=b, c=c)


class ScheduleAnalysisTests(unittest.TestCase):
    def test_owner_work_ready_delay_and_handoff_have_distinct_costs(self):
        summary, _ = analyze([
            row(0, "mark"), row(0, "cs_take", 1, 16, 1),
            row(1, "cs_wait", 0, 16), row(1, "run_start", 1),
            row(4, "run_end", 1, 32, 3_000_000, 500_000),
            row(6, "run_start", 1), row(8, "cs_release", 1, 16, 0),
            row(9, "run_end", 1, 48, 3_000_000, 0),
            row(10, "cs_take", 0, 16, 1), row(11, "observe"),
            row(13, "lease", 1),  # Events after the window cannot extend totals.
        ])
        lock = summary["locks"][0]
        self.assertEqual((lock["wait_ms"], lock["to_release_ms"], lock["handoff_ms"]), (9, 7, 2))
        self.assertEqual(lock["owner_state_ms"], {"owner": 5, "ready": 2})
        self.assertEqual(summary["states_ms"][1]["ready"], 4)
        self.assertEqual(summary["inline_runs"]["ctx1_host_ms"], 0.5)

    def test_press_response_splits_polling_waits_from_blocked_waits(self):
        summary, _ = analyze([
            row(0, "mark"), row(10, "input", 0, 0x201),
            row(10, "run_start", 1), row(12, "run_end", 1, 0, 2_000_000),
            row(12, "poll_idle", 0, 1000, 1), row(13, "host_wait", 0, 1_000_000, 1_000_000),
            row(13, "run_start", 1), row(14, "run_end", 1, 0, 1_000_000),
            row(17, "host_wait", 0, 4_000_000, 3_000_000, 1), row(18, "frame", 1, 1),
            row(20, "observe"),
        ])
        press = summary["input_responses"][0]
        self.assertEqual((press["frames"], press["first_frame_ms"], press["longest_gap_ms"]), (1, 8, 8))
        budget = press["longest_gap_budget"]
        self.assertEqual((budget["run_ms"], budget["poll_wait_ms"], budget["blocked_wait_ms"]), (3, 1, 3))
        self.assertEqual(summary["host_waits"]["polling_count"], 1)

    def test_stolen_lock_starts_new_attempt_without_reusing_old_owner(self):
        summary, waits = analyze([
            row(0, "mark"), row(0, "cs_take", 1, 16, 1),
            row(1, "cs_wait", 0, 16), row(2, "cs_release", 1, 16, 0),
            row(3, "cs_take", 2, 16, 1), row(4, "cs_wait", 0, 16),
            row(7, "cs_release", 2, 16, 0), row(8, "cs_take", 0, 16, 1),
            row(9, "cs_wait", 3, 32), row(10, "observe"),
        ])
        self.assertEqual([(w["owner"], w["outcome"]) for w in waits], [(1, "retry"), (2, "acquired")])
        self.assertEqual(sum(lock["wait_ms"] for lock in summary["locks"]), 7)
        self.assertEqual(summary["incomplete_lock_waits"], 1)

    def test_incomplete_trace_cannot_support_complete_interval_claims(self):
        for rows in ([], [row(2, "mark"), row(1, "observe")], [row(0, "mark"), row(3, "truncated")]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                analyze(rows)

    def test_frame_gap_clips_runs_and_includes_crossing_wait(self):
        summary, _ = analyze([
            row(0, "mark"), row(0, "cs_take", 1, 16, 1),
            row(1, "cs_wait", 0, 16), row(1, "run_start", 1),
            row(2, "frame", a=10), row(5, "frame", a=11),
            row(6, "run_end", 1, 32, 5_000_000, 1_000_000),
            row(7, "cs_release", 1, 16, 0), row(8, "cs_take", 0, 16, 1),
            row(9, "frame", a=13), row(10, "observe"),
        ])
        gaps = summary["longest_frame_gaps"]
        self.assertEqual(len(gaps), 1)  # A missing frame cannot define a gap.
        self.assertEqual(gaps[0]["wall_ms"], 3)
        self.assertEqual(gaps[0]["inline_runs"][1], {"runs": 1, "wall_ms": 3, "host_ms": 0.6})
        self.assertEqual(gaps[0]["completed_lock_waits"][0]["overlap_ms"], 3)

    def test_frame_gaps_are_labelled_by_the_producer_state(self):
        summary, _ = analyze([
            row(0, "mark"), row(0, "cs_take", 1, 16, 1), row(0, "frame", a=1),
            # Lock wait: context 0 blocks on a lock context 1 holds.
            row(1, "cs_wait", 0, 16), row(1, "block", 0, 2), row(1, "run_start", 1),
            row(9, "run_end", 1, 32, 8_000_000, 0), row(9, "cs_release", 1, 16, 0),
            row(9, "wake", 0), row(9, "run_start", 0), row(9, "cs_take", 0, 16, 1),
            row(10, "frame", a=2),
            # Execution: context 0 computes most of the gap.
            row(16, "run_end", 0, 32, 6_000_000, 0), row(16, "run_start", 0),
            row(17, "frame", a=3),
            # Idle: sleeping and polling are pacing, not stalls.
            row(17, "block", 0, 1), row(30, "wake", 0), row(30, "yield", 0),
            row(39, "run_start", 0), row(40, "frame", a=4), row(41, "observe"),
        ])
        causes = summary["frame_gap_causes"]
        self.assertEqual({k: v["count"] for k, v in causes["by_cause"].items()},
                         {"lock_wait": 1, "execution": 1, "idle": 1})
        self.assertEqual(causes["non_idle_ms"]["max"], 10)
        longest = summary["longest_frame_gaps"]
        self.assertEqual([(g["wall_ms"], g["cause"]) for g in longest],
                         [(23, "idle"), (10, "lock_wait"), (7, "execution")])
        self.assertEqual(longest[1]["producer_state_ms"], {"lock_wait": 8, "execution": 1})

    def test_owner_frames_split_wall_by_layer_inside_the_window(self):
        rows = [
            row(0, "mark"), row(0, "compute_offer", 2, 0x400),
            row(1, "run_start", 1), row(5, "run_end", 1, 0, 4_000_000, 1_000_000),
            row(8, "host_wait", 0, 3_000_000, 3_000_000), row(8, "frame", 1, 1, 7_000_000, 16_000_000),
            row(9, "run_start", 1), row(11, "run_end", 1, 0, 2_000_000, 0),
            row(14, "compute_done", 2, 100, 3_000_000), row(14, "compute_opt", 2, 75),
            row(14, "compute_round", 0, 1, 3_000_000, 1),
            row(16, "frame", 1, 2, 5_000_000, 16_000_000), row(16, "observe"),
        ]
        frames = analyze(rows)[0]["owner_frame"]
        self.assertEqual(frames["wall_ms"], {"guest": 2.5, "host_wait": 1.5, "compute_rounds": 1.5,
                                             "other": 2, "host_services": 0.5})
        self.assertEqual((frames["owner_work_ms"], frames["period_ms"]), (6, 16))
        computation = analyze(rows)[0]["computation"]
        self.assertEqual((computation["retired"], computation["optimized_share"]), (100, 0.75))
        self.assertEqual(computation["entries"][0]["entry"], "0x400")
        # The last 8 ms hold the second frame only.
        frames = analyze(rows, 8_000_000)[0]["owner_frame"]
        self.assertEqual((frames["frames"], frames["wall_ms"]["guest"]), (1, 2))


if __name__ == "__main__":
    unittest.main()
