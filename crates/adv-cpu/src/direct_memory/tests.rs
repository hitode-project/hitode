//! Synthetic executor fixtures; no guest sample or external tool is needed.
use crate::{EXEC, Executor, READ, Region, RunOutcome, WRITE};

fn owner(code: &[u8], mixed: bool) -> Executor {
    let mut executor = Executor::new(0x10000, 0x20000).unwrap();
    executor.set_direct_memory(true).unwrap();
    if mixed {
        // Real mmap/mprotect, grouped to 16 KiB on a 4 KiB host.
        executor.memory.direct_test_pages(16384).unwrap();
    }
    executor
        .memory
        .protect(0x10000, 4096, READ | WRITE | EXEC)
        .unwrap();
    executor
        .memory
        .protect(
            0x20000,
            if mixed {
                4096
            } else {
                (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }) as usize
            },
            READ | WRITE,
        )
        .unwrap();
    executor.memory.write(0x10000, code).unwrap();
    executor
        .add_region(Region::new(0x10000, code.len()))
        .unwrap();
    executor.state.eip = 0x10000;
    executor
}

#[test]
fn mixed_scalar_load_store_and_suffix_continue_under_strict_verification() {
    for mixed in [false, true] {
        // inc ebx; mov [ecx],edx; mov eax,[ecx]; add eax,1
        let code = [0x43, 0x89, 0x11, 0x8b, 0x01, 0x83, 0xc0, 1];
        let mut e = owner(&code, mixed);
        e.set_tier_verification(Some(true));
        e.state.regs[1] = 0x20000;
        e.state.regs[2] = 0x12345678;
        for _ in 0..4 {
            e.state.eip = 0x10000;
            assert_eq!(e.run(0x10008, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(e.state.regs[0], 0x12345679);
        }
        assert_eq!(e.state.regs[3], 4);
        assert_eq!(e.stats.verification.mismatches, 0);
        assert!(e.stats.verification.verified > 0);
        assert_eq!(e.stats.direct_memory.sites, 2);
        assert_eq!(e.stats.direct_memory.accesses, 8);
        assert_eq!(e.stats.direct_memory.recoveries, if mixed { 8 } else { 0 });
        assert_eq!(e.stats.direct_memory.violations, 0);
    }
}

#[test]
fn real_read_and_write_faults_preserve_prefix_and_retry() {
    for write in [false, true] {
        // inc ebx; mov eax,[ecx] / mov [ecx],edx; inc esi
        let code = [
            0x43,
            if write { 0x89 } else { 0x8b },
            if write { 0x11 } else { 0x01 },
            0x46,
        ];
        let mut e = owner(&code, true);
        e.memory
            .protect(0x20000, 4096, if write { READ } else { 0 })
            .unwrap();
        e.state.regs[0] = 0xaabbccdd;
        e.state.regs[1] = 0x20000;
        e.state.regs[2] = 0x12345678;
        let expected = if write {
            RunOutcome::WriteFault {
                pc: 0x10001,
                address: 0x20000,
            }
        } else {
            RunOutcome::ReadFault {
                pc: 0x10001,
                address: 0x20000,
            }
        };
        assert_eq!(e.run(0x10004, 100).unwrap(), expected);
        assert_eq!(e.state.regs[3], 1);
        assert_eq!(e.state.regs[0], 0xaabbccdd);
        assert_eq!(e.state.regs[6], 0);
        assert_eq!(e.stats.direct_memory.violations, 1);
        e.memory.protect(0x20000, 4096, READ | WRITE).unwrap();
        assert_eq!(e.run(0x10004, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(e.state.regs[3], 1);
        assert_eq!(e.state.regs[6], 1);
    }
}

#[test]
fn scalar_widths_zero_extension_partial_stores_and_unaligned_fallback() {
    for (load, store, width) in [
        (&[0x0f, 0xb6, 0x01][..], &[0x88, 0x11][..], 1),
        (&[0x0f, 0xb7, 0x01][..], &[0x66, 0x89, 0x11][..], 2),
        (&[0x8b, 0x01][..], &[0x89, 0x11][..], 4),
    ] {
        let code: Vec<_> = store.iter().chain(load).copied().collect();
        let mut e = owner(&code, true);
        e.state.regs[1] = 0x20000;
        e.state.regs[2] = 0x89abcdef;
        e.state.regs[0] = u32::MAX;
        let stop = 0x10000 + code.len() as u32;
        assert_eq!(e.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
        let expected = 0x89abcdef & (u32::MAX >> ((4 - width) * 8));
        assert_eq!(e.state.regs[0], expected);
        assert_eq!(e.stats.direct_memory.recoveries, 2);
        if width > 1 {
            e.state.eip = 0x10000;
            e.state.regs[1] += 1;
            assert_eq!(e.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(e.state.regs[0], expected);
            assert_eq!(e.stats.direct_memory.checked_fallbacks, 1);
        }
    }
}

#[cfg(all(target_os = "android", target_arch = "aarch64"))]
#[test]
fn tagged_thunk_entries_recover_with_untagged_signal_pcs() {
    use iced_x86::code_asm::*;

    let mut assembler = CodeAssembler::new(32).unwrap();
    assembler.mov(dword_ptr(ecx), edx).unwrap();
    assembler.mov(eax, dword_ptr(ecx)).unwrap();
    let code = assembler.assemble(0x10000).unwrap();
    for tag in [0_u64, 0xb4, 0xa5] {
        let mut executor = owner(&code, true);
        for thunk in &mut executor.memory.direct_engine.as_mut().unwrap().thunks {
            let address = (thunk.entry as usize & 0x00ff_ffff_ffff_ffff) | (tag as usize) << 56;
            // SAFETY: Android's instruction fetch ignores the top byte. Only
            // the borrowed entry changes; the allocator's owning pointer keeps
            // its original tag for deallocation. Signal PCs have no such tag.
            thunk.entry = unsafe { std::mem::transmute::<usize, super::Entry>(address) };
        }
        executor.state.regs[1] = 0x20000;
        executor.state.regs[2] = 0x89abcdef;
        assert_eq!(
            executor.run(0x10000 + code.len() as u32, 100).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(executor.state.regs[0], executor.state.regs[2]);
        assert_eq!(executor.stats.direct_memory.accesses, 2);
        assert_eq!(executor.stats.direct_memory.recoveries, 2);
        assert_eq!(executor.stats.direct_memory.checked_fallbacks, 0);
    }
}

#[test]
fn cross_page_store_falls_back_without_partial_commit() {
    let mut e = owner(&[0x89, 0x11], true);
    e.state.regs[1] = 0x20ffe;
    e.state.regs[2] = 0x12345678;
    e.memory.write(0x20ffe, &[0xaa, 0xbb]).unwrap();
    assert_eq!(
        e.run(0x10002, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: 0x10000,
            address: 0x20ffe
        }
    );
    assert_eq!(
        e.memory.read_data(0x20ffe, 2).unwrap().as_ref(),
        &[0xaa, 0xbb]
    );
    assert_eq!(e.stats.direct_memory.accesses, 0);
    assert_eq!(e.stats.direct_memory.checked_fallbacks, 1);
    e.memory.protect(0x21000, 4096, READ | WRITE).unwrap();
    assert_eq!(e.run(0x10002, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        e.memory.read_data(0x20ffe, 4).unwrap().as_ref(),
        &0x12345678_u32.to_le_bytes()
    );
}

#[test]
fn code_write_exits_lease_and_invalidates_current_suffix() {
    // mov [ecx],dl; inc eax. Replace inc eax with inc ebx in this unit.
    let mut e = owner(&[0x88, 0x11, 0x40], false);
    e.state.regs[1] = 0x10002;
    e.state.regs[2] = 0x43;
    assert_eq!(e.run(0x10003, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(e.state.regs[0], 0);
    assert_eq!(e.state.regs[3], 1);
    assert_eq!(e.stats.direct_memory.code_writes, 1);
    assert!(e.stats.invalidations > 0);
}

#[test]
fn mapping_protection_release_and_recommit_keep_the_alias_consistent() {
    let mut e = Executor::address_space().unwrap();
    e.set_direct_memory(true).unwrap();
    e.memory.map(0x20000, 4096, READ | WRITE).unwrap();
    e.memory.write(0x20000, &[7]).unwrap();
    e.memory.protect(0x20000, 4096, READ).unwrap();
    assert_eq!(e.memory.read_data(0x20000, 1).unwrap()[0], 7);
    e.memory.unmap(0x20000, 4096).unwrap();
    e.memory.map(0x20000, 4096, READ | WRITE).unwrap();
    assert_eq!(e.memory.read_data(0x20000, 1).unwrap()[0], 0);
}

#[test]
fn shared_overlay_and_cross_page_load_use_checked_storage() {
    let mut e = owner(&[0x89, 0x11, 0x8b, 0x01], true);
    e.memory.unmap(0x20000, 4096).unwrap();
    let audio = e.memory.map_shared_audio(0x20000, 4096).unwrap();
    e.state.regs[1] = 0x20000;
    e.state.regs[2] = 0x12345678;
    assert_eq!(e.run(0x10004, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(e.state.regs[0], 0x12345678);
    assert_eq!(audio.load_u8(0), 0x78);
    assert_eq!(e.stats.direct_memory.accesses, 0);
    assert_eq!(e.stats.direct_memory.checked_fallbacks, 2);
    e.memory.unmap(0x20000, 4096).unwrap();
    e.memory.map(0x20000, 4096, READ | WRITE).unwrap();
    e.memory.protect(0x21000, 4096, READ | WRITE).unwrap();
    e.memory.write(0x20ffe, &[0x78, 0x56, 0x34, 0x12]).unwrap();
    let mut region = Region::new(0x10000, 4);
    region.entries.push(0x10002);
    e.add_region(region).unwrap();
    e.state.eip = 0x10002;
    e.state.regs[1] = 0x20ffe;
    assert_eq!(e.run(0x10004, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(e.state.regs[0], 0x12345678);
    assert_eq!(e.stats.direct_memory.accesses, 0);
    assert_eq!(e.stats.direct_memory.checked_fallbacks, 3);
}

#[test]
fn committed_store_and_flags_survive_a_later_fault_without_replay() {
    // mov [ecx],edx; add ebx,1; mov eax,[edi]
    let mut e = owner(&[0x89, 0x11, 0x83, 0xc3, 1, 0x8b, 0x07], true);
    e.state.regs[1] = 0x20000;
    e.state.regs[2] = 0x12345678;
    e.state.regs[3] = u32::MAX;
    e.state.regs[7] = 0x21000;
    assert_eq!(
        e.run(0x10007, 100).unwrap(),
        RunOutcome::ReadFault {
            pc: 0x10005,
            address: 0x21000
        }
    );
    assert_eq!(e.state.regs[3], 0);
    assert_eq!(e.state.eflags & 0x41, 0x41);
    assert_eq!(
        e.memory.read_data(0x20000, 4).unwrap().as_ref(),
        &0x12345678_u32.to_le_bytes()
    );
    e.memory.write(0x20000, &[0xaa; 4]).unwrap();
    e.memory.protect(0x21000, 4096, READ | WRITE).unwrap();
    assert_eq!(e.run(0x10007, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(e.state.regs[3], 0);
    assert_eq!(e.memory.read_data(0x20000, 4).unwrap().as_ref(), &[0xaa; 4]);
}
