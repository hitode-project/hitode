#![cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "macos"
))]
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, Prot, SharedRegion};
use adv_cpu::{Region, RunOutcome};
use iced_x86::code_asm::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
const CODE: u32 = 0x10000;
const STOP: u32 = CODE + 0x1000;
const DATA: u32 = 0x20000;
const SOURCE: u32 = 0x30000;
const STACK: u32 = 0x4ff00;

fn bytes(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(CODE as u64).unwrap()
}

fn fixture(code: &[u8], native: bool) -> (JitCpu, SharedRegion) {
    let mut cpu = JitCpu::new(STOP, false).unwrap();
    for at in [CODE, STOP] {
        cpu.map(at, 4096, Prot::RX).unwrap();
    }
    cpu.map(SOURCE, 8192, Prot::RW).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.write(CODE, code).unwrap();
    cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
    let shared = cpu.map_shared_audio(DATA, 8192).unwrap();
    let machine = cpu.executor_mut();
    machine.state.eip = CODE;
    machine.state.regs[4] = STACK;
    if native {
        machine.add_region(Region::new(CODE, code.len())).unwrap();
    }
    (cpu, shared)
}

fn run(cpu: &mut JitCpu) {
    assert_eq!(
        cpu.executor_mut().run(STOP, 1_000_000).unwrap(),
        RunOutcome::ReachedStop
    );
}

#[test]
fn alias_survives_protect_partial_unmap_address_reuse_and_machine_destruction() {
    let (mut cpu, old) = fixture(&[0xc3], false);
    assert_eq!(old.guest_base, DATA);
    assert_eq!(old.len, 8192);
    cpu.write(DATA, &[0x12, 0x34]).unwrap();
    cpu.write(DATA + 4096, &[0x56]).unwrap();
    assert!(cpu.executor().memory().read(DATA, 1).is_err());
    cpu.protect(DATA, 8192, Prot(0)).unwrap();
    assert!(!cpu.accessible(DATA, 1, Prot::READ));
    assert_eq!(old.read_alias.load_u8(0), 0x12);
    cpu.protect(DATA, 8192, Prot::RW).unwrap();
    cpu.write(DATA + 1, &[0x91]).unwrap();
    cpu.unmap(DATA + 4096, 4096).unwrap();
    assert!(cpu.read(DATA + 4096, &mut [0; 1]).is_err());
    cpu.map(DATA + 4096, 4096, Prot::RW).unwrap();
    cpu.write(DATA + 4096, &[0xa9]).unwrap();
    assert_eq!(old.read_alias.load_u8(4096), 0x56);
    cpu.unmap(DATA, 4096).unwrap();
    let new = cpu.map_shared_audio(DATA, 4096).unwrap();
    cpu.write(DATA, &[0xaa]).unwrap();
    assert_eq!(new.read_alias.load_u8(0), 0xaa);
    assert_eq!(old.read_alias.load_u8(0), 0x12);
    assert_eq!(old.read_alias.load_u8(1), 0x91);
    drop(cpu);
    assert_eq!(new.read_alias.load_u8(0), 0xaa);
    assert_eq!(old.read_alias.load_u8(4096), 0x56);
}

