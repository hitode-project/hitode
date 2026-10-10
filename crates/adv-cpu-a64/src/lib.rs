//! AArch64 thin translator for the adv-cpu core.
//!
//! Skeleton only: the facade does not select this engine yet. The design and
//! milestones live in `docs/a64-workbook.md`. Encoders are checked against
//! NDK clang vectors (`tools/cpu/a64_vectors.py`), and translated code runs
//! under Unicorn arm64 in host tests, so most work needs no device.

pub mod encode;

#[cfg(test)]
mod emulator;
