use super::*;
use iced_x86::code_asm::*;

fn forms() -> Vec<Instruction> {
    let mut a = CodeAssembler::new(32).unwrap();
    a.movdqu(xmm0, xmm1).unwrap();
    a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
    a.movdqu(xmmword_ptr(esi), xmm0).unwrap();
    a.movdqu(xmm0, xmmword_ptr(bx + si + 0x20).fs()).unwrap();
    a.movaps(xmm0, xmmword_ptr(esi)).unwrap();
    a.movaps(xmmword_ptr(esi), xmm0).unwrap();
    a.movq(mm0, mm1).unwrap();
    a.movq(mm0, qword_ptr(esi)).unwrap();
    a.movq(qword_ptr(esi), mm0).unwrap();
    a.movq(xmm0, xmm1).unwrap();
    a.movq(xmm0, qword_ptr(esi)).unwrap();
    a.movq(qword_ptr(esi), xmm0).unwrap();
    a.movd(xmm0, eax).unwrap();
    a.movd(eax, xmm0).unwrap();
    a.movd(mm0, eax).unwrap();
    a.movd(eax, mm0).unwrap();
    a.movd(xmm0, dword_ptr(esi)).unwrap();
    a.movd(dword_ptr(esi), xmm0).unwrap();
    a.movd(mm0, dword_ptr(esi)).unwrap();
    a.movd(dword_ptr(esi), mm0).unwrap();
    a.movntq(qword_ptr(esi), mm0).unwrap();
    a.movntdq(xmmword_ptr(esi), xmm0).unwrap();
    a.paddb(xmm0, xmm1).unwrap();
    a.paddb(xmm0, xmmword_ptr(esi)).unwrap();
    a.paddb(mm0, mm1).unwrap();
    a.paddb(mm0, qword_ptr(esi)).unwrap();
    a.paddusb(xmm0, xmm1).unwrap();
    a.psubsw(mm0, mm1).unwrap();
    a.pcmpeqd(xmm0, xmm1).unwrap();
    a.pcmpgtw(mm0, mm1).unwrap();
    a.pmullw(xmm0, xmm1).unwrap();
    a.pmulhw(mm0, mm1).unwrap();
    a.pxor(xmm0, xmm1).unwrap();
    a.pandn(mm0, mm1).unwrap();
    a.pxor(xmm0, xmm0).unwrap();
    a.pxor(mm0, mm0).unwrap();
    for count in [0, 1, 15, 16, 31, 32, 63, 64, 255] {
        a.psrlw(xmm0, count).unwrap();
        a.psllq(mm0, count).unwrap();
        a.psrad(xmm0, count).unwrap();
    }
    a.psrlw(xmm0, xmm1).unwrap();
    a.psrlw(xmm0, xmmword_ptr(esi)).unwrap();
    a.psrlw(mm0, mm1).unwrap();
    a.psrlw(mm0, qword_ptr(esi)).unwrap();
    let bytes = a.assemble(0x10000).unwrap();
    Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE)
        .into_iter()
        .collect()
}

#[test]
fn decoded_simd_matches_transactional_effects_and_fault_priority() {
    let forms = forms();
    for address in [0x20000, 0x20003, 0x21ff8] {
        for protection in [READ | WRITE, READ, 0] {
            for mode in [0, 1, 2, 0x405] {
                for pending in [false, true] {
                    let mut a = Memory::new(0x10000, 0x20000).unwrap();
                    let mut b = Memory::new(0x10000, 0x20000).unwrap();
                    for m in [&mut a, &mut b] {
                        m.protect(0x10000, 0x20000, READ | WRITE).unwrap();
                        let bytes: Vec<_> = (0..0x4000).map(|n| (n * 17 + 3) as u8).collect();
                        m.write(0x20000, &bytes).unwrap();
                        m.protect(0x22000, PAGE_SIZE, protection).unwrap();
                    }
                    for i in &forms {
                        let plan = Plan::decode(i).expect("authored fast SIMD form");
                        let mut actual = CpuState {
                            eip: i.ip32(),
                            eflags: 0xad7,
                            fs_base: 0x20000,
                            mmx_mode: mode,
                            ..Default::default()
                        };
                        actual.regs[0] = 0xf0a5c377;
                        actual.regs[3] = 0xffe0;
                        actual.regs[6] = address;
                        actual.x87.status = 0x3800 | if pending { 0x80 } else { 0 };
                        for n in 0..8 {
                            actual.mmx[n] = 0xfedc_ba98_8765_4321u64.wrapping_mul(n as u64 + 1);
                            actual.x87.regs[n] =
                                crate::X87Register::from_parts(n as u64 * 71, 0x3fff);
                            actual.xmm[n] = std::array::from_fn(|k| (k * 19 + n * 33) as u8);
                        }
                        let mut expected = actual.clone();
                        let got = plan.execute(&mut actual, &mut a);
                        let want = crate::floating::execute(
                            i,
                            crate::floating::Operation::Simd,
                            None,
                            &mut expected,
                            &mut b,
                            true,
                        );
                        assert_eq!(got, want, "{:?}, mode {mode}, pending {pending}", i.code());
                        assert_eq!(
                            actual,
                            expected,
                            "{:?}, mode {mode}, pending {pending}",
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
}

#[test]
fn decoded_simd_keeps_shared_stores_code_invalidation_and_tf() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.movdqu(xmmword_ptr(esi), xmm0).unwrap();
    a.paddb(xmm0, xmm1).unwrap();
    let code = a.assemble(0x10000).unwrap();
    for (destination, protection, tf) in [
        (0x20000, READ | WRITE, false),
        (0x10080, READ | WRITE | EXEC, false),
        (0x20000, READ | WRITE, true),
    ] {
        let mut memory = Memory::reserve(0x10000, 0x20000).unwrap();
        memory.map(0x10000, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
        let shared = memory.map_shared_audio(0x20000, PAGE_SIZE).unwrap();
        memory.protect(0x20000, PAGE_SIZE, READ | WRITE).unwrap();
        memory
            .protect(destination & !4095, PAGE_SIZE, protection)
            .unwrap();
        memory.write(0x10000, &code).unwrap();
        let mut s = CpuState {
            eip: 0x10000,
            eflags: 0x202 | if tf { 0x100 } else { 0 },
            ..Default::default()
        };
        s.regs[6] = destination;
        s.xmm[0] = [0x31; 16];
        s.xmm[1] = [1; 16];
        let mut interpreter = Interpreter::new();
        let result = interpreter.run_block(&mut s, &mut memory, 2, 0, None).0;
        if tf || destination == 0x10080 {
            assert_eq!(result.units, 1);
            assert_eq!(s.xmm[0], [0x31; 16]);
        } else {
            assert_eq!(result.units, 2);
            assert_eq!(s.xmm[0], [0x32; 16]);
        }
        assert_eq!(
            result.step,
            if tf {
                Step::Trap {
                    pc: 0x10000,
                    vector: 1,
                }
            } else {
                Step::Executed
            }
        );
        assert_eq!(
            memory.read_data(destination, 16).unwrap().as_ref(),
            &[0x31; 16]
        );
        drop(shared);
    }
}
