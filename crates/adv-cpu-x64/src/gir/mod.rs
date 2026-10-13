//! GIR v1 semantics and validation, independent of any host backend.
//!
//! These modules index values within one region and keep no Cranelift
//! `Value` or host pointer (EXEC-GIR-6). They describe sized values,
//! explicit edge arguments, flag definitions and preserved bits, the order
//! of memory side effects, commit points, pre-fault snapshots and code
//! dependencies. Their integer and control-flow coverage is wider than the
//! production SSA of `lift::tier2`, so a construct they cover is not
//! necessarily compiled as SSA. `eval` runs on `Memory` in software only to
//! compare with the interpreter. Every operation that may fail keeps its
//! pre-commit state; the recovery DAG has node and depth limits and rebuilds
//! the prefix only from captured values.
#![allow(dead_code)]

pub(crate) mod cfg;
pub(crate) mod diagnostics;
pub(crate) mod eval;
pub(crate) mod ir;
pub(crate) mod lift;
pub(crate) mod state_ssa;
pub(crate) mod verify;

#[cfg(test)]
mod semantic_tests;

// Keep the standalone recovery prototypes test-only.
#[cfg(all(
    test,
    any(
        all(windows, target_arch = "x86_64", target_env = "msvc"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
include!("prototype.rs");
pub(crate) mod recovery;
