//! Host replacements for guest functions.
//!
//! [`Machine::detour`] overwrites a guest function's entry with `jmp thunk`, so calls to it
//! become host calls exactly like an imported function (the thunk's `ret` returns to the
//! caller). The overwritten prologue is kept in a trampoline, followed by a jump back into
//! the function, so the handler can still run the original code with
//! [`Outcome::call`](crate::Outcome::call) when it cannot (or must not) handle a call itself.

use pal_cpu::asm::Asm;
use pal_cpu::{Backend, MemFault, Prot};

use crate::Machine;
use crate::layout::{PAGE, THUNK_BASE, THUNK_STRIDE};

/// Length of the `jmp rel32` written over the entry.
pub const JMP_LEN: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detour {
    /// The guest function's entry.
    pub entry: u32,
    /// Entry of the original code: the saved prologue, then a jump to `entry + prologue`.
    pub original: u32,
    /// Index of the host function's thunk ([`crate::Env::thunk`] in its handler).
    pub thunk: u32,
}

impl<B: Backend> Machine<B> {
    /// Redirect guest function `entry` to the registered host function `module!name`.
    ///
    /// `prologue` is the number of bytes at `entry` that move to the trampoline. They must be
    /// whole instructions, at least [`JMP_LEN`] bytes, with no relative operands and no branch
    /// target inside them; callers only detour functions matched byte for byte, so this is a
    /// property of the matched code.
    pub fn detour(
        &mut self,
        entry: u32,
        prologue: u32,
        module: &str,
        name: &str,
    ) -> Result<Detour, MemFault> {
        assert!(
            (JMP_LEN..=32).contains(&prologue),
            "detour prologue of {prologue} bytes"
        );
        let thunk = self.sys.thunks.resolve(&mut self.cpu, module, name)?;
        let mut saved = vec![0u8; prologue as usize];
        self.cpu.read(entry, &mut saved)?;

        let original = self.sys.vm.alloc(&mut self.cpu, PAGE, Prot::RX)?;
        let mut t = Asm::new(original);
        t.bytes.extend_from_slice(&saved);
        t.jmp(entry + prologue);
        self.cpu.write(original, &t.bytes)?;

        let mut j = Asm::new(entry);
        j.jmp(thunk);
        // Fill the rest of the moved prologue so a disassembly of the patched code stays sane.
        j.bytes.resize(prologue as usize, 0xcc);
        self.cpu.write(entry, &j.bytes)?;
        self.cpu.invalidate_code(entry, prologue);
        Ok(Detour {
            entry,
            original,
            thunk: (thunk - THUNK_BASE) / THUNK_STRIDE,
        })
    }
}
