use adv_cpu::backend::{FilePage, Guest, Prot};
use adv_cpu::{JitCpu, Region, RunOutcome};
use iced_x86::code_asm::*;
use std::cell::Cell;
use std::rc::Rc;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const SOURCE: u32 = 0x20000;
const DATA: u32 = 0x30000;
const MIRROR: u32 = 0x40000;
const STACK: u32 = 0x5ff00;

fn program(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    build(&mut assembler);
    assembler.assemble(CODE.into()).unwrap()
}

fn fixture(code: &[u8], native: bool) -> JitCpu {
    let mut cpu = JitCpu::new(STOP, false).unwrap();
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.map(STOP, 4096, Prot::RX).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.write(CODE, code).unwrap();
    cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
    cpu.executor_mut().state.eip = CODE;
    cpu.executor_mut().state.regs[4] = STACK;
    if native {
        cpu.executor_mut()
            .add_region(Region::new(CODE, code.len()))
            .unwrap();
    }
    cpu
}

fn finish(cpu: &mut JitCpu) {
    assert_eq!(
        cpu.executor_mut().run(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(cpu.stats().interpreter_instructions, 0);
}

#[test]
fn scalar_mmx_and_simd_cross_page_reads_writes_and_cow_aliases_stay_native() {
    for width in [1, 2, 4, 8, 16] {
        for cow in [false, true] {
            let code = program(|a| {
                let src = SOURCE as i32 + 4094;
                let dst = DATA as i32 + 4094;
                match width {
                    1 => {
                        a.mov(al, byte_ptr(src)).unwrap();
                        a.mov(byte_ptr(dst), al).unwrap();
                        a.mov(cl, byte_ptr(dst)).unwrap();
                    }
                    2 => {
                        a.mov(ax, word_ptr(src)).unwrap();
                        a.mov(word_ptr(dst), ax).unwrap();
                        a.mov(cx, word_ptr(dst)).unwrap();
                    }
                    4 => {
                        a.mov(eax, dword_ptr(src)).unwrap();
                        a.mov(dword_ptr(dst), eax).unwrap();
                        a.mov(ecx, dword_ptr(dst)).unwrap();
                    }
                    8 => {
                        a.movq(mm0, qword_ptr(src)).unwrap();
                        a.movq(qword_ptr(dst), mm0).unwrap();
                        a.movq(mm1, qword_ptr(dst)).unwrap();
                    }
                    _ => {
                        a.movdqu(xmm0, ptr(src)).unwrap();
                        a.movdqu(ptr(dst), xmm0).unwrap();
                        a.movdqu(xmm1, ptr(dst)).unwrap();
                    }
                }
                a.ret().unwrap();
            });
            let mut cpu = fixture(&code, true);
            let source: Vec<_> = (0..2)
                .map(|page| {
                    FilePage::new(std::array::from_fn(|i| {
                        ((page * 4096 + i) * 17 % 251) as u8
                    }))
                })
                .collect();
            let destination: Vec<_> = (0..2).map(|_| FilePage::new([0; 4096])).collect();
            cpu.map_file_pages(SOURCE, &source, Prot::READ, false)
                .unwrap();
            cpu.map_file_pages(DATA, &destination, Prot::RW, cow)
                .unwrap();
            cpu.map_file_pages(MIRROR, &destination, Prot::READ, false)
                .unwrap();
            finish(&mut cpu);
            let expected: Vec<_> = (4094..4094 + width).map(|i| (i * 17 % 251) as u8).collect();
            let mut actual = vec![0; width];
            cpu.read(DATA + 4094, &mut actual).unwrap();
            assert_eq!(actual, expected);
            cpu.read(MIRROR + 4094, &mut actual).unwrap();
            assert_eq!(
                actual,
                if cow {
                    vec![0; width]
                } else {
                    expected.clone()
                }
            );
            assert_eq!(cpu.file_page_copy_on_write(DATA), Some(false));
            assert_eq!(
                cpu.file_page_copy_on_write(DATA + 4096),
                Some(cow && width <= 2)
            );
            if width <= 4 {
                let mut value = [0; 4];
                value[..width].copy_from_slice(&expected);
                assert_eq!(cpu.executor().state.regs[1], u32::from_le_bytes(value));
            } else if width == 8 {
                assert_eq!(
                    cpu.executor().state.mmx[1],
                    u64::from_le_bytes(expected.try_into().unwrap())
                );
            } else {
                assert_eq!(cpu.executor().state.xmm[1], expected.as_slice());
            }
        }
    }
}

#[test]
fn lazy_read_failure_is_delivered_once_before_retry_and_preserves_destination() {
    let code = program(|a| {
        a.movdqu(xmm0, ptr(SOURCE as i32 + 4094)).unwrap();
        a.ret().unwrap();
    });
    let mut cpu = fixture(&code, true);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let pages = [
        FilePage::new([0x17; 4096]),
        FilePage::lazy(move || {
            observed.set(observed.get() + 1);
            if observed.get() == 1 {
                Err("transient read error".into())
            } else {
                Ok([0x31; 4096])
            }
        }),
    ];
    cpu.map_file_pages(SOURCE, &pages, Prot::READ, false)
        .unwrap();
    cpu.executor_mut().state.xmm[0] = [0xa5; 16];
    let before = cpu.executor().state.clone();
    assert_eq!(
        cpu.executor_mut().run(STOP, 100).unwrap(),
        RunOutcome::ReadFault {
            pc: CODE,
            address: SOURCE + 4094
        }
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(cpu.executor().state.regs, before.regs);
    assert_eq!(cpu.executor().state.xmm, before.xmm);
    assert_eq!(cpu.executor().state.eflags, before.eflags);
    finish(&mut cpu);
    assert_eq!(calls.get(), 2);
    assert_eq!(&cpu.executor().state.xmm[0][..2], &[0x17; 2]);
    assert_eq!(&cpu.executor().state.xmm[0][2..], &[0x31; 14]);
}

#[test]
fn lazy_store_failure_leaves_both_cow_pages_attached_and_no_byte_prefix() {
    let code = program(|a| {
        a.movdqu(ptr(DATA as i32 + 4094), xmm0).unwrap();
        a.ret().unwrap();
    });
    let mut cpu = fixture(&code, true);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let pages = [
        FilePage::new([0x17; 4096]),
        FilePage::lazy(move || {
            observed.set(observed.get() + 1);
            if observed.get() == 1 {
                Err("transient COW source error".into())
            } else {
                Ok([0x31; 4096])
            }
        }),
    ];
    cpu.map_file_pages(DATA, &pages, Prot::RW, true).unwrap();
    cpu.map_file_pages(MIRROR, &pages, Prot::READ, false)
        .unwrap();
    cpu.executor_mut().state.xmm[0] = [0x5a; 16];
    let before = cpu.executor().state.clone();
    assert_eq!(
        cpu.executor_mut().run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(cpu.executor().state.regs, before.regs);
    assert_eq!(cpu.executor().state.xmm, before.xmm);
    assert_eq!(cpu.executor().state.eflags, before.eflags);
    for at in [DATA, DATA + 4096] {
        assert_eq!(cpu.file_page_copy_on_write(at), Some(true));
    }
    assert_eq!(pages[0].snapshot().unwrap(), [0x17; 4096]);
    assert!(!pages[0].dirty());
    finish(&mut cpu);
    assert_eq!(calls.get(), 2);
    let mut actual = [0; 16];
    cpu.read(DATA + 4094, &mut actual).unwrap();
    assert_eq!(actual, [0x5a; 16]);
    cpu.read(MIRROR + 4094, &mut actual).unwrap();
    assert_eq!(&actual[..2], &[0x17; 2]);
    assert_eq!(&actual[2..], &[0x31; 14]);
}

#[test]
fn denied_rmw_keeps_input_flags_and_cow_then_restarts_natively() {
    let code = program(|a| {
        a.add(dword_ptr(DATA as i32 + 4094), eax).unwrap();
        a.ret().unwrap();
    });
    let mut cpu = fixture(&code, true);
    let pages = [FilePage::new([0x7f; 4096]), FilePage::new([0x7f; 4096])];
    cpu.map_file_pages(DATA, &pages, Prot::RW, true).unwrap();
    cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    cpu.executor_mut().state.regs[0] = 1;
    cpu.executor_mut().state.eflags = 0x8d7;
    let before = cpu.executor().state.clone();
    assert_eq!(
        cpu.executor_mut().run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(cpu.executor().state.regs, before.regs);
    assert_eq!(cpu.executor().state.eflags, before.eflags);
    for at in [DATA, DATA + 4096] {
        assert_eq!(cpu.file_page_copy_on_write(at), Some(true));
    }
    cpu.protect(DATA + 4096, 4096, Prot::RW).unwrap();
    finish(&mut cpu);
    let mut actual = [0; 4];
    cpu.read(DATA + 4094, &mut actual).unwrap();
    assert_eq!(actual, 0x7f7f_7f80u32.to_le_bytes());
    assert!(pages.iter().all(|page| !page.dirty()));
}

#[test]
fn rep_source_or_destination_io_fault_preserves_completed_prefix_without_replay() {
    for source_fails in [false, true] {
        let mut cpu = fixture(&[0xf3, 0xa5, 0xc3], true);
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let bad = FilePage::lazy(move || {
            observed.set(observed.get() + 1);
            if observed.get() == 1 {
                Err("second page IO failure".into())
            } else {
                Ok([if source_fails { 0x63 } else { 0 }; 4096])
            }
        });
        let src = [
            FilePage::new([0x63; 4096]),
            if source_fails {
                bad.clone()
            } else {
                FilePage::new([0x63; 4096])
            },
        ];
        let dst = [
            FilePage::new([0; 4096]),
            if source_fails {
                FilePage::new([0; 4096])
            } else {
                bad
            },
        ];
        cpu.map_file_pages(SOURCE, &src, Prot::READ, false).unwrap();
        cpu.map_file_pages(DATA, &dst, Prot::RW, true).unwrap();
        cpu.executor_mut().state.regs[1] = 2048;
        cpu.executor_mut().state.regs[6] = SOURCE;
        cpu.executor_mut().state.regs[7] = DATA;
        let expected = if source_fails {
            RunOutcome::ReadFault {
                pc: CODE,
                address: SOURCE + 4096,
            }
        } else {
            RunOutcome::WriteFault {
                pc: CODE,
                address: DATA + 4096,
            }
        };
        assert_eq!(cpu.executor_mut().run(STOP, 100).unwrap(), expected);
        assert_eq!(calls.get(), 1);
        assert_eq!(cpu.executor().state.regs[1], 1024);
        assert_eq!(cpu.executor().state.regs[6], SOURCE + 4096);
        assert_eq!(cpu.executor().state.regs[7], DATA + 4096);
        assert_eq!(cpu.file_page_copy_on_write(DATA + 4096), Some(true));
        assert_eq!(cpu.stats().bulk_bytes, 4096);
        finish(&mut cpu);
        assert_eq!(calls.get(), 2);
        let mut actual = [0; 8192];
        cpu.read(DATA, &mut actual).unwrap();
        assert_eq!(actual, [0x63; 8192]);
        assert_eq!(cpu.stats().bulk_bytes, 8192);
    }
}

#[test]
fn rep_comparison_io_fault_restores_original_flags_after_a_native_slice() {
    let mut cpu = fixture(&[0xf3, 0xa6, 0xc3], true);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let src = [FilePage::new([0x31; 4096]), FilePage::new([0x31; 4096])];
    let dst = [
        FilePage::new([0x31; 4096]),
        FilePage::lazy(move || {
            observed.set(observed.get() + 1);
            if observed.get() == 1 {
                Err("comparison IO failure".into())
            } else {
                Ok([0x31; 4096])
            }
        }),
    ];
    cpu.map_file_pages(SOURCE, &src, Prot::READ, false).unwrap();
    cpu.map_file_pages(DATA, &dst, Prot::READ, true).unwrap();
    let state = &mut cpu.executor_mut().state;
    state.regs[1] = 8192;
    state.regs[6] = SOURCE;
    state.regs[7] = DATA;
    state.eflags = 0x8d7;
    assert_eq!(cpu.executor_mut().run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_ne!(cpu.executor().state.eflags, 0x8d7);
    assert_eq!(
        cpu.executor_mut().run(STOP, 100).unwrap(),
        RunOutcome::ReadFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(cpu.executor().state.eflags, 0x8d7);
    assert_eq!(cpu.executor().state.regs[1], 4096);
    assert!(cpu.executor().state.rep.is_none());
    finish(&mut cpu);
    assert_eq!(calls.get(), 2);
    assert_eq!(cpu.executor().state.regs[1], 0);
    for at in [DATA, DATA + 4096] {
        assert_eq!(cpu.file_page_copy_on_write(at), Some(true));
    }
}

#[test]
fn comparison_stopping_in_first_page_never_initializes_later_file_pages() {
    let mut cpu = fixture(&[0xf3, 0xa6, 0xc3], true);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let bad = FilePage::lazy(move || {
        observed.set(observed.get() + 1);
        Err("must not read next page".into())
    });
    cpu.map_file_pages(
        SOURCE,
        &[FilePage::new([0x11; 4096]), bad.clone()],
        Prot::READ,
        false,
    )
    .unwrap();
    cpu.map_file_pages(DATA, &[FilePage::new([0x22; 4096]), bad], Prot::READ, false)
        .unwrap();
    cpu.executor_mut().state.regs[1] = 8192;
    cpu.executor_mut().state.regs[6] = SOURCE;
    cpu.executor_mut().state.regs[7] = DATA;
    finish(&mut cpu);
    assert_eq!(calls.get(), 0);
    assert_eq!(cpu.executor().state.regs[1], 8191);
}

#[test]
fn plain_operand_permission_faults_do_not_start_lazy_io_or_cow() {
    for write in [false, true] {
        let code = program(|a| {
            if write {
                a.movdqu(ptr(DATA as i32 + 4094), xmm0).unwrap();
            } else {
                a.movdqu(xmm0, ptr(DATA as i32 + 4094)).unwrap();
            }
            a.ret().unwrap();
        });
        let mut cpu = fixture(&code, true);
        let calls = Rc::new(Cell::new(0));
        let pages: Vec<_> = (0..2)
            .map(|_| {
                let count = calls.clone();
                FilePage::lazy(move || {
                    count.set(count.get() + 1);
                    Ok([0x31; 4096])
                })
            })
            .collect();
        cpu.map_file_pages(DATA, &pages, Prot::RW, true).unwrap();
        cpu.protect(DATA + 4096, 4096, if write { Prot::READ } else { Prot(0) })
            .unwrap();
        let before = cpu.executor().state.clone();
        let expected = if write {
            RunOutcome::WriteFault {
                pc: CODE,
                address: DATA + 4094,
            }
        } else {
            RunOutcome::ReadFault {
                pc: CODE,
                address: DATA + 4094,
            }
        };
        assert_eq!(cpu.executor_mut().run(STOP, 100).unwrap(), expected);
        assert_eq!(calls.get(), 0);
        assert_eq!(cpu.executor().state.regs, before.regs);
        assert_eq!(cpu.executor().state.xmm, before.xmm);
        assert_eq!(cpu.executor().state.eflags, before.eflags);
        for address in [DATA, DATA + 4096] {
            assert_eq!(cpu.file_page_copy_on_write(address), Some(true));
        }
        assert_eq!(cpu.stats().interpreter_instructions, 0);
    }
}

#[test]
fn file_cmps_and_scas_all_widths_directions_and_repeat_conditions_match_interpreter() {
    for width in [1, 2, 4] {
        for backwards in [false, true] {
            for cmps in [false, true] {
                for equal_repeat in [false, true] {
                    for stopping in [None, Some(17)] {
                        let mut code = vec![if equal_repeat { 0xf3 } else { 0xf2 }];
                        if width == 2 {
                            code.push(0x66);
                        }
                        code.push(match (cmps, width) {
                            (true, 1) => 0xa6,
                            (true, _) => 0xa7,
                            (false, 1) => 0xae,
                            _ => 0xaf,
                        });
                        code.push(0xc3);
                        let start = if backwards { 4093 } else { 4001 };
                        let mut outputs = vec![];
                        for native in [false, true] {
                            let mut cpu = fixture(&code, native);
                            let source = [FilePage::new([0x31; 4096]), FilePage::new([0x31; 4096])];
                            let mut data = [0x31; 8192];
                            for element in 0..33 {
                                let offset = if backwards {
                                    start - element * width
                                } else {
                                    start + element * width
                                };
                                let equal = equal_repeat != (stopping == Some(element));
                                if !equal {
                                    data[offset] ^= 0x80;
                                }
                            }
                            let destination: Vec<_> = data
                                .as_chunks::<4096>()
                                .0
                                .iter()
                                .map(|page| FilePage::new(*page))
                                .collect();
                            cpu.map_file_pages(SOURCE, &source, Prot::READ, false)
                                .unwrap();
                            cpu.map_file_pages(DATA, &destination, Prot::READ, true)
                                .unwrap();
                            let state = &mut cpu.executor_mut().state;
                            state.regs[0] = 0x3131_3131;
                            state.regs[1] = 33;
                            state.regs[6] = SOURCE + start as u32;
                            state.regs[7] = DATA + start as u32;
                            state.eflags = 0x8d7 | if backwards { 0x400 } else { 0 };
                            assert_eq!(
                                cpu.executor_mut().run(STOP, 10000).unwrap(),
                                RunOutcome::ReachedStop
                            );
                            if native {
                                assert_eq!(cpu.stats().interpreter_instructions, 0);
                            }
                            assert_eq!(
                                cpu.executor().state.regs[1],
                                if stopping.is_some() { 15 } else { 0 }
                            );
                            for address in [DATA, DATA + 4096] {
                                assert_eq!(cpu.file_page_copy_on_write(address), Some(true));
                            }
                            outputs.push((cpu.executor().state.regs, cpu.executor().state.eflags));
                        }
                        assert_eq!(
                            outputs[0], outputs[1],
                            "width={width}, DF={backwards}, CMPS={cmps}, REPE={equal_repeat}, stop={stopping:?}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn mixed_file_audio_elements_preserve_byte_atomic_storage() {
    for strategy in [
        adv_cpu::CompileStrategy::Fast,
        adv_cpu::CompileStrategy::Speed,
    ] {
        let code = program(|a| {
            a.movdqu(xmm0, ptr(DATA as i32 + 4094)).unwrap();
            a.movdqu(ptr(DATA as i32 + 4094), xmm1).unwrap();
            a.ret().unwrap();
        });
        let mut cpu = fixture(&code, false);
        let mut region = Region::new(CODE, code.len());
        region.compile_strategy = strategy;
        cpu.executor_mut().add_region(region).unwrap();
        let page = FilePage::new([0x17; 4096]);
        cpu.map_file_pages(DATA, std::slice::from_ref(&page), Prot::RW, true)
            .unwrap();
        let audio = cpu.map_shared_audio(DATA + 4096, 4096).unwrap();
        cpu.write(DATA + 4096, &[0x31; 14]).unwrap();
        cpu.executor_mut().state.xmm[1] = [0x5a; 16];
        finish(&mut cpu);
        assert_eq!(&cpu.executor().state.xmm[0][..2], &[0x17; 2]);
        assert_eq!(&cpu.executor().state.xmm[0][2..], &[0x31; 14]);
        let mut actual = [0; 14];
        audio.read_alias.copy_into(0, &mut actual);
        assert_eq!(actual, [0x5a; 14]);
        assert_eq!(page.snapshot().unwrap(), [0x17; 4096]);
    }
}
