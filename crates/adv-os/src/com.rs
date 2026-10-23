//! COM activation, task memory and Automation for the old Win32 stack.
pub use adv_abi::com::*;
use adv_machine::{Env, Outcome};
use std::collections::{BTreeMap, BTreeSet};
mod automation;
pub use automation::{
    AutomationStrings, MAX_BSTR_BYTES, alloc_bstr, copy_bstr, free_bstr, owns_bstr,
};

/// A personality's in-process class constructor. The output was cleared and
/// checked by COM activation; the constructor owns rollback if publication fails.
pub type ClassFactory = fn(&mut Env<'_>, Iid, u32, u32) -> Outcome;

/// Process assembly registers classes here without introducing a second object
/// owner. Constructors still allocate their payloads in the common `Objects`.
#[derive(Default)]
pub struct ClassFactories {
    classes: BTreeMap<Iid, ClassFactory>,
}

impl ClassFactories {
    pub fn register(&mut self, class: Iid, factory: ClassFactory) {
        self.classes.insert(class, factory);
    }

    pub fn get(&self, class: &Iid) -> Option<ClassFactory> {
        self.classes.get(class).copied()
    }
}

/// Live CoTaskMemAlloc blocks. Every COM front allocates task memory through
/// [`task_alloc`] so that a guest CoTaskMemFree accepts what any front returned.
#[derive(Default)]
pub struct TaskMemory {
    pub allocations: BTreeSet<u32>,
}

/// CoTaskMemAlloc: zero on failure. The process heap backs task memory.
pub fn task_alloc(env: &mut Env<'_>, size: u32) -> u32 {
    if size.checked_add(0xffff).is_none() {
        return 0;
    }
    let pointer = env
        .ext
        .get_mut::<crate::Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, size, false)
        .unwrap_or(0);
    if pointer != 0 {
        env.ext
            .get_or_default::<TaskMemory>()
            .allocations
            .insert(pointer);
    }
    pointer
}

/// CoTaskMemFree for a block this process allocated; false for a foreign pointer.
pub fn task_free(env: &mut Env<'_>, pointer: u32) -> bool {
    if !env
        .ext
        .get_or_default::<TaskMemory>()
        .allocations
        .remove(&pointer)
    {
        return false;
    }
    env.ext
        .get_mut::<crate::Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, pointer);
    true
}
