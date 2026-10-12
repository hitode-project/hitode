//! Execution layer facade: shared-state x86-32 interpreter, hot-region JIT,
//! and the guest CPU interface ([`backend`]) the machine layer drives. The
//! engine is chosen per target architecture; see the README for scope.
#[cfg(target_arch = "aarch64")]
pub use adv_cpu_a64::{Cpu as JitCpu, *};
#[cfg(not(target_arch = "aarch64"))]
pub use adv_cpu_x64::*;

/// The platform's execution engine.
pub type Cpu = JitCpu;
