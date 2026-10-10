use super::*;
use iced_x86::code_asm::*;

fn forms() -> Vec<Instruction> {
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, dword_ptr(esi)).unwrap();
    a.mov(dword_ptr(esi), eax).unwrap();
    a.mov(ah, byte_ptr(esi)).unwrap();
    a.mov(word_ptr(esi), ax).unwrap();
    a.movzx(eax, byte_ptr(esi)).unwrap();
    a.movsx(eax, word_ptr(esi)).unwrap();
    a.mov(eax, dword_ptr(bx + si + 17).fs()).unwrap();
    a.mov(dword_ptr(bx + si + 17).fs(), eax).unwrap();
    macro_rules! binary {
        ($op:ident) => {
            a.$op(dword_ptr(esi), eax).unwrap();
            a.$op(eax, dword_ptr(esi)).unwrap();
            a.$op(byte_ptr(esi), ah).unwrap();
            a.$op(word_ptr(esi), -1).unwrap();
        };
    }
    binary!(add);
    binary!(adc);
    binary!(sub);
    binary!(sbb);
    binary!(and);
    binary!(or);
    binary!(xor);
    binary!(cmp);
    a.test(dword_ptr(esi), eax).unwrap();
    a.inc(byte_ptr(esi)).unwrap();
    a.dec(word_ptr(esi)).unwrap();
    a.neg(dword_ptr(esi)).unwrap();
    a.not(dword_ptr(esi)).unwrap();
    macro_rules! shift {
        ($op:ident) => {
            a.$op(ah, cl).unwrap();
            a.$op(ax, cl).unwrap();
            a.$op(eax, cl).unwrap();
            a.$op(byte_ptr(esi), cl).unwrap();
            a.$op(word_ptr(esi), cl).unwrap();
            a.$op(dword_ptr(esi), cl).unwrap();
        };
    }
    shift!(shl);
    shift!(shr);
    shift!(sar);
    shift!(rol);
    shift!(ror);
    shift!(rcl);
    shift!(rcr);
    a.imul_2(eax, dword_ptr(esi)).unwrap();
    a.imul_2(ax, bx).unwrap();
    a.imul_3(eax, dword_ptr(esi), -71).unwrap();
    a.imul_3(ax, bx, -13).unwrap();
    a.setbe(byte_ptr(esi)).unwrap();
    a.jmp(dword_ptr(esi)).unwrap();
    a.jmp(eax).unwrap();
    a.push(eax).unwrap();
    a.push(esp).unwrap();
    a.push(sp).unwrap();
    a.push(dword_ptr(esp)).unwrap();
    a.push(word_ptr(esi)).unwrap();
    a.pop(esp).unwrap();
    a.pop(sp).unwrap();
    a.pop(eax).unwrap();
    a.call(dword_ptr(esp)).unwrap();
    a.call(esp).unwrap();
    a.call(0x18000u64).unwrap();
    a.ret().unwrap();
    a.ret_1(16).unwrap();
    let bytes = a.assemble(0x10000).unwrap();
    Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE)
        .into_iter()
        .collect()
}

#[test]
fn checked_scalar_forms_match_transactions_and_decline_without_effects() {
    let forms = forms();
    let mut successes = 0;
    let mut declines = 0;
    for address in [0x20000, 0x20003, 0x21fff] {
        for permission in [READ | WRITE, READ, WRITE, 0] {
            for count in [0, 1, 8, 9, 16, 17, 31, 32, 255] {
                for flags in [0x202, 0xad7, 0x302] {
                    let mut a = Memory::new(0x10000, 0x20000).unwrap();
                    let mut b = Memory::new(0x10000, 0x20000).unwrap();
                    for m in [&mut a, &mut b] {
                        m.protect(0x10000, 0x20000, READ | WRITE).unwrap();
                        m.write(0x20000, &vec![0x81; 0x4000]).unwrap();
                        m.protect(0x20000, PAGE_SIZE, permission).unwrap();
                        m.protect(0x22000, PAGE_SIZE, permission).unwrap();
                    }
                    for i in &forms {
                        let action = predecode(i).unwrap();
                        assert!(eligible(action), "{:?}", i.code());
                        let mut actual = CpuState {
                            regs: [
                                0x8011ff7f, count, 0x12345678, 0xffe0, address, 0, address, 0,
                            ],
                            eip: i.ip32(),
                            eflags: flags,
                            fs_base: 0x20000,
                            ..Default::default()
                        };
                        let mut expected = actual.clone();
                        let fast = Predecoded {
                            action,
                            next: i.next_ip32(),
                            exclusive: false,
                            register_only: false,
                            checked_scalar: true,
                        };
                        let reference = Predecoded {
                            checked_scalar: false,
                            ..fast
                        };
                        // An unsuccessful probe must have no visible effects.
                        let before = actual.clone();
                        let mut probe = actual.clone();
                        let mut probe_memory = Memory::new(0x10000, PAGE_SIZE).unwrap();
                        if execute(&fast, &mut probe, &mut probe_memory).is_none() {
                            assert_eq!(probe, before, "{:?}", i.code());
                            declines += 1;
                        } else {
                            successes += 1;
                        }
                        let got =
                            execute_decoded(&Decoded::Ready(fast), &mut actual, &mut a, 1, None);
                        let want = execute_decoded(
                            &Decoded::Ready(reference),
                            &mut expected,
                            &mut b,
                            1,
                            None,
                        );
                        assert_eq!(
                            got,
                            want,
                            "{:?}, count {count}, flags {flags}, permission {permission}",
                            i.code()
                        );
                        assert_eq!(
                            actual,
                            expected,
                            "{:?}, count {count}, flags {flags}, permission {permission}",
                            i.code()
                        );
                        assert_eq!(
                            a.read_data(0x20000, 0x4000).unwrap(),
                            b.read_data(0x20000, 0x4000).unwrap()
                        );
                    }
                }
            }
        }
    }
    assert!(successes > 0 && declines > 0);
}

#[test]
fn scalar_code_writes_and_locked_forms_keep_the_checked_owner_path() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(dword_ptr(esi), eax).unwrap();
    a.lock().add(dword_ptr(edi), 1).unwrap();
    a.inc(ebx).unwrap();
    let code = a.assemble(0x10000).unwrap();
    let mut memory = Memory::new(0x10000, 0x20000).unwrap();
    memory.protect(0x10000, 0x20000, READ | WRITE).unwrap();
    memory.write(0x10000, &code).unwrap();
    memory
        .protect(0x10000, PAGE_SIZE, READ | WRITE | EXEC)
        .unwrap();
    let mut state = CpuState {
        eip: 0x10000,
        eflags: 0x202,
        ..Default::default()
    };
    state.regs[0] = 17;
    state.regs[6] = 0x10080;
    state.regs[7] = 0x20000;
    let mut interpreter = Interpreter::new();
    let result = interpreter.run_block(&mut state, &mut memory, 3, 0, None).0;
    assert_eq!(result.units, 1);
    assert_eq!(state.regs[3], 0);
    assert_eq!(
        memory.read_data(0x10080, 4).unwrap().as_ref(),
        &17u32.to_le_bytes()
    );
    let Decoded::Ready(locked) = fetch(&mut memory, state.eip).decoded else {
        panic!()
    };
    assert!(locked.exclusive && !locked.checked_scalar);
    assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
    assert_eq!(
        memory.read_data(0x20000, 4).unwrap().as_ref(),
        &1u32.to_le_bytes()
    );
}