#[test]
fn pcm_in_a_partial_host_page_preserves_ordinary_neighbours_and_permissions() {
    let mut cpu = JitCpu::new(STOP, false).unwrap();
    // All four guest pages share one host page on a 16 KiB host. PCM remains
    // an overlay there, so mapping/reusing it cannot relax or destroy neighbours.
    cpu.map(DATA, 16384, Prot::RW).unwrap();
    let pattern: Vec<_> = (0..16384).map(|i| (i / 4096 + 0x31) as u8).collect();
    cpu.write(DATA, &pattern).unwrap();
    cpu.protect(DATA, 4096, Prot::RX).unwrap();
    cpu.protect(DATA + 8192, 4096, Prot::READ).unwrap();
    cpu.protect(DATA + 12288, 4096, Prot(0)).unwrap();
    cpu.unmap(DATA + 4096, 4096).unwrap();

    assert!(cpu.map_shared_audio(DATA + 4096, 0).is_err());
    assert!(cpu.map_shared_audio(DATA + 4097, 4096).is_err());
    assert!(cpu.map_shared_audio(DATA + 4096, 4095).is_err());
    assert!(cpu.map_shared_audio(DATA + 4096, 8192).is_err());
    let old = cpu.map_shared_audio(DATA + 4096, 4096).unwrap();
    assert!(cpu.shared_bytes(DATA + 4096, 1).is_none());
    assert_eq!(old.read_alias.len(), 4096);
    assert_eq!(old.read_alias.load_u8(0), 0);
    assert_eq!(old.read_alias.load_u8(4095), 0);
    cpu.write(DATA + 4096, &[0x91]).unwrap();
    cpu.write(DATA + 8191, &[0x37]).unwrap();
    cpu.protect(DATA + 4096, 4096, Prot(0)).unwrap();
    assert!(!cpu.accessible(DATA + 4096, 1, Prot::READ));
    assert_eq!(old.read_alias.load_u8(0), 0x91);

    cpu.unmap(DATA + 4096, 4096).unwrap();
    let new = cpu.map_shared_audio(DATA + 4096, 4096).unwrap();
    assert_eq!(new.read_alias.load_u8(0), 0);
    cpu.write(DATA + 4096, &[0xa5]).unwrap();
    assert_eq!(old.read_alias.load_u8(0), 0x91);
    cpu.unmap(DATA + 4096, 4096).unwrap();
    cpu.map(DATA + 4096, 4096, Prot::RW).unwrap();
    let mut zeroed = [0xa5; 4096];
    cpu.read(DATA + 4096, &mut zeroed).unwrap();
    assert_eq!(zeroed, [0; 4096]);
    cpu.write(DATA + 4096, &[0xee; 4096]).unwrap();

    for page in [0, 2, 3] {
        let mut actual = [0; 4096];
        cpu.read(DATA + page * 4096, &mut actual).unwrap();
        assert_eq!(actual, [page as u8 + 0x31; 4096]);
    }
    assert!(cpu.accessible(DATA, 4096, Prot::RX));
    assert!(!cpu.accessible(DATA, 4096, Prot::WRITE));
    assert!(cpu.accessible(DATA + 8192, 4096, Prot::READ));
    assert!(!cpu.accessible(DATA + 8192, 4096, Prot::WRITE));
    assert!(!cpu.accessible(DATA + 12288, 4096, Prot::READ));
    assert!(!cpu.accessible(DATA + 12288, 4096, Prot::WRITE));
    drop(cpu);
    assert_eq!(old.read_alias.load_u8(0), 0x91);
    assert_eq!(old.read_alias.load_u8(4095), 0x37);
    assert_eq!(new.read_alias.load_u8(0), 0xa5);
}

#[test]
fn native_integer_mmx_and_sse_use_atomic_bytes_while_x87_keeps_its_helper() {
    let code = bytes(|a| {
        a.mov(dword_ptr(DATA as i32), 0x1234_5678).unwrap();
        a.mov(eax, dword_ptr(DATA as i32)).unwrap();
        a.movq(qword_ptr(DATA as i32 + 8), mm0).unwrap();
        a.movq(mm1, qword_ptr(DATA as i32 + 8)).unwrap();
        a.movdqu(ptr(DATA as i32 + 32), xmm0).unwrap();
        a.movdqu(xmm1, ptr(DATA as i32 + 32)).unwrap();
        a.emms().unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(DATA as i32 + 64)).unwrap();
        a.ret().unwrap();
    });
    for native in [false, true] {
        let (mut cpu, shared) = fixture(&code, native);
        cpu.executor_mut().state.mmx[0] = 0x8123_4567_89ab_cdef;
        cpu.executor_mut().state.mmx_mode = 1;
        cpu.executor_mut().state.xmm[0] = [0xa5; 16];
        run(&mut cpu);
        let machine = cpu.executor();
        assert_eq!(machine.state.regs[0], 0x1234_5678);
        assert_eq!(machine.state.mmx[1], 0x8123_4567_89ab_cdef);
        assert_eq!(machine.state.xmm[1], [0xa5; 16]);
        if native {
            assert!(machine.stats.interpreter_instructions < 7);
        } else {
            assert!(machine.stats.interpreter_instructions >= 7);
        }
        let mut data = [0; 72];
        shared.read_alias.copy_into(0, &mut data);
        assert_eq!(&data[..4], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&data[8..16], &0x8123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(&data[32..48], &[0xa5; 16]);
        assert_eq!(&data[64..72], &1.0f64.to_le_bytes());
    }
}

