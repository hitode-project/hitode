//! AArch64 execution facade: the x86-32 interpreter, translator and guest CPU
//! interface ([`backend`]) the machine layer drives.
pub use adv_cpu_a64::{Cpu as JitCpu, *};

/// The AArch64 execution engine.
pub type Cpu = JitCpu;
