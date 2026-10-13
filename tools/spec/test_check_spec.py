import unittest

from check_spec import check

HEAD = "# Example\n\nID prefix: EX\n\n"
GOOD = """### EX-A-1 Example rule

**Rule**: The example MUST hold.
**Verification**: Test
"""


def spec(body: str, **extra: str) -> dict[str, str]:
    # The rules document quotes banned words and is never checked.
    files = {"docs/spec/example.md": HEAD + body, "docs/spec/README.md": "currently client"}
    files.update(extra)
    return files


class CheckSpecTest(unittest.TestCase):
    def test_a_tested_rule_with_a_marker_passes(self) -> None:
        self.assertEqual(check(spec(GOOD, **{"a.rs": "// spec: EX-A-1\n#[test]"})), [])

    def test_a_tested_rule_needs_a_marker(self) -> None:
        errors = check(spec(GOOD))
        self.assertTrue(any("no test has" in e for e in errors), errors)

    def test_review_needs_a_reason_and_rejects_markers(self) -> None:
        body = GOOD.replace("**Verification**: Test", "**Verification**: Review")
        errors = check(spec(body, **{"a.rs": "// spec: EX-A-1"}))
        self.assertTrue(any("needs ' — reason'" in e for e in errors), errors)
        self.assertTrue(any("mark it Test" in e for e in errors), errors)

    def test_rules_need_a_keyword_and_required_fields(self) -> None:
        errors = check(spec("### EX-A-1 Example\n\n**Rule**: The example holds.\n"))
        self.assertTrue(any("has no MUST" in e for e in errors), errors)
        self.assertTrue(any("lacks Verification" in e for e in errors), errors)

    def test_ids_are_unique_and_follow_the_prefix(self) -> None:
        errors = check(spec(GOOD + "\n" + GOOD.replace("EX-A-1", "OTHER-A-1") + "\n" + GOOD,
                            **{"a.rs": "// spec: EX-A-1"}))
        self.assertTrue(any("already used" in e for e in errors), errors)
        self.assertTrue(any("does not match EX-GROUP-N" in e for e in errors), errors)

    def test_banned_phrases_terms_dates_and_long_sentences(self) -> None:
        long = " ".join(["word"] * 26)
        body = GOOD.replace("The example MUST hold.",
                            f"The client currently MUST hold on 2026-01-01. {long} MUST.")
        errors = check(spec(body, **{"a.rs": "// spec: EX-A-1"}))
        for needle in ("'currently'", "'guest'", "dates", "over 25 words"):
            self.assertTrue(any(needle in e for e in errors), (needle, errors))

    def test_lowercase_keywords_are_ambiguous(self) -> None:
        body = GOOD.replace("The example MUST hold.", "The example must hold and MAY wait.")
        errors = check(spec(body, **{"a.rs": "// spec: EX-A-1"}))
        self.assertTrue(any("'must'" in e for e in errors), errors)

    def test_references_must_name_live_rules(self) -> None:
        body = GOOD + "\nWithdrawn: EX-A-2\n"
        files = spec(body, **{"a.rs": "// spec: EX-A-1", "b.md": "see EX-A-3 and EX-A-2"})
        errors = check(files)
        self.assertTrue(any("EX-A-3 names no rule" in e for e in errors), errors)
        self.assertTrue(any("EX-A-2 is withdrawn" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
