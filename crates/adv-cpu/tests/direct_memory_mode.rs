use adv_cpu::Executor;

#[test]
fn direct_memory_selection_reports_platform_and_lifecycle_limits() {
    let mut executor = Executor::new(0x10000, 4096).unwrap();
    let supported = cfg!(all(
        any(
            target_os = "linux",
            target_os = "android",
            all(target_os = "macos", target_arch = "aarch64")
        ),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ));
    let selected = executor.set_direct_memory(true);
    if supported {
        selected.unwrap();
        executor.set_direct_memory(true).unwrap();
        assert!(executor.set_direct_memory(false).is_err());
    } else {
        assert!(selected.unwrap_err().contains("Linux/Android"));
        executor.set_direct_memory(false).unwrap();
    }
}

#[cfg(all(
    any(
        target_os = "linux",
        target_os = "android",
        all(target_os = "macos", target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn direct_memory_executes_scalars_and_recovers_protection_faults() {
    use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::*;
    let hardware = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } == 4096
        || std::env::var("ADV_DIRECT_MEMORY_INLINE").as_deref() == Ok("0");

    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;
    let mut assembler = CodeAssembler::new(32).unwrap();
    assembler.inc(ebx).unwrap();
    let write_pc = CODE + assembler.assemble(u64::from(CODE)).unwrap().len() as u32;
    assembler.mov(dword_ptr(ecx), edx).unwrap();
    assembler.mov(eax, dword_ptr(ecx)).unwrap();
    assembler.inc(esi).unwrap();
    let code = assembler.assemble(u64::from(CODE)).unwrap();
    let stop = CODE + code.len() as u32;
    let mut executor = Executor::new(CODE, 0x20000).unwrap();
    executor.set_direct_memory(true).unwrap();
    executor.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    executor.protect(DATA, 4096, READ | WRITE).unwrap();
    executor.write(CODE, &code).unwrap();
    // The counter assertions below require native reentry at the pending
    // store after a permission fault, without replaying the committed INC.
    let mut region = Region::new(CODE, code.len());
    region.entries.push(write_pc);
    executor.add_region(region).unwrap();
    executor.state.regs[1] = DATA;
    executor.state.regs[2] = 0x12345678;
    executor.state.eip = CODE;
    assert_eq!(executor.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[0], executor.state.regs[2]);
    assert_eq!(
        executor.stats.direct_memory.accesses,
        if hardware { 2 } else { 0 }
    );
    assert_eq!(
        executor.stats.direct_memory.checked_fallbacks,
        if hardware { 0 } else { 2 }
    );

    executor.protect(DATA, 4096, READ).unwrap();
    executor.state.eip = CODE;
    assert!(matches!(
        executor.run(stop, 100).unwrap(),
        RunOutcome::WriteFault { address: DATA, .. }
    ));
    assert_eq!(executor.state.regs[3], 2);
    assert_eq!(executor.state.regs[6], 1);
    assert_eq!(executor.stats.direct_memory.violations, u64::from(hardware));
    assert_eq!(
        executor.stats.direct_memory.accesses,
        if hardware { 3 } else { 0 }
    );
    executor.protect(DATA, 4096, READ | WRITE).unwrap();
    assert_eq!(executor.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[3], 2);
    assert_eq!(executor.state.regs[6], 2);
    assert_eq!(
        executor.stats.direct_memory.checked_fallbacks,
        if hardware { 0 } else { 5 }
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_16k_subpages_boundaries_and_partial_recommit_keep_guest_permissions() {
    use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::*;
    assert_eq!(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }, 16384);
    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.mov(eax, dword_ptr(ecx)).unwrap();
    let load = asm.assemble(u64::from(CODE)).unwrap();
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.mov(dword_ptr(ecx), edx).unwrap();
    let store = asm.assemble(u64::from(CODE)).unwrap();
    for (write, code) in [(false, load), (true, store)] {
        let mut e = Executor::address_space().unwrap();
        e.set_direct_memory(true).unwrap();
        e.map(CODE, 4096, READ | WRITE | EXEC).unwrap();
        e.map(DATA, 32768, READ | WRITE).unwrap();
        e.write(CODE, &code).unwrap();
        e.add_region(Region::new(CODE, code.len())).unwrap();
        let stop = CODE + code.len() as u32;
        let access = |e: &mut Executor, address| {
            e.state.eip = CODE;
            e.state.regs[1] = address;
            e.state.regs[2] = 0x12345678;
            e.run(stop, 100).unwrap()
        };
        // All four guest permission combinations share one native page.
        for (index, perm) in [READ | WRITE, READ, WRITE, 0].into_iter().enumerate() {
            e.protect(DATA + index as u32 * 4096, 4096, perm).unwrap();
        }
        for (index, perm) in [READ | WRITE, READ, WRITE, 0].into_iter().enumerate() {
            let address = DATA + index as u32 * 4096;
            let outcome = access(&mut e, address);
            if perm & if write { WRITE } else { READ } != 0 {
                assert_eq!(outcome, RunOutcome::ReachedStop);
            } else if write {
                assert_eq!(outcome, RunOutcome::WriteFault { pc: CODE, address });
            } else {
                assert_eq!(outcome, RunOutcome::ReadFault { pc: CODE, address });
            }
        }
        // Mixed 16 KiB pages use checked aliases in the inline configuration.
        // The legacy opt-out retains its explicit recovery sessions.
        if std::env::var("ADV_DIRECT_MEMORY_INLINE").as_deref() == Ok("0") {
            assert_eq!(e.stats.direct_memory.recoveries, 2);
            assert_eq!(e.stats.direct_memory.violations, 2);
        } else {
            assert_eq!(e.stats.direct_memory.accesses, 0);
            assert!(e.stats.direct_memory.checked_fallbacks >= 4);
        }
        // Cross both a guest boundary and a real host boundary. A denied
        // suffix must not commit even the writable prefix of a store.
        e.protect(DATA, 32768, READ | WRITE).unwrap();
        for boundary in [4096, 16384] {
            let address = DATA + boundary - 2;
            e.write(address, &[0x55; 4]).unwrap();
            e.protect(DATA + boundary, 4096, if write { READ } else { 0 })
                .unwrap();
            assert!(matches!(
                access(&mut e, address),
                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
            ));
            assert_eq!(e.memory().read(address, 2).unwrap().as_ref(), &[0x55; 2]);
            e.protect(DATA + boundary, 4096, READ | WRITE).unwrap();
            assert_eq!(access(&mut e, address), RunOutcome::ReachedStop);
        }
        e.write(DATA, &[0x77; 4]).unwrap();
        e.unmap(DATA + 4096, 4096).unwrap();
        assert_eq!(access(&mut e, DATA), RunOutcome::ReachedStop);
        assert!(matches!(
            access(&mut e, DATA + 4096),
            RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
        ));
        e.map(DATA + 4096, 4096, READ).unwrap();
        assert_eq!(e.memory().read(DATA + 4096, 4).unwrap().as_ref(), &[0; 4]);
        e.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
        assert_eq!(access(&mut e, DATA + 4096), RunOutcome::ReachedStop);
    }
}
