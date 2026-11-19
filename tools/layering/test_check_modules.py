import unittest

from check_modules import violations


class ModuleDependenciesTest(unittest.TestCase):
    # spec: A64A-DEP-1
    def test_grouped_imports_follow_target_roles(self):
        sources = {
            "asm.rs": "",
            "native/store.rs": "",
            "translate/front/decode.rs": "use iced_x86::Instruction;",
            "translate/front/trace.rs": "use super::decode::Opcode;",
            "translate/mod.rs": "use self::front::{decode::Opcode, trace::Trace};",
            "facts.rs": "use crate::{asm::Asm, native::{store::CodeStore as Store}};",
        }
        self.assertEqual(violations(sources, set()), [
            "facts.rs:1: facts -> native::store: disallowed",
        ])

    # spec: A64A-DEP-1
    def test_optimized_and_tier1_lowerings_cannot_use_each_other(self):
        self.assertEqual(violations({
            "translate/mod.rs": "use self::opt::analyze;",
            "translate/opt/mod.rs": "use super::Translator;",
        }, set()), [
            "translate/mod.rs:1: translate::jit -> translate::opt: disallowed",
            "translate/opt/mod.rs:1: translate::opt -> translate::jit: disallowed",
        ])

    # spec: A64A-DEP-1
    def test_parent_globs_and_test_scopes(self):
        sources = {
            "lib.rs": "#[cfg(test)] mod helpers;",
            "helpers.rs": "use super::*;",
            "translate/mod.rs": "",
            "translate/ops.rs": """
#[cfg(all(test, native))] mod differential { use super::*; }
#[test] fn unit_test() { use super::*; }
#[cfg(any(test, native))] use crate::translate::{*};
use crate::encode::*;
""",
            "encode.rs": "",
        }
        self.assertEqual(violations(sources, set()), [
            "translate/ops.rs:4: translate::jit -> parent glob crate::translate::*: disallowed",
        ])

    # spec: A64A-DEP-1
    def test_external_uses_include_qualified_paths_and_grouped_imports(self):
        self.assertEqual(violations({
            "executor.rs": "use adv_cpu_core::interpreter::Interpreter;",
            "native/offload.rs": "use adv_cpu_core::interpreter as interp;",
            "translate/simd.rs": """
use adv_cpu_core::{Memory, interpreter::Step};
fn lower(insn: ::iced_x86::Instruction) {}
""",
            "translate/front/decode.rs": "fn decode() { iced_x86::Decoder::new(); }",
        }, set()), [
            "translate/simd.rs:2: translate::jit -> adv_cpu_core::interpreter: disallowed",
            "translate/simd.rs:3: translate::jit -> iced_x86: disallowed",
        ])

    # spec: A64A-DEP-1
    def test_comments_and_literals_are_not_imports(self):
        self.assertEqual(violations({
            "executor.rs": "",
            "cache.rs": '''
/* " /* nested comment */ */
use crate::executor::Executor;
// use iced_x86::Code;
const TEXT: &str = r#"use super::*; iced_x86::Decoder"#;
''',
        }, set()), ["cache.rs:3: cache -> executor: disallowed"])

    # spec: A64A-DEP-1
    def test_transitional_edges_expire_and_do_not_cover_other_files(self):
        transitional = {("native/plan.rs", "iced_x86")}
        sources = {
            "native/plan.rs": "use iced_x86::Code;",
            "native/run.rs": "use iced_x86::Code;",
        }
        self.assertEqual(violations(sources, transitional), [
            "native/run.rs:1: native::run -> iced_x86: disallowed",
        ])
        sources["native/plan.rs"] = "#[cfg(test)] mod tests { use iced_x86::code_asm::*; }"
        self.assertEqual(violations(sources, transitional), [
            "native/plan.rs -> iced_x86: transitional edge is gone; drop it",
            "native/plan.rs:1: native::plan -> iced_x86 (tests): disallowed",
            "native/run.rs:1: native::run -> iced_x86: disallowed",
        ])


if __name__ == "__main__":
    unittest.main()
