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


if __name__ == "__main__":
    unittest.main()
