//! In-place qsort with real two-argument guest comparator callbacks. Heap sort
//! bounds comparisons to O(n log n), including an inconsistent comparator;
//! callback order and equal-element order are not an MSVC bit-identity promise.
//! Per-call continuation state owns only bounded two-element swap scratch.
use super::{Env, Outcome, Registry, invalid_parameter_result};
use adv_abi::{
    Api, GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const MAX_COUNT: u32 = 1 << 20;
const MAX_BYTES: u32 = 64 << 20;
const MAX_WIDTH: u32 = 1 << 20;

fn unsupported(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

struct Sort {
    base: u32,
    width: u32,
    compare: u32,
    build: u32,
    end: u32,
    root: u32,
    sifting: bool,
    scratch: Vec<u8>,
}

impl Sort {
    fn address(&self, index: u32) -> u32 {
        // The initial count*width and inclusive range-end checks prove every
        // index used by heap construction/extraction fits the guest address.
        self.base + index * self.width
    }
    fn swap(&mut self, env: &mut Env<'_>, left: u32, right: u32) -> Result<(), MemFault> {
        let (left, right) = (self.address(left), self.address(right));
        env.cpu.quiesce_memory();
        // A callback may have changed mappings/protection. Guard both complete
        // elements anew, then read both before writing either; no stale element
        // content is retained across a guest callback or wait.
        check_range(env.cpu, left, self.width, Prot::RW)?;
        check_range(env.cpu, right, self.width, Prot::RW)?;
        let (a, b) = self.scratch.split_at_mut(self.width as usize);
        env.cpu.read(left, a)?;
        env.cpu.read(right, b)?;
        env.cpu.write(left, b)?;
        env.cpu.write(right, a)
    }
    fn compare(
        self,
        env: &mut Env<'_>,
        left: u32,
        right: u32,
        then: impl FnOnce(&mut Env<'_>, Self, i32) -> Outcome + 'static,
    ) -> Outcome {
        let (left, right) = (self.address(left), self.address(right));
        for (address, length, protection) in [
            (left, self.width, Prot::READ),
            (right, self.width, Prot::READ),
            (self.compare, 1, Prot::EXEC),
        ] {
            if let Err(error) = check_range(env.cpu, address, length, protection) {
                return memory_failure(error);
            }
        }
        // Outcome::call installs the ordinary cdecl argument order on the
        // guest stack and restores that stack after the callback's plain RET.
        Outcome::call(self.compare, [left, right], move |env, result| {
            then(env, self, result.value() as i32)
        })
    }
    fn compare_parent(self, env: &mut Env<'_>, child: u32) -> Outcome {
        let root = self.root;
        self.compare(env, root, child, move |env, mut sort, result| {
            if result < 0 {
                if let Err(error) = sort.swap(env, root, child) {
                    return memory_failure(error);
                }
                sort.root = child;
            } else {
                sort.sifting = false;
            }
            sort.advance(env)
        })
    }
    fn advance(mut self, env: &mut Env<'_>) -> Outcome {
        loop {
            if !self.sifting {
                if self.build != 0 {
                    self.build -= 1;
                    self.root = self.build;
                } else {
                    if self.end <= 1 {
                        return Outcome::Return(0);
                    }
                    self.end -= 1;
                    if let Err(error) = self.swap(env, 0, self.end) {
                        return memory_failure(error);
                    }
                    self.root = 0;
                }
                self.sifting = true;
            }
            let child = self.root * 2 + 1;
            if child >= self.end {
                self.sifting = false;
                continue;
            }
            if child + 1 < self.end {
                return self.compare(env, child, child + 1, move |env, sort, result| {
                    sort.compare_parent(env, if result < 0 { child + 1 } else { child })
                });
            }
            return self.compare_parent(env, child);
        }
    }
}

#[win32(msvcrt, name = "qsort", level = implement)]
fn sort(
    env: &mut Env<'_>,
    base: GuestPtr<u8>,
    number: u32,
    width: u32,
    compare: GuestPtr<u8>,
) -> Outcome {
    // VC80/90 validate width/comparator even for zero/one elements. NULL base
    // is legal only when number==0. Returning handlers observe shared errno22;
    // DOS errno stays unchanged, and normal sorts preserve all error owners.
    if width == 0 || compare.is_null() || (base.is_null() && number != 0) {
        if env.sys.thunks.get(env.thunk).module == "msvcrt.dll" {
            return unsupported(
                env,
                "legacy MSVCRT invalid qsort parameter handling is unavailable",
            );
        }
        crate::util::set_last_error(env, 0);
        return invalid_parameter_result(env, "qsort", 22, 0);
    }
    if number < 2 {
        return Outcome::Return(0);
    }
    let Some(bytes) = number.checked_mul(width) else {
        return unsupported(env, "qsort count*width exceeds the 32-bit address range");
    };
    if base.addr().checked_add(bytes - 1).is_none() {
        return unsupported(env, "qsort array range overflows the 32-bit address space");
    }
    if number > MAX_COUNT || bytes > MAX_BYTES || width > MAX_WIDTH {
        return unsupported(
            env,
            "qsort exceeds 1048576 elements, 64 MiB array, or 1 MiB element limit",
        );
    }
    if let Err(error) = check_range(env.cpu, base.addr(), bytes, Prot::RW) {
        return memory_failure(error);
    }
    let mut scratch = Vec::new();
    if scratch.try_reserve_exact(width as usize * 2).is_err() {
        return unsupported(env, "qsort two-element scratch allocation is unavailable");
    }
    scratch.resize(width as usize * 2, 0);
    Sort {
        base: base.addr(),
        width,
        compare: compare.addr(),
        build: number / 2,
        end: number,
        root: 0,
        sifting: false,
        scratch,
    }
    .advance(env)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api { module, ..sort_ABI });
    }
}

#[cfg(test)]
mod tests;
