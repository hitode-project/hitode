mod support;
use adv_cpu::*;
use iced_x86::code_asm::*;
use support::Executor;

#[test]
fn scattered_regions_invalidate_only_the_modified_code_page() {
    let (entry, dll, stack, stop) = (0x400000u32, 0x60000000u32, 0x70000000u32, 0x7e100000u32);
    let mut m = Executor::address_space().unwrap();
    for p in [entry, dll, stop] {
        m.map(p, 4096, READ | EXEC).unwrap();
    }
    m.map(stack, 4096, READ | WRITE).unwrap();
    let mut a = CodeAssembler::new(32).unwrap();
    a.call(u64::from(dll)).unwrap();
    a.add(eax, 1).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(entry)).unwrap();
    m.write(entry, &code).unwrap();
    m.write(dll, &[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();
    m.add_region(Region::new(entry, code.len())).unwrap();
    m.add_region(Region::new(dll, 6)).unwrap();
    for expected in [8, 10] {
        m.state.eip = entry;
        m.state.regs[4] = stack + 0x800;
        m.write(stack + 0x800, &stop.to_le_bytes()).unwrap();
        assert_eq!(m.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], expected);
        if expected == 8 {
            m.write(dll + 1, &9u32.to_le_bytes()).unwrap();
        }
    }

    m.unmap(dll, 4096).unwrap();
    assert!(m.memory().read(dll, 4).is_err());
    m.map(dll, 4096, READ | WRITE).unwrap();
    assert_eq!(m.memory().read(dll, 6).unwrap(), [0; 6]);
}

#[test]
fn context_switch_keeps_fpu_and_xmm_separate() {
    let (code, stop, stack, data) = (0x10000u32, 0x18000u32, 0x20000u32, 0x21000u32);
    let mut m = Executor::new(code, 0x20000).unwrap();
    m.protect(code, 0x20000, READ | WRITE).unwrap();
    // fld1 ; ret, then fstp dword [data] ; ret. Both are Unicorn-owned x87 operations.
    m.write(code, &[0xd9, 0xe8, 0xc3]).unwrap();
    let mut a = CodeAssembler::new(32).unwrap();
    a.fstp(dword_ptr(data as i32)).unwrap();
    a.ret().unwrap();
    m.write(code + 16, &a.assemble(u64::from(code + 16)).unwrap())
        .unwrap();
    m.protect(code, 4096, READ | EXEC).unwrap();
    m.protect(stop, 4096, READ | EXEC).unwrap();
    m.write(stack, &stop.to_le_bytes()).unwrap();
    m.state.regs[4] = stack;
    m.state.eip = code;
    let blank = m.save_context().unwrap();
    m.run_unicorn(stop, 10).unwrap();
    m.state.xmm[3] = [17; 16];
    let a = m.save_context().unwrap();
    m.restore_context(&blank).unwrap();
    m.state.xmm[3] = [29; 16];
    let b = m.save_context().unwrap();
    m.restore_context(&a).unwrap();
    assert_eq!(m.state.xmm[3], [17; 16]);
    m.state.eip = code + 16;
    m.state.regs[4] = stack;
    m.run_unicorn(stop, 10).unwrap();
    assert_eq!(m.memory().read(data, 4).unwrap(), 1f32.to_le_bytes());
    m.restore_context(&b).unwrap();
    assert_eq!(m.state.xmm[3], [29; 16]);
}
