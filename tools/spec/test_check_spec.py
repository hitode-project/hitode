import unittest

from check_spec import check

HEAD = "# 示例\n\n编号前缀：EX\n\n"
GOOD = """### EX-A-1 示例规定

**规定**：示例必须成立。
**验证**：测试
"""


def spec(body: str, **extra: str) -> dict[str, str]:
    files = {"docs/spec/example.md": HEAD + body, "docs/spec/README.md": "目前 主机"}
    files.update(extra)
    return files


class CheckSpecTest(unittest.TestCase):
    def test_a_tested_rule_with_a_marker_passes(self) -> None:
        # The rules document mentions banned words and is never checked.
        self.assertEqual(check(spec(GOOD, **{"a.rs": "// spec: EX-A-1\n#[test]"})), [])

    def test_a_tested_rule_needs_a_marker(self) -> None:
        errors = check(spec(GOOD))
        self.assertTrue(any("no test has" in e for e in errors), errors)

    def test_review_needs_a_reason_and_rejects_markers(self) -> None:
        body = GOOD.replace("**验证**：测试", "**验证**：审查")
        errors = check(spec(body, **{"a.rs": "// spec: EX-A-1"}))
        self.assertTrue(any("needs ' — reason'" in e for e in errors), errors)
        self.assertTrue(any("mark it 测试" in e for e in errors), errors)

    def test_rules_need_a_modal_and_required_fields(self) -> None:
        errors = check(spec("### EX-A-1 示例\n\n**规定**：示例成立。\n"))
        self.assertTrue(any("has no 必须" in e for e in errors), errors)
        self.assertTrue(any("lacks 验证" in e for e in errors), errors)

    def test_ids_are_unique_and_follow_the_prefix(self) -> None:
        errors = check(spec(GOOD + "\n" + GOOD.replace("EX-A-1", "OTHER-A-1") + "\n" + GOOD,
                            **{"a.rs": "// spec: EX-A-1"}))
        self.assertTrue(any("already used" in e for e in errors), errors)
        self.assertTrue(any("does not match EX-GROUP-N" in e for e in errors), errors)

    def test_banned_words_dates_and_long_sentences(self) -> None:
        long = "长" * 81
        body = GOOD.replace("示例必须成立。", f"目前主机必须在 2026-01-01 成立。{long}必须。")
        errors = check(spec(body, **{"a.rs": "// spec: EX-A-1"}))
        for needle in ("'目前'", "宿主", "dates", "sentence over"):
            self.assertTrue(any(needle in e for e in errors), (needle, errors))

    def test_references_must_name_live_rules(self) -> None:
        body = GOOD + "\n已撤销：EX-A-2\n"
        files = spec(body, **{"a.rs": "// spec: EX-A-1", "b.md": "见 EX-A-3 与 EX-A-2"})
        errors = check(files)
        self.assertTrue(any("EX-A-3 names no rule" in e for e in errors), errors)
        self.assertTrue(any("EX-A-2 is withdrawn" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
