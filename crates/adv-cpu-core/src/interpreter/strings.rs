//! Restartable string execution with bounded, page-local copy/fill batches.
use super::*;

/// A batch cannot fault after validation or touch code. PCM destinations keep
/// byte-atomic writes. Limit it at page and address-size boundaries; the scalar path
/// handles crossing elements and preserves their exact fault priority.
fn batch(state: &mut CpuState, memory: &mut Memory, op: StringOp, budget: u32) -> u32 {
    if !matches!(op.kind, StringKind::Movs | StringKind::Stos) {
        return 0;
    }
    let width = u32::from(op.width);
    let index_mask = if op.word { 0xffff } else { u32::MAX };
    let reverse = state.eflags & (1 << 10) != 0;
    let source_index = state.regs[6] & index_mask;
    let destination = state.regs[7] & index_mask;
    let source = source_index.wrapping_add(if op.fs { state.fs_base } else { 0 });
    let available = |address: u32, index: u32| {
        let offset = address % PAGE_SIZE as u32;
        if offset + width > PAGE_SIZE as u32 {
            return 0;
        }
        if reverse {
            u64::from(offset / width + 1).min(u64::from(index) / u64::from(width) + 1) as u32
        } else {
            u64::from((PAGE_SIZE as u32 - offset) / width)
                .min((u64::from(index_mask) + 1 - u64::from(index)) / u64::from(width))
                as u32
        }
    };
    let mut count = (state.regs[1] & index_mask)
        .min(budget)
        .min(available(destination, destination));
    if op.kind == StringKind::Movs {
        count = count.min(available(source, source_index));
    }
    if count < 2 {
        return 0;
    }
    let back = if reverse { (count - 1) * width } else { 0 };
    let destination = destination.wrapping_sub(back);
    let source = source.wrapping_sub(back);
    let len = (count * width) as usize;
    let dst = memory.direct(destination, len, true);
    if dst.is_none() {
        // A forward copy from ordinary RAM to PCM can validate once per page.
        // It still publishes through Memory's byte-atomic audio stores.
        if reverse
            || op.kind != StringKind::Movs
            || memory.direct(source, len, false).is_none()
            || !memory.writable_audio(destination, len)
        {
            return 0;
        }
        memory
            .copy(source, destination, len)
            .expect("validated ordinary-to-PCM copy");
    } else if op.kind == StringKind::Movs {
        let Some(src) = memory.direct(source, len, false) else {
            return 0;
        };
        // Overlapping REP copies propagate each preceding element. A memcpy
        // or memmove snapshot would change this, including distinct file views.
        if u64::from(source) < u64::from(destination) + len as u64
            && u64::from(destination) < u64::from(source) + len as u64
            || memory.rep_alias_hazard(source, destination, len)
        {
            return 0;
        }
        // SAFETY: direct() validated both complete ordinary ranges, and the
        // overlap/alias checks exclude physically overlapping storage. The
        // exclusive Memory borrow keeps mappings and permissions unchanged.
        unsafe { std::ptr::copy_nonoverlapping(src, dst.unwrap(), len) };
    } else {
        let dst = dst.unwrap();
        let value = state.regs[0];
        // SAFETY: the entire ordinary writable page range was validated above;
        // each unaligned element is inside it, and no shared/code page qualifies.
        unsafe {
            match width {
                1 => std::ptr::write_bytes(dst, value as u8, len),
                2 => {
                    for n in 0..count as usize {
                        dst.add(n * 2)
                            .cast::<u16>()
                            .write_unaligned((value as u16).to_le());
                    }
                }
                4 => {
                    for n in 0..count as usize {
                        dst.add(n * 4).cast::<u32>().write_unaligned(value.to_le());
                    }
                }
                _ => unreachable!("validated string width"),
            }
        }
    }
    let delta = if reverse {
        0u32.wrapping_sub(count * width)
    } else {
        count * width
    };
    for index in [7, 6] {
        if index == 6 && op.kind != StringKind::Movs {
            continue;
        }
        state.regs[index] = (state.regs[index] & !index_mask)
            | (state.regs[index].wrapping_add(delta) & index_mask);
    }
    state.regs[1] =
        (state.regs[1] & !index_mask) | (state.regs[1].wrapping_sub(count) & index_mask);
    count
}

