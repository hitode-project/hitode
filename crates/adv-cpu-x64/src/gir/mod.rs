//! GIR v1 semantics and validation, independent of any host backend.
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