#[test]
fn native_rep_copy_fill_and_comparison_use_byte_atomic_storage() {
    let copy = bytes(|a| {
        a.rep().movsd().unwrap();
        a.ret().unwrap();
    });
    let fill = bytes(|a| {
        a.rep().stosd().unwrap();
        a.ret().unwrap();
    });
    let compare = bytes(|a| {
        a.repe().cmpsb().unwrap();
        a.ret().unwrap();
    });
    for (code, kind) in [(&copy, 0), (&fill, 1), (&compare, 2)] {
        let (mut cpu, shared) = fixture(code, true);
        let source: Vec<_> = (0..8192).map(|i| (i % 251) as u8).collect();
        cpu.write(SOURCE, &source).unwrap();
        cpu.executor_mut().state.regs[0] = 0x7654_3210;
        cpu.executor_mut().state.regs[1] = if kind == 2 { 8192 } else { 2048 };
        cpu.executor_mut().state.regs[6] = SOURCE;
        cpu.executor_mut().state.regs[7] = DATA;
        if kind == 2 {
            cpu.write(DATA, &source).unwrap();
        }
        run(&mut cpu);
        assert_eq!(cpu.executor().state.regs[1], 0);
        assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
        let mut actual = vec![0; 8192];
        shared.read_alias.copy_into(0, &mut actual);
        if kind == 1 {
            assert_eq!(actual, 0x7654_3210u32.to_le_bytes().repeat(2048));
        } else {
            assert_eq!(actual, source);
        }
    }
}

#[test]
fn native_element_widths_cross_audio_and_ordinary_page_boundaries() {
    for width in [1, 2, 4, 8, 16] {
        for address in [DATA - 1, DATA + 4095] {
            let code = bytes(|a| {
                match width {
                    1 => {
                        a.mov(byte_ptr(address as i32), al).unwrap();
                        a.mov(cl, byte_ptr(address as i32)).unwrap();
                    }
                    2 => {
                        a.mov(word_ptr(address as i32), ax).unwrap();
                        a.mov(cx, word_ptr(address as i32)).unwrap();
                    }
                    4 => {
                        a.mov(dword_ptr(address as i32), eax).unwrap();
                        a.mov(ecx, dword_ptr(address as i32)).unwrap();
                    }
                    8 => {
                        a.movq(qword_ptr(address as i32), mm0).unwrap();
                        a.movq(mm1, qword_ptr(address as i32)).unwrap();
                    }
                    _ => {
                        a.movdqu(ptr(address as i32), xmm0).unwrap();
                        a.movdqu(xmm1, ptr(address as i32)).unwrap();
                    }
                }
                a.ret().unwrap();
            });
            let (mut cpu, shared) = fixture(&code, true);
            cpu.map(DATA - 4096, 4096, Prot::RW).unwrap();
            let state = &mut cpu.executor_mut().state;
            state.regs[0] = 0x8765_4321;
            state.mmx[0] = 0xfedc_ba98_7654_3210;
            state.mmx_mode = u32::from(width == 8);
            state.xmm[0] = std::array::from_fn(|i| (i * 13 + 7) as u8);
            run(&mut cpu);
            let state = &cpu.executor().state;
            let expected = match width {
                1 | 2 | 4 => 0x8765_4321u32.to_le_bytes().to_vec(),
                8 => 0xfedc_ba98_7654_3210u64.to_le_bytes().to_vec(),
                _ => std::array::from_fn::<_, 16, _>(|i| (i * 13 + 7) as u8).to_vec(),
            };
            match width {
                1 => assert_eq!(state.regs[1], 0x21),
                2 => assert_eq!(state.regs[1], 0x4321),
                4 => assert_eq!(state.regs[1], 0x8765_4321),
                8 => assert_eq!(state.mmx[1], 0xfedc_ba98_7654_3210),
                _ => assert_eq!(state.xmm[1], expected.as_slice()),
            }
            let mut actual = vec![0; width];
            cpu.read(address, &mut actual).unwrap();
            assert_eq!(actual, expected[..width]);
            let first_audio = address.max(DATA);
            for at in first_audio..address + width as u32 {
                assert_eq!(
                    shared.read_alias.load_u8((at - DATA) as usize),
                    expected[(at - address) as usize]
                );
            }
            assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
        }
    }
}

