//! GIR v1 semantics and validation, independent of any host backend.
#![allow(dead_code)]

pub(crate) mod cfg;
pub(crate) mod diagnostics;
pub(crate) mod eval;
pub(crate) mod hot;
pub(crate) mod ir;
pub(crate) mod lift;
pub(crate) mod state_ssa;
pub(crate) mod verify;

#[cfg(test)]
mod semantic_tests;

// Keep the original recovery experiments and measurement test paths intact.
#[cfg(all(
    test,
    any(
        all(windows, target_arch = "x86_64", target_env = "msvc"),
        all(
            any(target_os = "linux", target_os = "android"),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
include!("prototype.rs");
pub(crate) mod recovery;

#[cfg(any(test, feature = "gir-lower"))]
pub(crate) mod lower_cl;

#[cfg(any(test, feature = "gir-execute"))]
pub(crate) mod execution;
#[cfg(test)]
mod execution_tests;
