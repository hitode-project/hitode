import json
from pathlib import Path
import tempfile
import unittest

from matrix_gate import gate, load, main, outcome


def result(sample="one", status="frames_observed", kind=None, failure=None, frames=100):
    return {"id": sample, "status": status, "failure_kind": kind, "failure": failure,
            "frames": frames, "host_calls": 1000}


SEH = ("blocked", "seh_dispatch", "context 0 at 0x00574916: nested frame-based exception dispatch")
WILD = ("blocked", "cpu_access_violation", "context 0 at 0x7FFE0010: memory fault\nmore")


class GateTests(unittest.TestCase):
    def test_new_shader_diagnostics_regress_even_when_frames_continue(self):
        candidate = result()
        candidate["api"] = {"warnings": [{"api": "IDirect3DDevice9::DrawPrimitive",
                                         "reason": "programmable declaration unavailable"}]}
        verdict = gate({"one": [result()]}, {"one": [candidate]}, 0.25)["one"]
        self.assertEqual(verdict["verdict"], "regressed")
        self.assertIn("new shader/declaration warning", verdict["reasons"][0])
        self.assertEqual(gate({"one": [candidate]}, {"one": [candidate]}, 0.25)["one"]["verdict"], "same")

    def test_outcome_names_the_failure_address_of_cpu_failures_only(self):
        self.assertEqual(outcome(result(status=SEH[0], kind=SEH[1], failure=SEH[2])),
                         ("blocked", "seh_dispatch", "0x00574916"))
        self.assertEqual(outcome(result(status="blocked", kind="called_stub",
                                        failure="unimplemented: a.dll!F at 0x10")),
                         ("blocked", "called_stub", "-"))
        self.assertEqual(outcome(result()), ("frames_observed", "-", "-"))

    def test_outcomes_seen_in_any_baseline_run_do_not_regress(self):
        baseline = {"one": [result(status=SEH[0], kind=SEH[1], failure=SEH[2]),
                            result(status="early_exit", kind="guest_exit_unreviewed")]}
        candidate = {"one": [result(status="early_exit", kind="guest_exit_unreviewed")]}
        verdict = gate(baseline, candidate, 0.25)["one"]
        self.assertEqual(verdict["verdict"], "changed")
        self.assertEqual(verdict["reasons"], [])
        self.assertEqual(verdict["gone"], ["blocked/seh_dispatch/0x00574916"])

    def test_a_new_failure_address_regresses(self):
        baseline = {"one": [result(status=SEH[0], kind=SEH[1], failure=SEH[2])] * 3}
        candidate = {"one": [result(status=SEH[0], kind=SEH[1], failure=SEH[2]),
                             result(status=WILD[0], kind=WILD[1], failure=WILD[2])]}
        verdict = gate(baseline, candidate, 0.25)["one"]
        self.assertEqual(verdict["verdict"], "regressed")
        self.assertEqual(verdict["reasons"], ["new blocked/cpu_access_violation at 0x7ffe0010"])

    def test_frames_regress_only_below_the_worst_baseline_run(self):
        baseline = {"one": [result(frames=100), result(frames=60)]}
        self.assertEqual(gate(baseline, {"one": [result(frames=50)]}, 0.25)["one"]["verdict"], "same")
        self.assertEqual(gate(baseline, {"one": [result(frames=40), result(frames=44)]}, 0.25)["one"]["reasons"],
                         ["frames 44 < 60"])

    def test_a_missing_candidate_sample_regresses(self):
        verdicts = gate({"one": [result()]}, {"two": [result("two")]}, 0.25)
        self.assertEqual(verdicts["one"]["reasons"], ["no candidate run"])
        self.assertEqual(verdicts["two"]["verdict"], "changed")

    def test_main_reads_run_directories_and_fails_on_regression(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def write(name, samples):
                (root / name).mkdir()
                (root / name / "results.json").write_text(json.dumps({"samples": samples}), encoding="utf-8")
                return str(root / name)
            a = write("a", [result()])
            b = write("b", [result(frames=90)])
            c = write("c", [result(status=WILD[0], kind=WILD[1], failure=WILD[2], frames=0)])
            self.assertEqual(len(load([Path(a), Path(b)])["one"]), 2)
            self.assertEqual(main(["--baseline", a, "--candidate", b]), 0)
            self.assertEqual(main(["--baseline", a, b, "--candidate", c, "--json", str(root / "v.json")]), 1)
            self.assertEqual(json.loads((root / "v.json").read_text(encoding="utf-8"))["one"]["verdict"], "regressed")


if __name__ == "__main__":
    unittest.main()
