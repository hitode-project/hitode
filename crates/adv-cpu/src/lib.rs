//! Execution layer facade: shared-state x86-32 interpreter, hot-region JIT,
//! and the guest CPU interface ([`backend`]) the machine layer drives. The
//! engine is chosen per target architecture; see the README for scope.
pub use adv_cpu_x64::*;

#[cfg(all(feature = "engine-a64", target_arch = "aarch64"))]
pub use adv_cpu_a64::Cpu as JitCpu;

/// The platform's execution engine.
pub type Cpu = JitCpu;
