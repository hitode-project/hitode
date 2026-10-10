//! Production native code over Windows guest memory protection: software
//! read checks, fault recipes and self-modifying code stay exact.
use crate::memory::testing::query;
use crate::memory::*;

#[test]
fn audio_read_denial_keeps_software_read_check_in_production_native_code() {
    let mut executor = crate::Executor::address_space().unwrap();
    let code = [0xa1, 0, 0, 2, 0, 0xc3];
    executor
        .memory
        .map(0x10000, PAGE_SIZE, READ | EXEC)
        .unwrap();
    executor.memory.write(0x10000, &code).unwrap();
    let pcm = executor
        .memory
        .map_shared_audio(0x20000, PAGE_SIZE)
        .unwrap();
    executor.memory.enable_native_protection().unwrap();
    executor.memory.protect(0x20000, PAGE_SIZE, WRITE).unwrap();
    pcm.store_u8(0, 0x91);
    executor
        .add_region(crate::Region::new(0x10000, code.len()))
        .unwrap();
    executor.state.eip = 0x10000;
    executor.state.regs[0] = 0x12345678;
    let before_recoveries = crate::host_fault::recipe_recoveries();
    assert_eq!(
        executor.run(0x11000, 1000).unwrap(),
        crate::RunOutcome::ReadFault {
            pc: 0x10000,
            address: 0x20000
        }
    );
    assert_eq!(executor.state.regs[0], 0x12345678);
    assert_eq!(pcm.load_u8(0), 0x91);
    assert_eq!(crate::host_fault::recipe_recoveries(), before_recoveries);
    // The capability only covers ordinary pages. SHARED classification and
    // its READ checks remain mandatory before choosing any raw host load.
    assert!(executor.memory.native_fault_reads_available());
}

#[test]
fn production_scalar_fault_recipes_restore_integer_prefix_flags_and_cross_page_operands() {
    use iced_x86::code_asm::*;
    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;
    const STACK: u32 = 0x30000;
    const STOP: u32 = 0x11000;
    for width in [1, 2, 4] {
        let address = DATA + PAGE_SIZE as u32 - (width - 1);
        let mut assembler = CodeAssembler::new(32).unwrap();
        assembler.mov(eax, 0x7fffffffu32).unwrap();
        assembler.add(eax, 1).unwrap();
        assembler.dec(ecx).unwrap();
        assembler.lea(esi, ptr(esi + 4)).unwrap();
        let prefix = assembler.assemble(CODE.into()).unwrap();
        let fault_pc = CODE + prefix.len() as u32;
        match width {
            1 => assembler.movsx(eax, byte_ptr(esi)).unwrap(),
            2 => assembler.movsx(eax, word_ptr(esi)).unwrap(),
            _ => assembler.mov(eax, dword_ptr(esi)).unwrap(),
        }
        assembler.ret().unwrap();
        let code = assembler.assemble(CODE.into()).unwrap();
        let mut results = Vec::new();
        for policy in [false, true] {
            let mut executor = crate::Executor::address_space().unwrap();
            executor.memory.map(CODE, PAGE_SIZE, READ | EXEC).unwrap();
            executor
                .memory
                .map(DATA, PAGE_SIZE * 2, READ | WRITE)
                .unwrap();
            executor.memory.map(STACK, PAGE_SIZE, READ | WRITE).unwrap();
            executor.memory.write(CODE, &code).unwrap();
            executor.memory.write(STACK, &STOP.to_le_bytes()).unwrap();
            executor
                .memory
                .write(address, &[0x81, 0xfe, 0x54, 0x91][..width as usize])
                .unwrap();
            executor
                .memory
                .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, 0)
                .unwrap();
            if policy {
                executor.enable_native_protection().unwrap();
            }
            executor
                .add_region(crate::Region::new(CODE, code.len()))
                .unwrap();
            executor.state.eip = CODE;
            executor.state.eflags = 0x203;
            executor.state.regs = [
                0x12345678,
                0x80000000,
                0x87654321,
                0x19,
                STACK,
                0x35,
                address - 4,
                0x71,
            ];
            executor.state.xmm[3] = [0xa7; 16];
            let before_count = crate::host_fault::recipe_recoveries();
            assert_eq!(
                executor.run(STOP, 1000).unwrap(),
                crate::RunOutcome::ReadFault {
                    pc: fault_pc,
                    address: DATA + PAGE_SIZE as u32
                }
            );
            assert_eq!(
                crate::host_fault::recipe_recoveries() - before_count,
                u64::from(policy),
                "width {width}"
            );
            assert_eq!(executor.stats.interpreter_instructions, 0);
            assert_eq!(executor.state.regs[0], 0x80000000);
            assert_eq!(executor.state.regs[1], 0x7fffffff);
            assert_eq!(executor.state.regs[6], address);
            assert_eq!(executor.state.xmm[3], [0xa7; 16]);
            assert_eq!(
                executor
                    .memory
                    .read_data(address, width as usize)
                    .unwrap()
                    .as_ref(),
                &[0x81, 0xfe, 0x54, 0x91][..width as usize]
            );
            results.push((
                executor.state.regs,
                executor.state.eflags,
                executor.state.eip,
                executor.state.fuel,
            ));
            executor
                .memory
                .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, READ)
                .unwrap();
            assert_eq!(
                executor.run(STOP, 1000).unwrap(),
                crate::RunOutcome::ReachedStop
            );
            assert_eq!(executor.state.regs[6], address); // Prefix was not replayed.
            assert_eq!(
                executor.state.regs[0],
                match width {
                    1 => 0xffffff81,
                    2 => 0xfffffe81,
                    _ => 0x9154fe81,
                }
            );
        }
        assert_eq!(results[0], results[1], "width {width}");
    }
}