#[test]
fn protected_read_modify_write_and_vector_store_preserve_instruction_state() {
    for vector in [false, true] {
        let code = bytes(|a| {
            if vector {
                a.movdqu(ptr(DATA as i32 + 4094), xmm0).unwrap();
            } else {
                a.add(dword_ptr(DATA as i32 + 4094), eax).unwrap();
            }
            a.ret().unwrap();
        });
        let (mut cpu, shared) = fixture(&code, true);
        cpu.write(DATA + 4094, &[0x7f; 16]).unwrap();
        cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
        cpu.executor_mut().state.eflags = 0x8d7;
        cpu.executor_mut().state.regs[0] = 1;
        cpu.executor_mut().state.xmm[0] = [0xa5; 16];
        let before = cpu.executor().state.clone();
        assert_eq!(
            cpu.executor_mut().run(STOP, 100).unwrap(),
            RunOutcome::WriteFault {
                pc: CODE,
                address: DATA + 4094
            }
        );
        let after = &cpu.executor().state;
        assert_eq!(after.regs, before.regs);
        assert_eq!(after.eflags, before.eflags);
        assert_eq!(after.xmm, before.xmm);
        assert_eq!(after.eip, CODE);
        let mut actual = [0; 16];
        shared.read_alias.copy_into(4094, &mut actual);
        assert_eq!(actual, [0x7f; 16]);
        cpu.protect(DATA + 4096, 4096, Prot::RW).unwrap();
        run(&mut cpu);
        shared.read_alias.copy_into(4094, &mut actual);
        if vector {
            assert_eq!(actual, [0xa5; 16]);
        } else {
            assert_eq!(&actual[..4], &0x7f7f_7f80u32.to_le_bytes());
        }
        assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
    }
}

#[test]
fn executable_audio_store_uses_the_self_modification_guard() {
    let code = bytes(|a| {
        a.mov(dword_ptr(DATA as i32), 0x1234_5678).unwrap();
        a.ret().unwrap();
    });
    let (mut cpu, shared) = fixture(&code, true);
    cpu.protect(DATA, 8192, Prot::RWX).unwrap();
    run(&mut cpu);
    let mut actual = [0; 4];
    shared.read_alias.copy_into(0, &mut actual);
    assert_eq!(actual, 0x1234_5678u32.to_le_bytes());
    assert!(cpu.executor().stats.interpreter_instructions > 0);
}

#[test]
fn audio_rep_comparisons_match_interpreter_for_width_direction_and_stop_condition() {
    for width in [1, 2, 4] {
        for backwards in [false, true] {
            for scan in [false, true] {
                for equal_repeat in [false, true] {
                    let mut code = vec![if equal_repeat { 0xf3 } else { 0xf2 }];
                    if width == 2 {
                        code.push(0x66);
                    }
                    code.push(match (scan, width) {
                        (false, 1) => 0xa6,
                        (false, _) => 0xa7,
                        (true, 1) => 0xae,
                        (true, _) => 0xaf,
                    });
                    code.push(0xc3);
                    let mut outputs = vec![];
                    for native in [false, true] {
                        let (mut cpu, _) = fixture(&code, native);
                        let mut data = vec![0; 8192];
                        for chunk in data.chunks_exact_mut(width) {
                            chunk.copy_from_slice(&0x8765_4321u32.to_le_bytes()[..width]);
                        }
                        cpu.write(SOURCE, &data).unwrap();
                        if !equal_repeat {
                            for byte in data.iter_mut().step_by(width) {
                                *byte ^= 0x80;
                            }
                        }
                        let first = if backwards { DATA + 4093 } else { DATA + 3 };
                        let stop = if backwards {
                            first - 17 * width as u32
                        } else {
                            first + 17 * width as u32
                        };
                        cpu.write(DATA, &data).unwrap();
                        // Both operands start at the same unaligned byte. Copy
                        // the selected comparison value there before the stop.
                        let value = if equal_repeat {
                            0x8765_4321u32
                        } else {
                            0x8765_43a1
                        };
                        for element in 0..33 {
                            let address = if backwards {
                                first - element * width as u32
                            } else {
                                first + element * width as u32
                            };
                            cpu.write(address, &value.to_le_bytes()[..width]).unwrap();
                            if !scan {
                                cpu.write(
                                    SOURCE + (address - DATA),
                                    &0x8765_4321u32.to_le_bytes()[..width],
                                )
                                .unwrap();
                            }
                        }
                        let stopping = if equal_repeat {
                            0x8765_43a1u32
                        } else {
                            0x8765_4321
                        };
                        cpu.write(stop, &stopping.to_le_bytes()[..width]).unwrap();
                        let state = &mut cpu.executor_mut().state;
                        state.regs[0] = 0x8765_4321;
                        state.regs[1] = 33;
                        state.regs[6] = SOURCE + (first - DATA);
                        state.regs[7] = first;
                        state.eflags = 0x8d7 | if backwards { 0x400 } else { 0 };
                        run(&mut cpu);
                        assert_eq!(cpu.executor().state.regs[1], 15);
                        if native {
                            assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
                        }
                        outputs.push((cpu.executor().state.regs, cpu.executor().state.eflags));
                    }
                    assert_eq!(
                        outputs[0], outputs[1],
                        "width={width}, DF={backwards}, SCAS={scan}, REPE={equal_repeat}"
                    );
                }
            }
        }
    }
}

