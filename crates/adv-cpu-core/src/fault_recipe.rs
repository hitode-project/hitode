//! Cold-only scalar recovery. No guest memory is read or instruction replayed.
/// Architectural register access independent of the operating system context.
/// Implementations read a frozen snapshot, never guest memory.
pub trait HostRegisters {
    fn gpr(&self, register: u8) -> Option<u64>;
}