#[test]
fn reentered_loops_and_tail_linked_callees_keep_software_read_checks() {
    use iced_x86::code_asm::*;
    const CODE: u32 = 0x10000;
    const CALLEE: u32 = 0x12000;
    const DATA: u32 = 0x20000;
    const STOP: u32 = 0x11000;
    let verifying =
        std::env::var("ADV_JIT_VERIFY").is_ok_and(|mode| !matches!(mode.as_str(), "" | "0"));
    for linked in [false, true] {
        let mut executor = crate::Executor::address_space().unwrap();
        executor.map(CODE, PAGE_SIZE, READ | EXEC).unwrap();
        executor.map(CALLEE, PAGE_SIZE, READ | EXEC).unwrap();
        executor.map(DATA, PAGE_SIZE * 2, READ | WRITE).unwrap();
        executor.write(DATA, &0x9177u32.to_le_bytes()).unwrap();
        executor
            .protect(DATA + PAGE_SIZE as u32, PAGE_SIZE, 0)
            .unwrap();
        executor.enable_native_protection().unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        let mut top = assembler.create_label();
        assembler.set_label(&mut top).unwrap();
        assembler.mov(eax, dword_ptr(esi)).unwrap();
        if linked {
            assembler.add(ebx, 7).unwrap();
            assembler.jmp(CALLEE as u64).unwrap();
        } else {
            assembler.add(esi, PAGE_SIZE as i32).unwrap();
            assembler.dec(ecx).unwrap();
            assembler.jnz(top).unwrap();
            assembler.ret().unwrap();
        }
        let code = assembler.assemble(CODE.into()).unwrap();
        executor.write(CODE, &code).unwrap();
        executor
            .add_region(crate::Region::new(CODE, code.len()))
            .unwrap();
        let fault_pc = if linked {
            let mut callee = CodeAssembler::new(32).unwrap();
            callee.add(eax, 3).unwrap();
            let prefix = callee.assemble(CALLEE.into()).unwrap();
            let fault_pc = CALLEE + prefix.len() as u32;
            callee.mov(edx, dword_ptr(DATA + PAGE_SIZE as u32)).unwrap();
            callee.ret().unwrap();
            let code = callee.assemble(CALLEE.into()).unwrap();
            executor.write(CALLEE, &code).unwrap();
            executor
                .add_region(crate::Region::new(CALLEE, code.len()))
                .unwrap();
            fault_pc
        } else {
            CODE
        };
        executor.state.eip = CODE;
        executor.state.regs[1] = 2;
        executor.state.regs[6] = DATA;
        let recoveries = crate::host_fault::recipe_recoveries();
        assert_eq!(
            executor.run(STOP, 1000).unwrap(),
            crate::RunOutcome::ReadFault {
                pc: fault_pc,
                address: DATA + PAGE_SIZE as u32
            }
        );
        // Verification splits the loop/tail transfer into fresh C entries,
        // which establish their own hardware-read recovery sessions.
        assert_eq!(
            crate::host_fault::recipe_recoveries() - recoveries,
            u64::from(verifying)
        );
        assert_eq!(executor.state.regs[0], if linked { 0x917a } else { 0x9177 });
        assert_eq!(executor.state.regs[1], if linked { 2 } else { 1 });
        assert_eq!(executor.state.regs[2], 0);
        assert_eq!(executor.state.regs[3], if linked { 7 } else { 0 });
        assert_eq!(
            executor.state.regs[6],
            if linked {
                DATA
            } else {
                DATA + PAGE_SIZE as u32
            }
        );
        assert_eq!(executor.stats.interpreter_instructions, 0);
        if linked {
            // Verification enters each region from Rust instead of linking.
            assert_eq!(executor.stats.native_entries, 1 + u64::from(verifying));
        }
    }
}

