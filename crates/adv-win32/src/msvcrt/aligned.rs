//! Aligned CRT views keep the backing RTL pointer in the native x86 guest header.
//! No host allocation map or second heap owns these blocks. Resizing may move,
//! preserving the old allocation until the replacement and copy have succeeded.
use super::*;
use adv_abi::{GuestReturn, guest::memory_failure};
use adv_cpu::backend::MemFault;

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("_aligned_malloc", malloc),
            ("_aligned_offset_malloc", malloc),
            ("_aligned_free", free),
            ("_aligned_msize", size),
            ("_aligned_realloc", resize),
            ("_aligned_offset_realloc", resize),
            ("_aligned_recalloc", resize),
            ("_aligned_offset_recalloc", resize),
        ],
    );
}

#[derive(Clone, Copy)]
struct Layout {
    size: u32,
    alignment: u32,
    offset: u32,
    gap: u32,
    total: u32,
}

fn no_memory(env: &mut Env<'_>) -> Outcome {
    heap::result(env, 0).into_outcome()
}

fn layout(env: &mut Env<'_>, size: u32, alignment: u32, offset: u32) -> Result<Layout, Outcome> {
    let profile = versions::profile(&env.sys.thunks.get(env.thunk).module);
    if (alignment == 0 && !profile.zero_alignment)
        || alignment & alignment.wrapping_sub(1) != 0
        || (offset != 0 && offset >= size)
    {
        return Err(errors::invalid(env, 0));
    }
    let alignment = alignment.max(4);
    let gap = offset.wrapping_neg() & 3;
    let Some(total) = size
        .checked_add(alignment)
        .and_then(|n| n.checked_add(3 + gap))
        .filter(|&n| n <= 0xffffffe0)
    else {
        return Err(no_memory(env));
    };
    Ok(Layout {
        size,
        alignment,
        offset,
        gap,
        total,
    })
}

fn backing(env: &Env<'_>, pointer: u32) -> Result<u32, MemFault> {
    env.cpu.read_u32((pointer & !3).wrapping_sub(4))
}

fn allocate(env: &mut Env<'_>, layout: Layout, then: Done) -> Outcome {
    crate::native_heap::allocate_then(
        env,
        layout.total,
        false,
        Box::new(move |env, base| {
            if base == 0 {
                return no_memory(env);
            }
            // Use a wide intermediate: adding the offset during rounding can
            // exceed the guest address range although the final view cannot.
            let pointer = (((u64::from(base)
                + u64::from(layout.gap + layout.alignment + 3)
                + u64::from(layout.offset))
                & !u64::from(layout.alignment - 1))
                - u64::from(layout.offset)) as u32;
            if let Err(fault) = env.cpu.write_u32(pointer - layout.gap - 4, base) {
                return crate::native_heap::free_then(
                    env,
                    base,
                    Box::new(move |_, _| memory_failure(fault)),
                );
            }
            then(env, pointer)
        }),
    )
}

fn malloc(env: &mut Env<'_>) -> Outcome {
    let offset = if env.sys.thunks.get(env.thunk).name.contains("offset") {
        env.arg(2)
    } else {
        0
    };
    let layout = match layout(env, env.arg(0), env.arg(1), offset) {
        Ok(layout) => layout,
        Err(outcome) => return outcome,
    };
    allocate(env, layout, Box::new(|_, pointer| Outcome::Return(pointer)))
}

fn free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer == 0 {
        return Outcome::Return(0);
    }
    match backing(env, pointer) {
        Ok(base) => crate::native_heap::free(env, base),
        Err(fault) => memory_failure(fault),
    }
}

fn size(env: &mut Env<'_>) -> Outcome {
    let (pointer, alignment, offset) = (env.arg(0), env.arg(1), env.arg(2));
    if pointer == 0 {
        return errors::invalid(env, u32::MAX);
    }
    let base = match backing(env, pointer) {
        Ok(base) => base,
        Err(fault) => return memory_failure(fault),
    };
    // Native VC80/VC90/UCRT check the pointer alone here. The documented
    // precondition requires the original alignment/offset; no new view is made.
    let overhead = alignment
        .max(4)
        .wrapping_add(3 + (offset.wrapping_neg() & 3));
    heap::size_then(
        env,
        base,
        Box::new(move |_, total| Outcome::Return(total.wrapping_sub(overhead))),
    )
}

fn resize(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let zero = name.contains("recalloc");
    let pointer = env.arg(0);
    let (size, alignment, offset) = if zero {
        let Some(size) = env
            .arg(1)
            .checked_mul(env.arg(2))
            .filter(|&n| n <= 0xffffffe0)
        else {
            return no_memory(env);
        };
        (
            size,
            env.arg(3),
            if name.contains("offset") {
                env.arg(4)
            } else {
                0
            },
        )
    } else {
        (
            env.arg(1),
            env.arg(2),
            if name.contains("offset") {
                env.arg(3)
            } else {
                0
            },
        )
    };
    if pointer != 0 && size == 0 {
        return match backing(env, pointer) {
            Ok(base) => crate::native_heap::free(env, base),
            Err(fault) => memory_failure(fault),
        };
    }
    let layout = match layout(env, size, alignment, offset) {
        Ok(layout) => layout,
        Err(outcome) => return outcome,
    };
    if pointer == 0 {
        return allocate(
            env,
            layout,
            Box::new(move |env, pointer| {
                if zero && let Err(fault) = memory_ops::fill(env, pointer, 0, layout.size) {
                    return memory_failure(fault);
                }
                Outcome::Return(pointer)
            }),
        );
    }
    let base = match backing(env, pointer) {
        Ok(base) => base,
        Err(fault) => return memory_failure(fault),
    };
    heap::size_then(
        env,
        base,
        Box::new(move |env, total| {
            let Some(available) = pointer
                .checked_sub(base)
                .and_then(|prefix| total.checked_sub(prefix))
            else {
                return Outcome::Fail("CRT aligned resize: invalid heap backing extent".into());
            };
            // Realloc preserves every byte available after the old view. Recalloc
            // determines its zeroing boundary with the native aligned-size formula.
            let old_size = total.wrapping_sub(layout.alignment + layout.gap + 3);
            allocate(
                env,
                layout,
                Box::new(move |env, replacement| {
                    let copied =
                        memory_ops::copy(env, replacement, pointer, available.min(layout.size));
                    let updated = copied.and_then(|_| {
                        if zero && old_size < layout.size {
                            memory_ops::fill(
                                env,
                                replacement + old_size,
                                0,
                                layout.size - old_size,
                            )?;
                        }
                        Ok(())
                    });
                    if let Err(fault) = updated {
                        let new_base = match backing(env, replacement) {
                            Ok(base) => base,
                            Err(error) => return memory_failure(error),
                        };
                        return crate::native_heap::free_then(
                            env,
                            new_base,
                            Box::new(move |_, _| memory_failure(fault)),
                        );
                    }
                    crate::native_heap::free_then(
                        env,
                        base,
                        Box::new(move |_, _| Outcome::Return(replacement)),
                    )
                }),
            )
        }),
    )
}