pub(super) fn execute<const BATCH: bool>(
    state: &mut CpuState,
    memory: &mut Memory,
    operation: StringOp,
    next: u32,
    max_units: u32,
    debug: bool,
) -> Execution {
    use StringKind::*;
    let pc = state.eip;
    let index_width = if operation.word { 2 } else { 4 };
    let counter = Reg {
        index: 1,
        shift: 0,
        width: index_width,
    };
    if operation.repeat && counter.read(&state.regs) == 0 {
        state.eip = next;
        state.rep = None;
        return Execution {
            step: if debug {
                Step::Trap { pc, vector: 1 }
            } else {
                Step::Executed
            },
            units: 1,
        };
    }
    let comparison = matches!(operation.kind, Cmps | Scas);
    let had_continuation = state
        .rep
        .as_ref()
        .is_some_and(|r| r.pc == pc && r.operation == operation);
    let original_flags = if operation.repeat && comparison {
        if !had_continuation {
            state.rep = Some(RepContinuation {
                pc,
                flags: state.eflags,
                operation,
            });
        }
        state.rep.as_ref().unwrap().flags
    } else {
        state.rep = None;
        state.eflags
    };
    let mut units = 0;
    while units < if debug { 1 } else { max_units } {
        if BATCH && operation.repeat && !debug {
            let completed = batch(state, memory, operation, max_units - units);
            if completed != 0 {
                units += completed;
                if counter.read(&state.regs) == 0 {
                    state.eip = next;
                    break;
                }
                continue;
            }
        }
        let mut tx = Transaction {
            regs: state.regs,
            flags: state.eflags,
            eip: pc,
            fs_base: state.fs_base,
            segments: state.segments,
            pc,
            write: None,
            more_writes: Vec::new(),
            indirect_branch: None,
        };
        let source_index = Reg {
            index: 6,
            shift: 0,
            width: index_width,
        };
        let dest_index = Reg {
            index: 7,
            shift: 0,
            width: index_width,
        };
        let source =
            source_index
                .read(&tx.regs)
                .wrapping_add(if operation.fs { state.fs_base } else { 0 });
        let destination = dest_index.read(&tx.regs);
        let accumulator = Reg {
            index: 0,
            shift: 0,
            width: operation.width,
        };
        let element = (|| -> Result<(), Step> {
            match operation.kind {
                Movs => {
                    let value = tx.read_mem(memory, source, operation.width)?;
                    tx.store(
                        memory,
                        Transaction::stack_operand_width(destination, operation.width),
                        value,
                    )?;
                }
                Stos => {
                    let value = accumulator.read(&tx.regs);
                    tx.store(
                        memory,
                        Transaction::stack_operand_width(destination, operation.width),
                        value,
                    )?;
                }
                Lods => {
                    let value = tx.read_mem(memory, source, operation.width)?;
                    accumulator.write(&mut tx.regs, value);
                }
                Cmps => {
                    let a = tx.read_mem(memory, source, operation.width)?;
                    let b = tx.read_mem(memory, destination, operation.width)?;
                    tx.arithmetic(a, b, 0, operation.width, true);
                }
                Scas => {
                    let a = accumulator.read(&tx.regs);
                    let b = tx.read_mem(memory, destination, operation.width)?;
                    tx.arithmetic(a, b, 0, operation.width, true);
                }
            }
            Ok(())
        })();
        if let Err(step) = element {
            if operation.repeat && comparison {
                state.eflags = original_flags;
                // The fault exposes the architectural restart context. A handler
                // may change EFLAGS before restarting the remaining elements;
                // that context is the next instruction's input, not this origin.
                state.rep = None;
            }
            return Execution { step, units };
        }
        let delta = if state.eflags & (1 << 10) != 0 {
            0u32.wrapping_sub(u32::from(operation.width))
        } else {
            u32::from(operation.width)
        };
        if matches!(operation.kind, Movs | Cmps | Lods) {
            let index = source_index.read(&tx.regs).wrapping_add(delta);
            source_index.write(&mut tx.regs, index);
        }
        if operation.kind != Lods {
            let index = dest_index.read(&tx.regs).wrapping_add(delta);
            dest_index.write(&mut tx.regs, index);
        }
        if operation.repeat {
            let count = counter.read(&tx.regs).wrapping_sub(1);
            counter.write(&mut tx.regs, count);
        }
        if let Some((address, width, value)) = tx.write {
            // This element's complete range and permissions have been checked.
            memory
                .write(address, &value.to_le_bytes()[..width as usize])
                .expect("validated REP element write");
        }
        state.regs = tx.regs;
        state.eflags = tx.flags;
        units += 1;
        let more = operation.repeat
            && counter.read(&state.regs) != 0
            && (!comparison || (state.eflags & ZF == 0) == operation.unequal);
        if !more {
            state.eip = next;
            state.rep = None;
        }
        if debug {
            // Unlike a scheduler chunk, a delivered debug exception exposes a
            // restart context that its handler is allowed to change.
            state.rep = None;
            return Execution {
                step: Step::Trap { pc, vector: 1 },
                units,
            };
        }
        if !more {
            break;
        }
    }
    Execution {
        step: Step::Executed,
        units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Memory {
        let mut memory = Memory::new(0, 0x20000).unwrap();
        memory.protect(0, 0x20000, READ | WRITE).unwrap();
        let bytes: Vec<_> = (0..0x20000).map(|n| (n * 17 + n / 256) as u8).collect();
        memory.write(0, &bytes).unwrap();
        memory
    }

    #[test]
    fn batches_match_element_transactions_at_page_and_address_boundaries() {
        for kind in [StringKind::Movs, StringKind::Stos] {
            for width in [1, 2, 4] {
                for word in [false, true] {
                    for reverse in [false, true] {
                        for fs in [false, true] {
                            for (source, dest) in [
                                (0x2800, 0x3800),
                                (0x2ffb, 0x4ff9),
                                (0x3002, 0x4001),
                                (0x2800, 0x2801),
                                (0x2803, 0x2800),
                                (0xfffd, 0x8ffd),
                                (0, 0x8000),
                            ] {
                                let op = StringOp {
                                    kind,
                                    width,
                                    word,
                                    fs,
                                    repeat: true,
                                    unequal: false,
                                };
                                let mut actual = CpuState {
                                    eip: 0x18000,
                                    eflags: 0x8d7 | if reverse { 0x400 } else { 0 },
                                    fs_base: 0x113,
                                    ..Default::default()
                                };
                                actual.regs[0] = 0x817f32af;
                                actual.regs[1] = 23 | if word { 0xa9870000 } else { 0 };
                                actual.regs[6] = source | if word { 0x87650000 } else { 0 };
                                actual.regs[7] = dest | if word { 0x12340000 } else { 0 };
                                let mut expected = actual.clone();
                                let mut a = memory();
                                let mut b = memory();
                                for budget in [1, 3, 7, 12] {
                                    let got = execute::<true>(
                                        &mut actual,
                                        &mut a,
                                        op,
                                        0x18002,
                                        budget,
                                        false,
                                    );
                                    let want = execute::<false>(
                                        &mut expected,
                                        &mut b,
                                        op,
                                        0x18002,
                                        budget,
                                        false,
                                    );
                                    assert_eq!(got, want, "{op:?}, {reverse}, {budget}");
                                    assert_eq!(actual, expected);
                                    assert_eq!(
                                        a.read_data(0, 0x20000).unwrap(),
                                        b.read_data(0, 0x20000).unwrap()
                                    );
                                    if got.step != Step::Executed {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn partial_batches_fault_resume_and_single_step_without_replaying_writes() {
        for kind in [StringKind::Movs, StringKind::Stos] {
            for write_fault in [false, true] {
                for debug in [false, true] {
                    let op = StringOp {
                        kind,
                        width: 4,
                        word: false,
                        fs: false,
                        repeat: true,
                        unequal: false,
                    };
                    let mut a = memory();
                    let mut b = memory();
                    let fault = if write_fault { 0x5000 } else { 0x3000 };
                    a.protect(fault, PAGE_SIZE, 0).unwrap();
                    b.protect(fault, PAGE_SIZE, 0).unwrap();
                    let mut actual = CpuState {
                        eip: 0x18000,
                        eflags: 0xad7,
                        ..Default::default()
                    };
                    actual.regs[0] = 0xdeadbeef;
                    actual.regs[1] = 9;
                    actual.regs[6] = 0x2ff0;
                    actual.regs[7] = 0x4ff0;
                    let mut expected = actual.clone();
                    let got = execute::<true>(&mut actual, &mut a, op, 0x18002, 9, debug);
                    let want = execute::<false>(&mut expected, &mut b, op, 0x18002, 9, debug);
                    assert_eq!(got, want);
                    assert_eq!(actual, expected);
                    for m in [&mut a, &mut b] {
                        m.protect(fault, PAGE_SIZE, READ | WRITE).unwrap();
                        m.write(0x2ff0, &[0x42; 16]).unwrap();
                    }
                    if actual.eip == 0x18000 {
                        assert_eq!(
                            execute::<true>(&mut actual, &mut a, op, 0x18002, 9, false),
                            execute::<false>(&mut expected, &mut b, op, 0x18002, 9, false)
                        );
                    }
                    assert_eq!(actual, expected);
                    assert_eq!(
                        a.read_data(0, 0x20000).unwrap(),
                        b.read_data(0, 0x20000).unwrap()
                    );
                }
            }
        }
    }

    // spec: A64-MEM-3, A64-MEM-5
    #[test]
    fn pcm_batches_keep_budget_page_fault_and_atomic_publication() {
        let mut m = Memory::reserve(0, 0x10000).unwrap();
        m.map(0x2000, PAGE_SIZE, READ | WRITE).unwrap();
        let audio = m.map_shared_audio(0x4000, PAGE_SIZE).unwrap();
        m.write(0x2000, &[0x37; 32]).unwrap();
        let op = StringOp {
            kind: StringKind::Movs,
            width: 4,
            word: false,
            fs: false,
            repeat: true,
            unequal: false,
        };
        let mut state = CpuState::default();
        state.regs[1] = 8;
        state.regs[6] = 0x2000;
        state.regs[7] = 0x4ff0;
        let first = execute::<true>(&mut state, &mut m, op, 2, 3, false);
        assert_eq!(first.units, 3);
        assert_eq!((state.regs[1], state.regs[7]), (5, 0x4ffc));
        let last = execute::<true>(&mut state, &mut m, op, 2, 8, false);
        assert!(matches!(
            last.step,
            Step::WriteFault {
                address: 0x5000,
                ..
            }
        ));
        assert_eq!(
            (state.regs[1], state.regs[6], state.regs[7]),
            (4, 0x2010, 0x5000)
        );
        let mut bytes = [0; 16];
        crate::backend::SharedReadView::copy_into(audio.as_ref(), 0xff0, &mut bytes);
        assert_eq!(bytes, [0x37; 16]);
    }

    #[test]
    fn batches_reject_code_and_file_aliases_but_publish_pcm_atomically() {
        let op = StringOp {
            kind: StringKind::Movs,
            width: 1,
            word: false,
            fs: false,
            repeat: true,
            unequal: false,
        };
        let mut state = CpuState::default();
        state.regs[1] = 16;
        state.regs[6] = 0x2000;
        state.regs[7] = 0x4000;
        let mut m = memory();
        m.protect(0x4000, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
        assert_eq!(batch(&mut state, &mut m, op, 16), 0);
        let mut m = Memory::reserve(0, 0x10000).unwrap();
        let page = crate::backend::FilePage::new([0x71; PAGE_SIZE]);
        m.map_file_pages(0x2000, std::slice::from_ref(&page), READ | WRITE, false)
            .unwrap();
        m.map_file_pages(0x4000, &[page], READ | WRITE, false)
            .unwrap();
        assert_eq!(batch(&mut state, &mut m, op, 16), 0);
        m.prepare_native_access(0x2000, 16, false).unwrap();
        m.prepare_native_access(0x4000, 16, true).unwrap();
        assert_eq!(batch(&mut state, &mut m, op, 16), 0);
        let mut m = Memory::reserve(0, 0x10000).unwrap();
        m.map(0x2000, PAGE_SIZE, READ | WRITE).unwrap();
        m.map_shared_audio(0x4000, PAGE_SIZE).unwrap();
        m.protect(0x4000, PAGE_SIZE, READ | WRITE).unwrap();
        assert_eq!(batch(&mut state, &mut m, op, 16), 16);
    }
}