#[test]
fn ordinary_write_only_pages_disable_the_hardware_read_capability() {
    let mut executor = crate::Executor::address_space().unwrap();
    let code = [0xa1, 0, 0, 2, 0, 0xc3];
    executor.map(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
    executor.map(0x20000, PAGE_SIZE, WRITE).unwrap();
    executor.write(0x10000, &code).unwrap();
    executor.write(0x20000, &0x91u32.to_le_bytes()).unwrap();
    executor.enable_native_protection().unwrap();
    assert!(!executor.memory.native_fault_reads_available());
    executor
        .add_region(crate::Region::new(0x10000, code.len()))
        .unwrap();
    executor.state.eip = 0x10000;
    executor.state.regs[0] = 0x12345678;
    let recoveries = crate::host_fault::recipe_recoveries();
    assert_eq!(
        executor.run(0x11000, 1000).unwrap(),
        crate::RunOutcome::ReadFault {
            pc: 0x10000,
            address: 0x20000
        }
    );
    assert_eq!(executor.state.regs[0], 0x12345678);
    assert_eq!(crate::host_fault::recipe_recoveries(), recoveries);
}

#[test]
fn concurrent_production_recipe_faults_keep_tls_and_code_owners_independent() {
    let threads: Vec<_> = (0..8u32)
        .map(|thread| {
            std::thread::spawn(move || {
                let mut executor = crate::Executor::address_space().unwrap();
                let code = [0x41, 0x8b, 0x06, 0xc3]; // inc ecx; mov eax,[esi]; ret
                executor.map(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
                executor.map(0x20000, PAGE_SIZE, 0).unwrap();
                executor.write(0x10000, &code).unwrap();
                executor.enable_native_protection().unwrap();
                executor
                    .add_region(crate::Region::new(0x10000, code.len()))
                    .unwrap();
                let before = crate::host_fault::recipe_recoveries();
                for iteration in 0..32 {
                    executor.state.eip = 0x10000;
                    executor.state.regs[0] = 0xabcdef00 | thread;
                    executor.state.regs[1] = 0x7fffffffu32.wrapping_add(iteration);
                    executor.state.regs[6] = 0x20000;
                    executor.state.eflags = 0x203;
                    assert_eq!(
                        executor.run(0x11000, 1000).unwrap(),
                        crate::RunOutcome::ReadFault {
                            pc: 0x10001,
                            address: 0x20000
                        }
                    );
                    assert_eq!(executor.state.regs[0], 0xabcdef00 | thread);
                    assert_eq!(
                        executor.state.regs[1],
                        0x80000000u32.wrapping_add(iteration)
                    );
                    assert_eq!(executor.state.eflags & 1, 1);
                }
                assert_eq!(crate::host_fault::recipe_recoveries() - before, 32);
                assert_eq!(executor.stats.interpreter_instructions, 0);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn production_jit_with_native_permissions_keeps_smc_and_precise_checked_faults() {
    use iced_x86::code_asm::*;
    let code_at = 0x10000;
    let data_at = 0x20000;
    let stack_at = 0x30ff0;
    let stop = 0x11000u32;
    let mut assembler = CodeAssembler::new(32).unwrap();
    assembler.mov(eax, dword_ptr(data_at)).unwrap();
    assembler.inc(eax).unwrap();
    assembler.mov(dword_ptr(code_at + 0x100), eax).unwrap();
    assembler.ret().unwrap();
    let code = assembler.assemble(code_at as u64).unwrap();
    let mut states = Vec::new();
    for native in [false, true] {
        let mut executor = crate::Executor::address_space().unwrap();
        executor
            .memory
            .map(code_at as u32, PAGE_SIZE * 2, READ | WRITE | EXEC)
            .unwrap();
        executor
            .memory
            .map(data_at as u32, PAGE_SIZE, READ | WRITE)
            .unwrap();
        executor
            .memory
            .map(stack_at & !4095, PAGE_SIZE, READ | WRITE)
            .unwrap();
        executor.memory.write(code_at as u32, &code).unwrap();
        executor
            .memory
            .write(data_at as u32, &0x1234u32.to_le_bytes())
            .unwrap();
        executor
            .memory
            .write(stack_at, &stop.to_le_bytes())
            .unwrap();
        executor.memory.enable_native_protection().unwrap();
        executor.state.eip = code_at as u32;
        executor.state.regs[4] = stack_at;
        if native {
            executor
                .add_region(crate::Region::new(code_at as u32, code.len()))
                .unwrap();
        }
        assert_eq!(
            executor.run(stop, 1000).unwrap(),
            crate::RunOutcome::ReachedStop
        );
        assert_eq!(
            executor.memory.read(code_at as u32 + 0x100, 4).unwrap(),
            &0x1235u32.to_le_bytes()
        );
        assert_eq!(query(&executor.memory, code_at as u32).protection, 2);
        states.push((
            executor.state.regs,
            executor.state.eflags,
            executor.state.eip,
        ));
        executor
            .memory
            .protect(data_at as u32, PAGE_SIZE, 0)
            .unwrap();
        executor.state.eip = code_at as u32;
        let before = executor.state.regs;
        assert_eq!(
            executor.run(stop, 1000).unwrap(),
            crate::RunOutcome::ReadFault {
                pc: code_at as u32,
                address: data_at as u32
            }
        );
        assert_eq!(executor.state.eip, code_at as u32);
        assert_eq!(executor.state.regs, before);
        assert_eq!(executor.state.fault_addr, data_at as u32);
    }
    assert_eq!(states[0], states[1]);
}