#[test]
fn protected_cross_page_native_load_preserves_integer_and_vector_destinations() {
    for vector in [false, true] {
        let code = bytes(|a| {
            if vector {
                a.movdqu(xmm0, ptr(DATA as i32 + 4094)).unwrap();
            } else {
                a.mov(eax, dword_ptr(DATA as i32 + 4094)).unwrap();
            }
            a.ret().unwrap();
        });
        let (mut cpu, _) = fixture(&code, true);
        cpu.write(DATA + 4094, &[0xa5; 16]).unwrap();
        cpu.protect(DATA + 4096, 4096, Prot(0)).unwrap();
        cpu.executor_mut().state.regs[0] = 0x1234_5678;
        cpu.executor_mut().state.xmm[0] = [0x12; 16];
        let before = cpu.executor().state.clone();
        assert_eq!(
            cpu.executor_mut().run(STOP, 100).unwrap(),
            RunOutcome::ReadFault {
                pc: CODE,
                address: DATA + 4094
            }
        );
        assert_eq!(cpu.executor().state.regs, before.regs);
        assert_eq!(cpu.executor().state.xmm, before.xmm);
        cpu.protect(DATA + 4096, 4096, Prot::RW).unwrap();
        run(&mut cpu);
        if vector {
            assert_eq!(cpu.executor().state.xmm[0], [0xa5; 16]);
        } else {
            assert_eq!(cpu.executor().state.regs[0], 0xa5a5_a5a5);
        }
        assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
    }
}

#[test]
fn shared_rep_chunks_preserve_budget_and_pending_interrupt_then_resume() {
    for compare in [false, true] {
        let code = if compare {
            vec![0xf3, 0xa6, 0xc3]
        } else {
            vec![0xf3, 0xa4, 0xc3]
        };
        let (mut cpu, shared) = fixture(&code, true);
        cpu.write(SOURCE, &[0x63; 8192]).unwrap();
        if compare {
            cpu.write(DATA, &[0x63; 8192]).unwrap();
        }
        let interrupt = Arc::new(AtomicBool::new(false));
        cpu.executor_mut()
            .set_host_interrupt_flag(interrupt.clone());
        cpu.executor_mut().state.regs[1] = 8192;
        cpu.executor_mut().state.regs[6] = SOURCE;
        cpu.executor_mut().state.regs[7] = DATA;
        assert_eq!(cpu.executor_mut().run(STOP, 1).unwrap(), RunOutcome::Budget);
        assert_eq!(cpu.executor().state.regs[1], 4096);
        assert_eq!(cpu.executor().state.regs[6], SOURCE + 4096);
        assert_eq!(cpu.executor().state.regs[7], DATA + 4096);
        assert_eq!(cpu.executor().state.eip, CODE);
        let before = cpu.executor().state.clone();
        interrupt.store(true, Ordering::Release);
        assert_eq!(
            cpu.executor_mut().run(STOP, 100).unwrap(),
            RunOutcome::Interrupted
        );
        assert_eq!(cpu.executor().state.regs, before.regs);
        assert_eq!(cpu.executor().state.eflags, before.eflags);
        interrupt.store(false, Ordering::Release);
        run(&mut cpu);
        assert_eq!(cpu.executor().state.regs[1], 0);
        assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
        let mut actual = [0; 8192];
        shared.read_alias.copy_into(0, &mut actual);
        assert_eq!(actual, [0x63; 8192]);
    }
}

