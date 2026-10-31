import unittest

from check_docs import check_guide

SOURCE = """pub struct Sys {
    pub direct_bodies: Map,
}
impl Translator<'_> {
    fn body(&mut self) {
        run_body(self);
    }
}
pub enum Op {
    Body(u8),
    Ret,
}
"""
FILES = {"a/lib.rs": SOURCE, "a/ops.rs": SOURCE, "docs/x.md": ""}


def guide(text: str, rules=("EX-A-1",), withdrawn=("EX-A-2",)) -> list[str]:
    return check_guide("G.md", text, FILES.get, set(rules), set(withdrawn), {"EX"})


class CheckGuideTest(unittest.TestCase):
    def test_defined_symbols_and_existing_files_pass(self) -> None:
        text = ("[`Sys::direct_bodies`](a/lib.rs) [`Translator::body`](a/ops.rs) "
                "[`Op::Body`](a/lib.rs) [`Op::Ret`](a/lib.rs) [`ops::body`](a/ops.rs) "
                "[docs](docs/x.md#part) [web](https://example.com) EX-A-1")
        self.assertEqual(guide(text), [])

    def test_a_missing_file_fails(self) -> None:
        self.assertIn("G.md: link to a/gone.rs, which does not exist", guide("[x](a/gone.rs)"))

    def test_a_renamed_symbol_fails(self) -> None:
        errors = guide("[`Translator::emit`](a/ops.rs)")
        self.assertEqual(errors, ["G.md: a/ops.rs does not define `emit` of `Translator::emit`"])

    def test_a_call_is_not_a_definition(self) -> None:
        self.assertTrue(guide("[`run_body`](a/ops.rs)"))

    def test_cited_rules_exist_and_are_not_withdrawn(self) -> None:
        self.assertEqual(guide("EX-A-2 and EX-B-9"), [
            "G.md: cites EX-A-2, which is withdrawn",
            "G.md: cites EX-B-9, which no spec defines",
        ])


if __name__ == "__main__":
    unittest.main()
