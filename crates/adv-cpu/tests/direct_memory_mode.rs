use adv_cpu::Executor;

#[test]
fn direct_memory_selection_reports_platform_and_lifecycle_limits() {
    let mut executor = Executor::new(0x10000, 4096).unwrap();
    let supported = cfg!(all(
        any(target_os = "linux", target_os = "android"),
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
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn direct_memory_executes_scalars_and_recovers_protection_faults() {
    use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::*;

    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;
    let mut assembler = CodeAssembler::new(32).unwrap();
    assembler.inc(ebx).unwrap();
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
    executor.add_region(Region::new(CODE, code.len())).unwrap();
    executor.state.regs[1] = DATA;
    executor.state.regs[2] = 0x12345678;
    executor.state.eip = CODE;
    assert_eq!(executor.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[0], executor.state.regs[2]);
    assert_eq!(executor.stats.direct_memory.accesses, 2);
    assert_eq!(executor.stats.direct_memory.checked_fallbacks, 0);

    executor.protect(DATA, 4096, READ).unwrap();
    executor.state.eip = CODE;
    assert!(matches!(
        executor.run(stop, 100).unwrap(),
        RunOutcome::WriteFault { address: DATA, .. }
    ));
    assert_eq!(executor.state.regs[3], 2);
    assert_eq!(executor.state.regs[6], 1);
    assert_eq!(executor.stats.direct_memory.violations, 1);
    executor.protect(DATA, 4096, READ | WRITE).unwrap();
    assert_eq!(executor.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[3], 2);
    assert_eq!(executor.state.regs[6], 2);
    assert_eq!(executor.stats.direct_memory.accesses, 5);
    assert_eq!(executor.stats.direct_memory.checked_fallbacks, 0);
}