#[test]
fn api_overlap_copy_and_mixed_page_stores_keep_their_exact_bytes() {
    let (mut cpu, shared) = fixture(&[0xc3], false);
    let source: Vec<_> = (0..8192).map(|i| (i % 251) as u8).collect();
    cpu.write(DATA, &source).unwrap();
    cpu.copy_memory(DATA, DATA + 3, 7000).unwrap();
    let mut expected = source.clone();
    expected.copy_within(0..7000, 3);
    let mut actual = vec![0; 8192];
    shared.read_alias.copy_into(0, &mut actual);
    assert_eq!(actual, expected);
    cpu.map(DATA - 4096, 4096, Prot::RW).unwrap();
    cpu.write(DATA - 2, &[9, 8, 7, 6]).unwrap();
    let mut mixed = [0; 4];
    cpu.read(DATA - 2, &mut mixed).unwrap();
    assert_eq!(mixed, [9, 8, 7, 6]);
    assert_eq!(shared.read_alias.load_u8(0), 7);
}

#[test]
fn protected_cross_page_store_faults_without_partial_commit_then_restarts() {
    let code = bytes(|a| {
        a.mov(dword_ptr(DATA as i32 + 4094), 0x1234_5678).unwrap();
        a.ret().unwrap();
    });
    let (mut cpu, shared) = fixture(&code, true);
    cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        cpu.executor_mut().run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(shared.read_alias.load_u8(4094), 0);
    assert_eq!(shared.read_alias.load_u8(4095), 0);
    cpu.protect(DATA + 4096, 4096, Prot::RW).unwrap();
    run(&mut cpu);
    let mut actual = [0; 4];
    shared.read_alias.copy_into(4094, &mut actual);
    assert_eq!(actual, 0x1234_5678u32.to_le_bytes());
}

#[path = "support/callback_reader.rs"]
mod callback_reader;
use callback_reader::CallbackReader;

#[test]
fn concurrent_callback_reads_never_observe_invalidated_or_reused_guest_storage() {
    let (mut cpu, shared) = fixture(&[0xc3], false);
    cpu.write(DATA, &[0x11; 8192]).unwrap();
    let alias = shared.read_alias.clone();
    let mut reader = CallbackReader::new(move |reads| {
        let mut bytes = [0; 128];
        alias.copy_into(reads % 8000, &mut bytes);
        assert!(bytes.iter().all(|&b| b == 0x11 || b == 0xee));
    });
    reader.checkpoint();
    for _ in 0..64 {
        cpu.write(DATA, &[0xee; 8192]).unwrap();
        reader.checkpoint();
        cpu.write(DATA, &[0x11; 8192]).unwrap();
    }
    cpu.unmap(DATA, 8192).unwrap();
    cpu.map(DATA, 8192, Prot::RW).unwrap();
    cpu.write(DATA, &[0xa5; 8192]).unwrap();
    reader.checkpoint();
    drop(cpu);
    reader.checkpoint();
    assert!(reader.finish() > 0);
    assert_eq!(shared.read_alias.load_u8(0), 0x11);
}

#[test]
fn concurrent_callback_reads_accept_byte_mixtures_during_native_simd_stores() {
    let code = bytes(|a| {
        let mut again = a.create_label();
        a.set_label(&mut again).unwrap();
        a.movdqu(ptr(edi), xmm0).unwrap();
        a.add(edi, 16).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(again).unwrap();
        a.ret().unwrap();
    });
    let (mut cpu, shared) = fixture(&code, true);
    cpu.write(DATA, &[0x11; 8192]).unwrap();
    let alias = shared.read_alias.clone();
    let mut reader = CallbackReader::new(move |reads| {
        let mut data = [0; 128];
        alias.copy_into(reads % 8000, &mut data);
        assert!(data.iter().all(|&byte| byte == 0x11 || byte == 0xee));
    });
    reader.checkpoint();
    for _ in 0..64 {
        for pattern in [0xee, 0x11] {
            let state = &mut cpu.executor_mut().state;
            state.eip = CODE;
            state.regs[1] = 512;
            state.regs[4] = STACK;
            state.regs[7] = DATA;
            state.xmm[0] = [pattern; 16];
            run(&mut cpu);
            reader.checkpoint();
        }
    }
    assert!(reader.finish() > 0);
    assert_eq!(cpu.executor().stats.interpreter_instructions, 0);
    let mut actual = [0; 8192];
    shared.read_alias.copy_into(0, &mut actual);
    assert_eq!(actual, [0x11; 8192]);
}
