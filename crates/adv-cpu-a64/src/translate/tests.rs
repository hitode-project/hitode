//! Execute whole generated units, including entry/exit flag mapping, on any host.
use super::*;
use crate::emulator::{CODE as HOST_CODE, Emulator};
use crate::runtime;
use crate::translate::lower::stubs;
use adv_cpu_core::{CpuState, Memory, X87State, interpreter::Interpreter};
use iced_x86::code_asm::*;
use std::mem::{offset_of, size_of};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const WINDOW_BASE: u64 = 7 << 32;
const CONTEXT: u64 = 0x20000000;
const CPU: u64 = CONTEXT + 0x1000;
const HOST_STACK: u64 = 0x30010000;

fn bytes(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn memory(code: &[u8], data: &[u8]) -> Memory {
    let mut memory = Memory::new(CODE, 0x30000).unwrap();
    memory.protect(CODE, 0x30000, 3).unwrap();
    memory.write(CODE, code).unwrap();
    memory.write(DATA, data).unwrap();
    memory.protect(CODE, 4096, 5).unwrap();
    memory
}

fn initial() -> CpuState {
    CpuState {
        regs: [
            0x81234567, 1, 0x12345678, DATA, 0x3f000, 0x20000, DATA, 0x76543210,
        ],
        eip: CODE,
        eflags: 0xed7,
        ..Default::default()
    }
}

// spec: A64A-PLACE-3, A64A-ROLE-2, A64-VER-1
#[test]
fn translation_words_match_baseline() {
    let compare = |name: &str, words: &[u32]| {
        let baseline = match name {
            "scalar" => concat!(
                "d1001129 b7f801c9 b8794b73 31000673 b83a4b73 b84046f0 d500401f 2a1303ea ",
                "5100066b 4a13016b a940479e cb100231 b5000111 9100439c d65f03c0 91001129 ",
                "52800100 52800001 72a00021 17ffffed 52800124 72a00024 f2c00064 17ffffe9 ",
            ),
            "checked" => concat!(
                "d1001129 b7f80989 11000f24 4a190084 376009c4 f94033a3 530c7f24 38644864 ",
                "36000944 37180064 b8794b73 14000008 f9403ba3 530c7f24 38644864 36000864 ",
                "37180844 f94037a3 b8794873 31000673 11000f44 4a1a0084 37600844 f94033a3 ",
                "530c7f44 38644864 360807c4 37100084 37180064 b83a4b73 14000014 f9403ba3 ",
                "530c7f44 38644864 360806c4 371806a4 371000a4 f94087a3 530c7f44 38644864 ",
                "36100104 f9406ba3 530c7f44 f8645863 53067f44 9ac42463 92400463 b5000523 ",
                "f94037a3 b83a4873 11000ee4 4a170084 37600544 f94033a3 530c7ee4 38644864 ",
                "360004c4 37180064 b8774b70 14000008 f9403ba3 530c7ee4 38644864 360003e4 ",
                "371803c4 f94037a3 b8774870 910012f7 d500401f 2a1303ea 5100066b 4a13016b ",
                "a940479e cb100231 b5000351 9100439c d65f03c0 91001129 52800100 52800001 ",
                "72a00021 17ffffaf 2a1903e1 10fff6e2 52800084 97ffffab 2a0003f3 17ffffbc ",
                "2a1a03e1 10fff882 2a1303e3 52800484 97ffffa4 17ffffd5 2a1703e1 10fffb62 ",
                "52800084 97ffff9f 2a0003f0 17ffffe0 52800124 72a00024 f2c00064 17ffff99 ",
            ),
            "switch" => concat!(
                "d1000d29 b7f80149 7100067f 540001a8 52800031 4b11026a 4a11026b 4a0a016b ",
                "10000411 f8737a31 d61f0220 91000d29 52800100 52800001 72a00021 17fffff1 ",
                "52800031 4b11026a 4a11026b 4a0a016b 91000529 52802001 72a00021 10ffffc2 ",
                "17ffffe8 10ffff8e 14000000 531e7670 52a00051 0b110210 b8704b70 f9401fb1 ",
                "4a504206 53003cc6 8b061231 a9404626 cb1000c6 b50000e6 d61f0220 d503201f ",
                "1000006c 00000000 1000006c 00000000 52800164 72a00024 f2c00044 17ffffd1 ",
            ),
            "loop" => concat!(
                "d1001d29 b7f80329 b8794b73 31000673 b83a4b73 31001339 3100135a 51000694 ",
                "1a9f37ed 1100068e 710005df ba1f85a2 d500401f 2a1403ea 1100068b 4a14016b ",
                "d1001d29 b7f80be9 540001a1 91001d29 52800241 72a00021 10ffffc2 17ffffe9 ",
                "10ffff8e 14000000 91001d29 52800100 52800001 72a00021 17ffffe2 14000001 ",
                "100009c0 f9400000 b9400001 51000421 b9000001 34000041 17ffffdc a9b81be5 ",
                "a90123e7 a9022be9 a90333eb a9043bed a90543ef d53b4210 a90643f1 f9003bfe ",
                "adbc47f0 ad014ff2 ad0257f4 ad035ff6 adbc67f8 ad016ffa ad0277fc ad037ffe ",
                "f94003a0 29005013 29015815 29026017 29036819 d53b4201 53187c22 121a0443 ",
                "53051444 52000084 2a040063 53041044 2a042c63 121c0164 2a040063 4a4a1144 ",
                "4a440884 4a440484 12000084 52000084 2a040863 2a0c2863 b9402404 52819aa5 ",
                "0a250084 2a030084 b9002404 52800001 72a00021 10fff942 aa1d03e0 f942afb0 ",
                "d63f0200 ad437ffe ad4277fc ad416ffa acc467f8 ad435ff6 ad4257f4 ad414ff2 ",
                "acc447f0 f9403bfe a94607f1 d51b4201 a94543ef a9443bed a94333eb a9422be9 ",
                "a94123e7 a8c81be5 17ffff98 91001d29 10fff66e 14000000 00000000 00000000 ",
                "91001d29 54000041 17ffffa2 52800100 52800001 72a00021 17ffff8a ",
            ),
            "worker" => concat!(
                "f940cfb1 b9400226 b9401227 b9402228 b940322a d1001d29 b7f80bc9 f94077b0 ",
                "91000610 f90077b0 b94193b0 35000890 aa0903e3 b7f80843 d2800391 9ad10863 ",
                "510004d0 4b1f0210 d342fe10 eb10007f 9a909063 b4000743 2a0703e0 d2c00030 ",
                "cb000202 f1001042 540006ab d344fc42 eb02007f 9a829063 b4000623 2a0803e0 ",
                "d2c00030 cb000202 f1001042 5400058b d344fc42 eb02007f 9a829063 b4000503 ",
                "2a0703e0 2a0803e1 6b01001f 540001a0 4b000022 f1001042 5400042b d344fc42 ",
                "eb02007f 9a829063 4b010002 f1001042 5400036b d344fc42 eb02007f 9a829063 ",
                "b40002e3 d2800390 9b107c65 aa0303e4 2a0703f0 8b304390 3dc00210 d2800030 ",
                "4e040e11 4eb18610 2a0803f0 8b304390 3d800210 510010c6 110040e7 11004108 ",
                "d1000484 b5fffe64 cb050129 17ffffbc 510004a5 35000065 17ffffb9 52800085 ",
                "b8674b8b 1100056b 110010ec 1100110d 510004ce b8284b8b 710004df 54000180 ",
                "d1001d29 b94193b0 34000090 51000610 b90193b0 34000a30 b7f804a9 2a0e03e6 ",
                "2a0c03e7 2a0d03e8 17ffffea 14000048 f940b3b0 b9400210 b9416bb1 0a110210 ",
                "350000f0 f940bbb0 b9400210 b9417bb1 0a110210 35000050 14000008 52800030 ",
                "b90193b0 b94197b0 11000610 b90197b0 121a0210 35000170 d29fff30 cb090210 ",
                "f940c3b1 8b110210 f940c7b1 eb11021f 54000088 f900c3b0 d29fff29 17ffff88 ",
                "52800030 b901b7b0 14000020 f940b3b0 b9400210 b9416bb1 0a110210 350000f0 ",
                "f940bbb0 b9400210 b9417bb1 0a110210 35000050 14000008 52800030 b90193b0 ",
                "b94197b0 11000610 b90197b0 121a0210 35000170 d29fff30 cb090210 f940c3b1 ",
                "8b110210 f940c7b1 eb11021f 54000088 f900c3b0 d29fff29 17ffffc1 52800030 ",
                "b901b7b0 1400000d d2800010 b901b3b0 1400000d d2800030 b901b3b0 1400000a ",
                "d2800050 b901b3b0 14000007 d2800070 b901b3b0 14000004 d2800090 b901b3b0 ",
                "14000001 00000000 ",
            ),
            "resident" => concat!(
                "2a0a03f0 d2800211 1b037e31 0b110210 11000e04 4a100084 376002e4 d2800390 ",
                "9b107c65 aa0303e4 2a0803f0 8b304370 3dc00210 d2800030 4e040e11 4eb18610 ",
                "2a0a03f0 8b304370 3d800210 510010e7 11004108 1100414a d1000484 b5fffe64 ",
                "cb050129 17ffffa5 510004a5 35000065 17ffffa2 52800085 b8684b6c 1100058c ",
                "11000d44 4a0a0084 376018e4 b82a4b6c 11001106 1100114d 510004ee 710004ff ",
                "54000100 d1001d29 b7f80389 2a0e03e7 2a0603e8 2a0c03e6 2a0d03ea 17ffffeb ",
            ),
            _ => unreachable!(),
        };
        let mut baseline: Vec<_> = baseline
            .split_ascii_whitespace()
            .map(|word| u32::from_str_radix(word, 16).unwrap())
            .collect();
        // Runtime field removals relocate field addresses without changing
        // the instructions that the path executes.
        let without_runtime_offset = |word: u32| {
            if word >> 5 & 31 == 29 && matches!(word >> 22, 0x244 | 0x2e4 | 0x2e5 | 0x3e4 | 0x3e5) {
                word & !(0xfff << 10)
            } else {
                word
            }
        };
        let words: Vec<u32> = words.iter().copied().map(without_runtime_offset).collect();

        match name {
            "loop" => {
                assert_eq!(words.len(), baseline.len());
                let baseline: Vec<u32> = baseline.into_iter().map(without_runtime_offset).collect();
                assert_eq!(words[..31], baseline[..31], "Tier-1 loop before its sensor");
            }
            "worker" => {
                for word in &mut baseline {
                    *word = match *word {
                        0xb901b3b0 => str(
                            W,
                            X16,
                            RUNTIME,
                            offset_of!(runtime::Runtime, recovery.record) as u32,
                        ),
                        0xb901b7b0 => str(
                            W,
                            X16,
                            RUNTIME,
                            offset_of!(runtime::Runtime, recovery.recalled) as u32,
                        ),
                        _ => *word,
                    };
                }
                let baseline: Vec<u32> = baseline.into_iter().map(without_runtime_offset).collect();
                assert_eq!(words, baseline, "detached worker");
            }
            "resident" => {
                // Compare the steady loop before its exit accounting. Side
                // branches keep their opcode and operands as cold code moves.
                let opcode = |word: u32| match word & 0x7e00_0000 {
                    0x3600_0000 => word & !0x0007_ffe0, // TBZ/TBNZ
                    _ => word,
                };
                let body = &words[116..164];
                assert!(
                    body.iter()
                        .copied()
                        .map(opcode)
                        .eq(baseline.into_iter().map(without_runtime_offset).map(opcode))
                );
            }
            _ => assert_eq!(
                words,
                baseline
                    .into_iter()
                    .map(without_runtime_offset)
                    .collect::<Vec<_>>(),
                "{name}"
            ),
        }
    };
    emission_fixture_words(compare);
}

// spec: A64A-ROLE-4
#[test]
fn whole_unit_emission_matches_baseline() {
    emission_fixture_words(|name, words| {
        // FNV-1a over the little-endian instruction bytes, including cold paths.
        let digest = words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .fold(0xcbf29ce484222325_u64, |digest, byte| {
                (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        let baseline = match name {
            "scalar" => (24, 0x7ced329c2d703d35),
            "checked" => (104, 0x20f24b50de03f256),
            "switch" => (48, 0x1b76e4b83d612bcd),
            "loop" => (119, 0xb8481f7bf0df221c),
            "worker" => (178, 0x439641c71718cb83),
            "resident" => (501, 0x5bc5104195d4aef9),
            _ => unreachable!(),
        };
        assert_eq!((words.len(), digest), baseline, "{name}");
    });
}

fn emission_fixture_words(compare: impl Fn(&str, &[u32])) {
    let scalar = bytes(|a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, 1).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.ret().unwrap();
    });
    let table = bytes(|a| {
        a.cmp(eax, 1).unwrap();
        a.ja(u64::from(CODE + 0x100)).unwrap();
        a.jmp(dword_ptr(eax * 4 + DATA as i32)).unwrap();
    });
    let row = bytes(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, 1).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
    });
    let host = HOST_CODE as usize;
    let stubs = Stubs {
        call: host,
        exit: host,
        dispatch: host,
        lookup: host,
        lookup_slow: host,
        ret_miss: host,
        access: host,
        fault: host,
        opt_exit: host,
    };
    for (name, code, cold, checked) in [
        ("scalar", &scalar, true, false),
        ("checked", &scalar, true, true),
        ("switch", &table, true, false),
        ("loop", &row, false, false),
    ] {
        let mut mem = memory(code, &CODE.to_le_bytes().repeat(2));
        mem.protect(DATA, 4096, 1).unwrap();
        let mut work = Workspace {
            cold,
            resident_links: true,
            ..Workspace::default()
        };
        work.unit.images.push((CODE, 0x30000));
        let mut decodes = front::Decodes::default();
        work.unit.build_cached(
            &mut mem,
            CODE,
            &[CODE + code.len() as u32],
            &|_, _| None,
            &|_| None,
            !cold,
            &mut decodes,
        );
        lower_unit(
            &mut work.code,
            &work.unit,
            CODE,
            &|_| checked,
            3,
            Features { flagm: true },
            host,
            stubs,
            0x037f,
            false,
            true,
        );
        compare(name, work.words());

        #[cfg(native)]
        if name == "loop" {
            work.unit.summarize(&mut decodes);
            let trace = &work.unit.trace;
            let mut proof = computation::Proof::build(trace).unwrap();
            proof.prove_split(trace, |pc, _| {
                trace.x86[trace.insts.iter().position(|i| i.pc == pc)?].clone()
            });
            let at = initial();
            let built = opt::front::build(&work.unit, &proof, &at).unwrap();
            compare("worker", &opt::back::lower(&built.func).words);
            let built =
                opt::front::build_with(&work.unit, None, &at, &|_| false, &|_| false).unwrap();
            compare(
                "resident",
                &opt::back::lower_resident(&built.func, &built.assumed, &built.func.entry, false)
                    .words,
            );
        }
    }
}

// spec: A64F-FP-7
#[test]
fn full_x87_units_keep_short_branches_to_cold_code_in_reach() {
    // A whole trace of x87 divisions gives every instruction a long generic
    // path; the parity edge and the backedge refusal still use tbz.
    for parity in [false, true] {
        let code = bytes(|a| {
            let mut top = a.create_label();
            a.fld1().unwrap();
            a.fld1().unwrap();
            a.set_label(&mut top).unwrap();
            for _ in 0..90 {
                a.fdiv_2(st0, st1).unwrap();
            }
            a.dec(ecx).unwrap();
            if parity {
                a.jp(top).unwrap();
            } else {
                a.jne(top).unwrap();
            }
        });
        let mut mem = memory(&code, &[]);
        let mut work = Workspace::default();
        let host = HOST_CODE as usize;
        let stubs = Stubs {
            call: host,
            exit: host,
            dispatch: host,
            lookup: host,
            lookup_slow: host,
            ret_miss: host,
            access: host,
            fault: host,
            opt_exit: host,
        };
        let features = Features { flagm: false };
        let no_calls = |_, _| None;
        translate(
            &mut work,
            &mut mem,
            CODE + 4,
            &[],
            &|_| false,
            0,
            features,
            host,
            stubs,
            &no_calls,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
            false,
        );
        assert!(
            work.words().len() * 4 > 1 << 15,
            "the unit outgrows tbz reach"
        );
    }
}

// spec: A64-CTL-6
#[test]
fn bytecode_vm_dispatch_instruction_gate() {
    // Authored VM: eight byte opcodes and eight handlers, each returning to
    // the fetch/dispatch pair. Compare the existing emitter with a resolved
    // site-table prototype; this deliberately adds no production mechanism.
    let guest_table = DATA + 0x100;
    let code = bytes(|a| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.jmp(dword_ptr(eax * 4 + guest_table as i32)).unwrap();
        for _ in 0..8 {
            a.inc(edx).unwrap();
            a.jmp(CODE as u64).unwrap();
        }
    });
    let mut mem = memory(&code, &[]);
    let mut work = Workspace::default();
    let stubs = Stubs {
        call: HOST_CODE as usize,
        exit: HOST_CODE as usize,
        dispatch: HOST_CODE as usize,
        lookup: HOST_CODE as usize,
        lookup_slow: HOST_CODE as usize,
        ret_miss: HOST_CODE as usize,
        access: HOST_CODE as usize,
        fault: HOST_CODE as usize,
        opt_exit: HOST_CODE as usize,
    };
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[],
        &|_| false,
        0,
        Features { flagm: false },
        HOST_CODE as usize,
        stubs,
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
    );
    let branch = work.words().iter().position(|&w| w == br(X17)).unwrap();
    let target_load = work.sites().last().unwrap().0 / 4;
    let before = branch + 1 - target_load;
    assert_eq!(before, 9, "guest target load through the hit-path br");
    // LDR (register), option 011: [x17, x19, lsl #3]. The existing
    // encoder's option 010 is UXTW; bit 13 selects the X-register form.
    let resolved = ldr_reg(X, X17, X17, host(0), true) | (1 << 13);
    let baseline = work.words()[2..=branch].to_vec();
    work.unit.images.push((CODE, 0x30000));
    mem.protect(DATA, 4096, 1).unwrap();
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[],
        &|_| false,
        0,
        Features { flagm: false },
        HOST_CODE as usize,
        stubs,
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
    );
    assert_eq!(work.unit.tables.len(), 1);
    let hot = work.words().iter().position(|&w| w == br(X17)).unwrap();
    assert_eq!(work.words()[hot - 1], resolved);
    let after = 3;
    assert!(before - after >= 4);
    let mut results = Vec::new();
    for direct in [false, true] {
        let mut asm = crate::asm::Asm::new();
        let top = asm.label();
        let table = asm.label();
        let next = asm.label();
        let handlers: Vec<_> = (0..8).map(|_| asm.label()).collect();
        asm.bind(top);
        if direct {
            asm.emit(ldr_narrow(Narrow::Byte, host(0), WINDOW, host(6)));
            asm.adr(X17, table);
            asm.emit_all(&[resolved, br(X17)]);
        } else {
            // Skip only the unit's two-instruction budget head. The rest
            // is the real emitter, including opcode fetch and guest load.
            asm.emit_all(&baseline);
        }
        for (opcode, &handler) in handlers.iter().enumerate() {
            asm.bind(handler);
            asm.emit(add_imm(W, X0, X0, opcode as u32 + 1, false));
            asm.b(next);
        }
        asm.bind(next);
        asm.emit_all(&[
            add_imm(W, host(6), host(6), 1, false),
            subs_imm(W, X9, X9, 1, false),
        ]);
        asm.b_cond(Cond::Ne, top);
        asm.emit(ret(Reg::LR));
        if !asm.offset().is_multiple_of(8) {
            asm.emit(NOP);
        }
        asm.bind(table);
        for &handler in &handlers {
            let address = HOST_CODE + asm.position(handler) as u64;
            asm.emit_all(&[address as u32, (address >> 32) as u32]);
        }
        let mut emu = Emulator::new();
        emu.map(WINDOW_BASE + u64::from(DATA), 0x1000);
        emu.map(CONTEXT, 0x1000);
        let indirect = CONTEXT + 0x10000;
        emu.map(indirect, (runtime::TABLE_LEN * 16) as u64);
        emu.write(CONTEXT + u64::from(runtime::TABLE), &indirect.to_le_bytes());
        emu.write(WINDOW_BASE + u64::from(DATA), &[0, 1, 2, 3, 4, 5, 6, 7]);
        for (opcode, &handler) in handlers.iter().enumerate() {
            let target = CODE + 0x100 + opcode as u32 * 16;
            emu.write(
                WINDOW_BASE + u64::from(guest_table) + opcode as u64 * 4,
                &target.to_le_bytes(),
            );
            let entry = indirect + (runtime::table_slot(target) * 16) as u64;
            emu.write(entry, &u64::from(target).to_le_bytes());
            emu.write(
                entry + 8,
                &(HOST_CODE + asm.position(handler) as u64).to_le_bytes(),
            );
        }
        emu.set_x(WINDOW, WINDOW_BASE);
        emu.set_x(RUNTIME, CONTEXT);
        emu.set_x(host(6), u64::from(DATA));
        emu.set_x(X9, 8);
        emu.set_x(X0, 0);
        emu.call(&asm.finish()).unwrap();
        results.push((emu.x(X0), emu.x(host(6))));
    }
    assert_eq!(results, vec![(36, u64::from(DATA + 8)); 2]);
    eprintln!("VM dispatch gate: {before} -> {after} A64 instructions; dependent loads 2 -> 1");
    eprintln!(
        "VM complete fetch-to-br: {} -> {} A64 instructions",
        baseline.len(),
        after + 1
    );
}

fn reference(code: &[u8], data: &[u8], initial: &CpuState) -> CpuState {
    let mut memory = memory(code, data);
    let mut state = initial.clone();
    let mut interpreter = Interpreter::new();
    while state.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            adv_cpu_core::interpreter::Step::Executed
        );
    }
    state
}

// spec: A64-TR-4, A64-VER-1
#[test]
fn tier_one_cpuid_uses_the_guest_profile_and_keeps_lazy_flags() {
    let code = bytes(|a| {
        a.add(eax, esi).unwrap();
        a.cpuid().unwrap();
    });
    let mut h = Harness::new();
    for flagm in [false, true] {
        for leaf in [0u32, 1, 2, 7, 0x8000_0000, 0x8000_0001, u32::MAX] {
            for subleaf in [0, 3, u32::MAX] {
                let mut state = initial();
                state.regs[0] = leaf.wrapping_sub(state.regs[6]);
                state.regs[1] = subleaf;
                let (got, exit, _) = h.run(&code, &[], &state, flagm);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(got, reference(&code, &[], &state));
            }
        }
    }
}

#[cfg(native)]
fn tier_one_pair(code: &[u8]) -> (crate::Executor, crate::Executor) {
    let build = || {
        let mut exec = crate::Executor::new(CODE, 0x30000).unwrap();
        exec.protect(CODE, 0x30000, 3).unwrap();
        exec.write(CODE, code).unwrap();
        exec.protect(CODE, 4096, 5).unwrap();
        exec
    };
    let mut oracle = build();
    oracle.interpreter_only = true;
    (build(), oracle)
}

#[cfg(native)]
fn compare_tier_one(
    native: &mut crate::Executor,
    oracle: &mut crate::Executor,
    state: &CpuState,
    stop: u32,
    fuel: u64,
) -> adv_cpu_core::RunOutcome {
    native.state = state.clone();
    oracle.state = state.clone();
    let outcome = oracle.run(stop, fuel).unwrap();
    assert_eq!(native.run(stop, fuel).unwrap(), outcome, "{state:?}");
    assert_eq!(native.state, oracle.state, "{state:?}");
    outcome
}

#[cfg(native)]
fn x87_setup() -> CpuState {
    let mut state = initial();
    state.x87.regs = std::array::from_fn(|slot| {
        adv_cpu_core::X87Register::from_parts((1 << 63) | (slot as u64) << 16, 0x3fff)
    });
    state.x87.status = 0x4704;
    state.x87.set_top(5);
    state.x87.tag = 0x8f7f;
    state.x87.ip = 0x0123_4567;
    state.x87.dp = 0x89ab_cdef;
    state.x87.opcode = 0x567;
    state.x87.cs = 0x23;
    state.x87.ds = 0x2b;
    state
}

// spec: A64-TR-1
#[test]
#[cfg(native)]
fn tier_one_widens_bit_tests_in_large_straight_line_units() {
    // Each FSQRT has an early bit test to its cold slow path, which is
    // laid out after the whole body.
    let code = bytes(|a| {
        for _ in 0..96 {
            a.fsqrt().unwrap();
        }
        a.nop().unwrap();
    });
    let stop = CODE + code.len() as u32 - 1;
    let (mut native, mut oracle) = tier_one_pair(&code);
    let mut state = x87_setup();
    state.x87.control = 0x027f;
    state.x87.regs[state.x87.top()] = adv_cpu_core::X87Register::from_parts(1 << 63, 0x3fff + 64);
    state.x87.set_physical_tag(state.x87.top(), 0);
    for _ in 0..3 {
        compare_tier_one(&mut native, &mut oracle, &state, stop, 200);
    }
    assert!(native.stats.native_code_bytes > 32 * 1024);
}

// spec: A64F-FP-1, A64F-FP-2, A64F-FP-3, A64-VER-1
#[test]
#[cfg(native)]
fn tier_one_fsqrt_matches_x87_rounding_and_exception_paths() {
    use adv_cpu_core::{RunOutcome, X87Register};
    let code = bytes(|a| {
        a.fsqrt().unwrap();
        a.nop().unwrap();
    });
    let stop = CODE + code.len() as u32 - 1;
    let (mut native, mut oracle) = tier_one_pair(&code);
    let mut state = x87_setup();
    let top = state.x87.top();
    let int_bit = 1 << 63;
    let exact = X87Register::from_parts(int_bit, 0x4001); // 4
    state.x87.regs[top] = exact;
    state.x87.set_physical_tag(top, exact.tag());
    compare_tier_one(&mut native, &mut oracle, &state, stop, 8);

    for p in [0, 2, 3] {
        for r in 0..4 {
            state.x87.control = 0x007f | p << 8 | r << 10;
            for (value, fast) in [
                (X87Register::from_parts(0, 0), true),
                (X87Register::from_parts(0, 0x8000), true),
                (exact, true),
                (X87Register::from_parts(int_bit, 0x4000), p != 3), // 2
                (X87Register::from_parts(3 << 62, 0x4000), p != 3), // 3
                (X87Register::from_parts(u64::MAX << 40, 0x7ffe), p != 3),
                (X87Register::from_parts(int_bit, 1), true),
                (X87Register::from_parts(int_bit | 1 << 11, 0x3fff), p == 2),
                (X87Register::from_parts(int_bit | 1, 0x3fff), false),
                (X87Register::from_parts(int_bit, 0xbfff), false),
                (X87Register::from_parts(3 << 62 | 7, 0x7fff), false),
                (X87Register::from_parts(int_bit | 7, 0x7fff), false),
                (X87Register::from_parts(1, 0), false),
                (X87Register::from_parts(int_bit, 0), false),
            ] {
                state.x87.regs[top] = value;
                state.x87.set_physical_tag(top, value.tag());
                let values = native.value_calls_at(CODE);
                assert_eq!(
                    compare_tier_one(&mut native, &mut oracle, &state, stop, 8),
                    RunOutcome::ReachedStop
                );
                assert_eq!(
                    native.value_calls_at(CODE) - values,
                    u64::from(!fast),
                    "precision={p} rounding={r} input={value:?}"
                );
            }
        }
    }
    let values = native.value_calls_at(CODE);
    state.x87.set_physical_tag(top, 3);
    assert_eq!(
        compare_tier_one(&mut native, &mut oracle, &state, stop, 8),
        RunOutcome::ReachedStop
    );
    assert_eq!(native.value_calls_at(CODE) - values, 1);
    assert!(native.stats.compilations > 0);

    // Unmasked exceptions retire FSQRT, then the next waiting
    // instruction raises #MF with the recorded state.
    let code = bytes(|a| {
        a.fsqrt().unwrap();
        a.wait().unwrap();
        a.nop().unwrap();
    });
    let stop = CODE + code.len() as u32 - 1;
    let (mut native, mut oracle) = tier_one_pair(&code);
    compare_tier_one(&mut native, &mut oracle, &state, stop, 8);
    let two = X87Register::from_parts(int_bit, 0x4000);
    for (value, mask, tag, pending) in [
        (two, 0x20, 0, 0),
        (X87Register::from_parts(int_bit, 0xbfff), 1, 0, 0),
        (X87Register::from_parts(int_bit | 7, 0x7fff), 1, 2, 0),
        (X87Register::from_parts(1, 0), 2, 2, 0),
        (two, 1, 3, 0),
        (two, 1, 0, 0x8081),
    ] {
        state.x87.control = 0x027f & !mask;
        state.x87.regs[top] = value;
        state.x87.set_physical_tag(top, tag);
        state.x87.status = (state.x87.status & !0x80ff) | pending;
        let values = native.value_calls_at(CODE);
        assert_eq!(
            compare_tier_one(&mut native, &mut oracle, &state, stop, 8),
            RunOutcome::Trap {
                pc: if pending == 0 { CODE + 2 } else { CODE },
                vector: 16,
            }
        );
        // A pending exception is a version fact: #MF is delivered as an
        // event at entry, without evaluating FSQRT.
        let expected = u64::from(pending == 0);
        assert_eq!(native.value_calls_at(CODE) - values, expected);
    }
}

// spec: A64-TR-4, A64F-FP-1, EXEC-CORE-8
#[test]
#[cfg(native)]
fn tier_one_fnsave_frstor_match_environment_images_and_faults() {
    use adv_cpu_core::RunOutcome;
    use iced_x86::{Code, Instruction, MemoryOperand, Register};

    for (save, restore) in [
        (Code::Fnsave_m94byte, Code::Frstor_m94byte),
        (Code::Fnsave_m108byte, Code::Frstor_m108byte),
    ] {
        let code = bytes(|a| {
            for (op, reg) in [(save, Register::ESI), (restore, Register::EDI)] {
                a.add_instruction(Instruction::with1(op, MemoryOperand::with_base(reg)).unwrap())
                    .unwrap();
            }
            a.wait().unwrap();
            a.nop().unwrap();
        });
        let stop = CODE + code.len() as u32 - 1;
        let (mut native, mut oracle) = tier_one_pair(&code);
        let mut state = x87_setup();
        state.x87.status |= 0x8081;
        state.x87.control &= !1;

        for exec in [&mut native, &mut oracle] {
            exec.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let restore_pc = CODE + iced_x86::Decoder::new(32, &code, 0).decode().len() as u32;
        let fault = DATA + 4096 - 64;
        for (save_at, restore_at, cut, outcome) in [
            (
                DATA,
                DATA,
                stop,
                RunOutcome::Trap {
                    pc: stop - 1,
                    vector: 16,
                },
            ),
            (
                DATA,
                DATA,
                stop,
                RunOutcome::Trap {
                    pc: stop - 1,
                    vector: 16,
                },
            ),
            (DATA, DATA, restore_pc, RunOutcome::ReachedStop),
            (
                fault,
                DATA,
                stop,
                RunOutcome::WriteFault {
                    pc: CODE,
                    address: DATA + 4096,
                },
            ),
            (
                DATA,
                fault,
                stop,
                RunOutcome::ReadFault {
                    pc: restore_pc,
                    address: DATA + 4096,
                },
            ),
        ] {
            state.regs[6] = save_at;
            state.regs[7] = restore_at;
            for exec in [&mut native, &mut oracle] {
                exec.write(DATA, &[0x5a; 4096]).unwrap();
            }
            assert_eq!(
                compare_tier_one(&mut native, &mut oracle, &state, cut, 8),
                outcome
            );
            assert_eq!(
                native.memory().read_data(DATA, 4096).unwrap().as_ref(),
                oracle.memory().read_data(DATA, 4096).unwrap().as_ref()
            );
        }
        assert!(native.value_calls_at(CODE) > 0);
        assert!(native.value_calls_at(restore_pc) > 0);
    }
}

fn state_bytes(initial: &CpuState) -> Vec<u8> {
    let mut state = vec![0; size_of::<CpuState>()];
    for (n, value) in initial.regs.iter().enumerate() {
        let at = offset_of!(CpuState, regs) + n * 4;
        state[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (at, value) in [
        (offset_of!(CpuState, eip), initial.eip),
        (offset_of!(CpuState, eflags), initial.eflags),
        (offset_of!(CpuState, mxcsr), initial.mxcsr),
        (offset_of!(CpuState, fs_base), initial.fs_base),
    ] {
        state[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    let at_xmm = offset_of!(CpuState, xmm);
    state[at_xmm..at_xmm + 128].copy_from_slice(initial.xmm.as_flattened());
    let at_mmx = offset_of!(CpuState, mmx);
    for (n, value) in initial.mmx.iter().enumerate() {
        state[at_mmx + n * 8..at_mmx + n * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    let at_x87 = offset_of!(CpuState, x87);
    let x87 = &initial.x87;
    for (n, r) in x87.regs.iter().enumerate() {
        state[at_x87 + n * 10..at_x87 + n * 10 + 10].copy_from_slice(&r.0);
    }
    for (at, value) in [
        (offset_of!(X87State, control), x87.control),
        (offset_of!(X87State, status), x87.status),
        (offset_of!(X87State, tag), x87.tag),
        (offset_of!(X87State, opcode), x87.opcode),
    ] {
        state[at_x87 + at..at_x87 + at + 2].copy_from_slice(&value.to_le_bytes());
    }
    for (at, value) in [
        (offset_of!(X87State, ip), x87.ip),
        (offset_of!(X87State, dp), x87.dp),
    ] {
        state[at_x87 + at..at_x87 + at + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mmx_mode = offset_of!(CpuState, mmx_mode);
    state[mmx_mode..mmx_mode + 4].copy_from_slice(&initial.mmx_mode.to_le_bytes());
    state
}

struct Harness {
    emu: Emulator,
    loaded: Vec<u32>,
    stubs: stubs::Stubs,
    prefix: Vec<u32>,
    dispatch: usize,
    forbidden: usize,
}

impl Harness {
    fn new() -> Self {
        let stubs = stubs::stubs();
        let mut prefix = stubs.code.clone();
        // Dispatch returns to this test instead of invoking a Rust owner. An
        // unexpected helper must trap, so a semantic match cannot hide fallback.
        let dispatch = prefix.len() * 4;
        prefix.push(movz(W, X0, Exit::Dispatch.word(), 0));
        prefix.push(b(stubs.exit as i32 - (dispatch as i32 + 4)));
        let forbidden = prefix.len() * 4;
        prefix.push(brk(0x101));
        let mut emu = Emulator::new();
        emu.map(CONTEXT, 0x4000);
        emu.map(HOST_STACK - 0x10000, 0x10000);
        emu.map(WINDOW_BASE + u64::from(DATA), 0x10000);
        Self {
            emu,
            loaded: Vec::new(),
            stubs,
            prefix,
            dispatch,
            forbidden,
        }
    }

    fn run(
        &mut self,
        code: &[u8],
        data: &[u8],
        initial: &CpuState,
        flagm: bool,
    ) -> (CpuState, Exit, u64) {
        self.run_form(code, data, initial, flagm, None)
    }

    fn run_form(
        &mut self,
        code: &[u8],
        data: &[u8],
        initial: &CpuState,
        flagm: bool,
        admitted: Option<u64>,
    ) -> (CpuState, Exit, u64) {
        let mut memory = memory(code, data);
        let at = self.prefix.len() * 4;
        let end = CODE + code.len() as u32;
        let mut work = Workspace::default();
        let addr = |offset| HOST_CODE as usize + offset;
        let stubs = Stubs {
            call: addr(self.forbidden),
            exit: addr(self.stubs.exit),
            dispatch: addr(self.dispatch),
            lookup: addr(self.forbidden),
            lookup_slow: addr(self.forbidden),
            ret_miss: addr(self.forbidden),
            access: addr(self.forbidden),
            fault: addr(self.forbidden),
            opt_exit: addr(self.forbidden),
        };
        translate(
            &mut work,
            &mut memory,
            CODE,
            &[end],
            &|_| false,
            0,
            Features { flagm },
            addr(at),
            stubs,
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            initial.x87.control,
            false,
        );
        assert!(
            !work
                .unit
                .trace
                .insts
                .iter()
                .any(|i| matches!(i.op, decode::Op::Value(_))),
            "form has no native plan"
        );
        let mut host_code = self.prefix.clone();
        #[cfg(native)]
        let mut computation_code = Code::default();
        if admitted.is_some() {
            #[cfg(native)]
            {
                let proof = computation::Proof::build(&work.unit.trace).expect("range proof");
                assert!(
                    proof
                        .evaluate(initial, &mut computation::Evaluation::default(), |addr| {
                            Some(u32::from_le_bytes(
                                memory.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                            ))
                        })
                        .is_some(),
                    "entry satisfies proof"
                );
                let lowered = lower_computation(
                    &work.unit,
                    &mut computation_code,
                    CODE,
                    Features { flagm },
                    addr(at),
                    stubs,
                    (initial.x87.control, false, Some(&proof)),
                )
                .expect("detached form");
                host_code.extend_from_slice(lowered.words);
            }
            #[cfg(not(native))]
            panic!("admitted form requires native translation");
        } else {
            host_code.extend_from_slice(work.words());
            // Wide bodies trap, so a matching result proves the double path.
            let marks = work.marks();
            for (n, mark) in marks.iter().enumerate() {
                if mark.part == Part::X87Wide {
                    let end = marks
                        .get(n + 1)
                        .map_or(work.words().len(), |m| m.offset as usize / 4);
                    host_code[at / 4 + mark.offset as usize / 4..at / 4 + end].fill(brk(0x87));
                }
            }
        }
        let mut context = vec![0; size_of::<runtime::Runtime>()];
        let mut put = |offset: usize, value: &[u8]| {
            context[offset..offset + value.len()].copy_from_slice(value)
        };
        put(offset_of!(runtime::Runtime, cpu), &CPU.to_le_bytes());
        put(
            offset_of!(runtime::Runtime, window),
            &WINDOW_BASE.to_le_bytes(),
        );
        put(
            offset_of!(runtime::Runtime, countdown),
            &admitted.unwrap_or(100).to_le_bytes(),
        );
        if admitted.is_some() {
            put(
                offset_of!(runtime::Runtime, shadow),
                &WINDOW_BASE.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, recall),
                &(CONTEXT + 0x3f00).to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, recall_mask),
                &1u32.to_le_bytes(),
            );
            // The recall has spent its grace: leave at the first checkpoint.
            put(
                offset_of!(runtime::Runtime, deferred),
                &runtime::RECALL_GRACE.to_le_bytes(),
            );
        }
        let permissions = CONTEXT + 0x3000;
        put(
            offset_of!(runtime::Runtime, alias_classes),
            &permissions.to_le_bytes(),
        );
        self.emu.write(permissions, &vec![3; 0x100]);
        self.emu.write(CONTEXT + 0x3f00, &1u32.to_le_bytes());
        self.emu.write(CONTEXT, &context);
        let state = state_bytes(initial);
        self.emu.write(CPU, &state);
        self.emu.write(WINDOW_BASE + u64::from(DATA), data);
        self.emu.set_sp(HOST_STACK);
        self.emu.set_x(X0, CONTEXT);
        self.emu.set_x(X1, HOST_CODE + at as u64);
        // Registers/data vary much more often than generated code. Reuse only
        // byte-identical units; replacements still flush Unicorn's old blocks.
        if self.loaded == host_code {
            self.emu.call_loaded_at(self.stubs.enter)
        } else {
            let result = self.emu.call_at(&host_code, self.stubs.enter);
            self.loaded = host_code;
            result
        }
        .expect("native unit returns without a helper");
        assert_eq!(self.emu.sp(), HOST_STACK);
        let state = self.emu.read(CPU, size_of::<CpuState>());
        let at_xmm = offset_of!(CpuState, xmm);
        let at_mmx = offset_of!(CpuState, mmx);
        let at_x87 = offset_of!(CpuState, x87);
        let read32 = |at| u32::from_le_bytes(state[at..at + 4].try_into().unwrap());
        let mut actual = initial.clone();
        actual.regs = std::array::from_fn(|n| read32(offset_of!(CpuState, regs) + n * 4));
        actual.eip = read32(offset_of!(CpuState, eip));
        actual.eflags = read32(offset_of!(CpuState, eflags));
        actual.mxcsr = read32(offset_of!(CpuState, mxcsr));
        actual
            .xmm
            .as_flattened_mut()
            .copy_from_slice(&state[at_xmm..at_xmm + 128]);
        actual.mmx = std::array::from_fn(|n| {
            u64::from_le_bytes(
                state[at_mmx + n * 8..at_mmx + n * 8 + 8]
                    .try_into()
                    .unwrap(),
            )
        });
        let read16 =
            |at: usize| u16::from_le_bytes(state[at_x87 + at..at_x87 + at + 2].try_into().unwrap());
        let x87 = &mut actual.x87;
        for (n, r) in x87.regs.iter_mut().enumerate() {
            r.0.copy_from_slice(&state[at_x87 + n * 10..at_x87 + n * 10 + 10]);
        }
        x87.control = read16(offset_of!(X87State, control));
        x87.status = read16(offset_of!(X87State, status));
        x87.tag = read16(offset_of!(X87State, tag));
        x87.opcode = read16(offset_of!(X87State, opcode));
        x87.ip = read32(at_x87 + offset_of!(X87State, ip));
        x87.dp = read32(at_x87 + offset_of!(X87State, dp));
        let exit = Exit::from_raw(self.emu.x(X0) as u32).unwrap();
        let left = self.emu.x(X1);
        (actual, exit, left)
    }
}

// spec: A64-TR-5, A64-VER-2
#[test]
fn resident_pixel_loops_cross_emulated_four_kib_pages_without_reordering_accesses() {
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
        a.paddusb(xmm0, xmm1).unwrap();
        a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        a.add(esi, 16).unwrap();
        a.add(edi, 16).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let data: Vec<u8> = (0..0x4000usize).map(|n| (n * 71 + n / 7) as u8).collect();
    let mut state = initial();
    state.regs[1] = 4;
    state.regs[6] = DATA + 4090;
    state.regs[7] = DATA + 8188;
    state.xmm[1] = std::array::from_fn(|n| (n * 13) as u8);
    let mut expected = state.clone();
    let mut mem = memory(&code, &data);
    let mut interpreter = Interpreter::new();
    while expected.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut expected, &mut mem),
            adv_cpu_core::interpreter::Step::Executed
        );
    }
    for flagm in [false, true] {
        let mut h = Harness::new();
        let (actual, exit, left) = h.run(&code, &data, &state, flagm);
        assert_eq!(actual.regs, expected.regs);
        assert_eq!(actual.eflags, expected.eflags);
        assert_eq!(actual.xmm, expected.xmm);
        assert_eq!(actual.eip, expected.eip);
        assert_eq!(exit, Exit::Dispatch);
        assert_eq!(left, 72);
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
            mem.read(DATA, data.len()).unwrap()
        );
    }
}

// spec: A64-FLAG-2
#[test]
fn bit_scans_preserve_undefined_flags_zero_destinations_and_word_halves() {
    let mut h = Harness::new();
    let values: Vec<u32> = [0, u32::MAX, 0x55555555, 0xaaaaaaaa]
        .into_iter()
        .chain((0..32).map(|bit| 1 << bit))
        .collect();
    // TZCNT and LZCNT run as BSF and BSR on this processor.
    for (reverse, counted) in [(false, false), (true, false), (false, true), (true, true)] {
        for word in [false, true] {
            for dst in 0..8 {
                for source in 0..3 {
                    let code = bytes(|a| {
                        // Exercise live flag recipes, not just a canonical entry.
                        a.add(eax, 0x1234).unwrap();
                        let regs = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
                        let words = [ax, cx, dx, bx, sp, bp, si, di];
                        macro_rules! scan {
                            ($d:expr, $s:expr) => {
                                match (reverse, counted) {
                                    (false, false) => a.bsf($d, $s),
                                    (true, false) => a.bsr($d, $s),
                                    (false, true) => a.tzcnt($d, $s),
                                    (true, true) => a.lzcnt($d, $s),
                                }
                                .unwrap()
                            };
                        }
                        if word {
                            match source {
                                0 => scan!(words[dst], words[2]),
                                1 => scan!(words[dst], words[dst]),
                                _ => scan!(words[dst], word_ptr(DATA as i32)),
                            }
                        } else {
                            match source {
                                0 => scan!(regs[dst], edx),
                                1 => scan!(regs[dst], regs[dst]),
                                _ => scan!(regs[dst], dword_ptr(DATA as i32)),
                            }
                        }
                    });
                    for &value in &values {
                        let mut state = initial();
                        state.regs[if source == 1 { dst } else { 2 }] = value;
                        let data = value.to_le_bytes();
                        let want = reference(&code, &data, &state);
                        for flagm in [false, true] {
                            let (got, exit, left) = h.run(&code, &data, &state, flagm);
                            assert_eq!(exit, Exit::Dispatch);
                            assert_eq!(left, 98);
                            assert_eq!(
                                got, want,
                                "reverse={reverse}, counted={counted}, word={word}, dst={dst}, source={source}, value={value}, flagm={flagm}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-FLAG-2
#[test]
fn shifts_by_cl_cover_registers_memory_and_every_count_with_live_and_dead_flags() {
    let mut h = Harness::new();
    for dst in 0..9 {
        for kind in 0..5 {
            for dead in [false, true] {
                let code = bytes(|a| {
                    a.add(edx, eax).unwrap();
                    macro_rules! shift {
                        ($dst:expr) => {
                            match kind {
                                0 => a.shl($dst, cl),
                                1 => a.shr($dst, cl),
                                2 => a.sar($dst, cl),
                                3 => a.rol($dst, cl),
                                _ => a.ror($dst, cl),
                            }
                            .unwrap()
                        };
                    }
                    if dst == 8 {
                        shift!(dword_ptr(ebp + 0x1c));
                        a.mov(edi, dword_ptr(ebp + 0x1c)).unwrap();
                    } else {
                        shift!([eax, ecx, edx, ebx, esp, ebp, esi, edi][dst]);
                    }
                    // SUB writes every flag, making the shift's flags dead.
                    if dead {
                        a.sub(edx, 19).unwrap();
                    }
                });
                for count in 0..=255 {
                    let mut state = initial();
                    if dst < 8 {
                        state.regs[dst] = 0x8123ffff;
                    }
                    state.regs[1] = (state.regs[1] & !255) | count;
                    let data = [0xff; 0x20];
                    let want = reference(&code, &data, &state);
                    let (got, exit, left) = h.run(&code, &data, &state, false);
                    assert_eq!(exit, Exit::Dispatch);
                    assert_eq!(left, 98 - u64::from(dead) - u64::from(dst == 8));
                    assert_eq!(
                        got, want,
                        "dst={dst}, kind={kind}, dead={dead}, count={count}"
                    );
                }
            }
        }
    }
}

// spec: A64F-FP-6
#[test]
fn comparisons_with_identical_operands_ignore_scalar_upper_snan_lanes() {
    let mut h = Harness::new();
    for scalar in [false, true] {
        for double in [false, true] {
            let width = if double { 8 } else { 4 };
            let mut state = initial();
            // A subnormal low lane sets DE. Upper scalar lanes are never inputs.
            state.xmm[3][..width].copy_from_slice(&1u64.to_le_bytes()[..width]);
            if scalar {
                let snan = if double {
                    0x7ff0000000000001u64
                } else {
                    0x7f800001u64
                };
                for lane in 1..16 / width {
                    state.xmm[3][lane * width..(lane + 1) * width]
                        .copy_from_slice(&snan.to_le_bytes()[..width]);
                }
            }
            for predicate in 0..8 {
                let code = bytes(|a| {
                    a.add(eax, ecx).unwrap();
                    match (scalar, double) {
                        (false, false) => a.cmpps(xmm3, xmm3, predicate),
                        (true, false) => a.cmpss(xmm3, xmm3, predicate),
                        (false, true) => a.cmppd(xmm3, xmm3, predicate),
                        (true, true) => a.cmpsd_3(xmm3, xmm3, predicate),
                    }
                    .unwrap();
                });
                let want = reference(&code, &[], &state);
                let (got, exit, left) = h.run(&code, &[], &state, false);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(left, 98);
                assert_eq!(got, want);
            }
        }
    }
}

fn cmp_code(scalar: bool, double: bool, memory: bool, predicate: i32) -> Vec<u8> {
    bytes(|a| {
        a.add(eax, ecx).unwrap();
        macro_rules! compare {
            ($op:ident, $mem:expr) => {
                if memory {
                    a.$op(xmm0, $mem, predicate)
                } else {
                    a.$op(xmm0, xmm1, predicate)
                }
                .unwrap()
            };
        }
        match (scalar, double) {
            (false, false) => compare!(cmpps, xmmword_ptr(DATA as i32)),
            (true, false) => compare!(cmpss, dword_ptr(DATA as i32)),
            (false, true) => compare!(cmppd, xmmword_ptr(DATA as i32)),
            (true, true) => compare!(cmpsd_3, qword_ptr(DATA as i32)),
        }
    })
}

// spec: A64F-FP-6
#[test]
fn floating_comparison_masks_cover_predicates_lanes_nan_and_denormal_inputs() {
    let mut h = Harness::new();
    for double in [false, true] {
        let values: &[u64] = if double {
            &[
                0,
                1 << 63,
                1,
                0x000fffffffffffff,
                0x0010000000000000,
                0x3ff0000000000000,
                0xbff8000000000000,
                0x7ff0000000000000,
                0xfff0000000000000,
                0x7ff8000000000001,
            ]
        } else {
            &[
                0,
                1 << 31,
                1,
                0x007fffff,
                0x00800000,
                0x3f800000,
                0xbfc00000,
                0x7f800000,
                0xff800000,
                0x7fc00001,
            ]
        };
        let width = if double { 8 } else { 4 };
        for scalar in [false, true] {
            for memory in [false, true] {
                for predicate in 0..=7 {
                    let code = cmp_code(scalar, double, memory, predicate | 0xf8);
                    for (n, &value) in values.iter().enumerate() {
                        let mut state = initial();
                        state.mxcsr |= 0x21; // Preserve existing sticky exception flags.
                        for lane in 0..16 / width {
                            let a = values[(n + lane) % values.len()].to_le_bytes();
                            let b = values[(n + lane + 3) % values.len()].to_le_bytes();
                            state.xmm[0][lane * width..(lane + 1) * width]
                                .copy_from_slice(&a[..width]);
                            state.xmm[1][lane * width..(lane + 1) * width]
                                .copy_from_slice(&b[..width]);
                        }
                        let nan = |v: u64| {
                            if double {
                                v & 0x7ff0000000000000 == 0x7ff0000000000000
                                    && v & 0x000fffffffffffff != 0
                            } else {
                                v & 0x7f800000 == 0x7f800000 && v & 0x007fffff != 0
                            }
                        };
                        let invalid = [1, 2, 5, 6].contains(&predicate)
                            && (0..if scalar { 1 } else { 16 / width }).any(|lane| {
                                nan(values[(n + lane) % values.len()])
                                    || nan(values[(n + lane + 3) % values.len()])
                            });
                        let data = state.xmm[1];
                        let (got, exit, left) = h.run(&code, &data, &state, false);
                        if invalid {
                            // The owner receives the state before this instruction.
                            let prefix = bytes(|a| a.add(eax, ecx).unwrap());
                            let want = reference(&prefix, &data, &state);
                            assert_eq!(exit, Exit::Dispatch);
                            assert_eq!(left, 99);
                            assert_eq!(got, want);
                        } else {
                            let want = reference(&code, &data, &state);
                            assert_eq!(exit, Exit::Dispatch);
                            assert_eq!(left, 98);
                            assert_eq!(
                                got, want,
                                "double={double}, scalar={scalar}, mem={memory}, predicate={predicate}, value={value}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-5
#[test]
fn floating_guard_and_snan_bail_before_destination_or_mxcsr_commit() {
    let mut h = Harness::new();
    for double in [false, true] {
        for scalar in [false, true] {
            for mxcsr in [0x1f80, 0x1fc0, 0x9f80, 0x1f00, 0x1d80, 0x3f80] {
                let mut state = initial();
                state.mxcsr = mxcsr;
                let snan = if double {
                    0x7ff0000000000001u64
                } else {
                    0x7f800001u64
                };
                state.xmm[1][..if double { 8 } else { 4 }]
                    .copy_from_slice(&snan.to_le_bytes()[..if double { 8 } else { 4 }]);
                let code = cmp_code(scalar, double, false, 0);
                let prefix = bytes(|a| a.add(eax, ecx).unwrap());
                let want = reference(&prefix, &[], &state);
                let (got, exit, left) = h.run(&code, &[], &state, false);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(left, 99);
                assert_eq!(got, want);
            }
        }
    }
}

// spec: A64-CTL-6, A64-CTL-10
#[test]
fn switch_table_recognition_checks_unsigned_bounds_pages_and_index_provenance() {
    for (n, ae, byte, clobber, expected) in [
        (7, false, false, false, 8),
        (8, true, false, false, 8),
        (1023, false, false, false, 1024),
        (1024, true, false, false, 1024),
        (1024, false, false, false, 0),
        (0, true, false, false, 0),
        (0, false, true, false, 256),
        (7, false, false, true, 0),
    ] {
        let table = DATA + 4096 - 16;
        let code = bytes(|a| {
            if byte {
                a.movzx(eax, byte_ptr(esi)).unwrap();
            } else {
                a.cmp(eax, n).unwrap();
                if ae {
                    a.jae(CODE as u64 + 0x800).unwrap();
                } else {
                    a.ja(CODE as u64 + 0x800).unwrap();
                }
            }
            a.mov(ecx, 13).unwrap();
            if clobber {
                a.mov(al, 1).unwrap();
            }
            a.jmp(dword_ptr(eax * 4 + table as i32)).unwrap();
        });
        let mut mem = memory(&code, &vec![0; 8192]);
        mem.protect(DATA, 8192, 1).unwrap();
        let mut work = Workspace::default();
        work.unit.images.push((CODE, 0x30000));
        let stubs = Stubs {
            call: HOST_CODE as usize,
            exit: HOST_CODE as usize,
            dispatch: HOST_CODE as usize,
            lookup: HOST_CODE as usize,
            lookup_slow: HOST_CODE as usize,
            ret_miss: HOST_CODE as usize,
            access: HOST_CODE as usize,
            fault: HOST_CODE as usize,
            opt_exit: HOST_CODE as usize,
        };
        for writable_tail in [false, true] {
            if writable_tail {
                mem.protect(DATA + 4096, 4096, 3).unwrap();
            }
            translate(
                &mut work,
                &mut mem,
                CODE,
                &[],
                &|_| false,
                0,
                Features { flagm: false },
                HOST_CODE as usize,
                stubs,
                &|_, _| None,
                &|_| None,
                &|_, _| Vec::new(),
                0x037f,
                false,
            );
            let count = work.unit.tables.first().map_or(0, |t| t.targets.len());
            assert_eq!(
                count,
                if writable_tail { 0 } else { expected },
                "n={n} ae={ae} byte={byte} clobber={clobber} writable_tail={writable_tail}"
            );
            if count != 0 {
                assert!(work.code().contains(&(table, (count * 4) as u32)));
            }
        }
    }
}

// spec: A64-FLAG-5
#[test]
fn narrow_immediate_moves_keep_their_value_when_pf_and_af_materialize() {
    // The register a lazy PF/AF recipe reads is overwritten by a narrow
    // immediate move, so the recipe materializes first; that must not
    // replace the moved value with the recipe's immediate.
    let mut h = Harness::new();
    let regs = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
    let bytes8 = [al, cl, dl, iced_x86::code_asm::bl, ah, ch, dh, bh];
    let words = [ax, cx, dx, bx, sp, bp, si, di];
    for part in 0..16 {
        for recipe in 0..3 {
            let base = if part < 8 { part % 4 } else { part - 8 };
            let code = bytes(|a| {
                let reg = regs[base];
                match recipe {
                    0 => a.cmp(reg, 0x50).unwrap(),
                    1 => a.add(reg, 0x5a).unwrap(),
                    _ => a.sub(reg, 0x12345).unwrap(),
                }
                if part < 8 {
                    a.mov(bytes8[part], 0x90).unwrap();
                } else {
                    a.mov(words[part - 8], 0x9091).unwrap();
                }
            });
            let state = initial();
            let want = reference(&code, &[], &state);
            for flagm in [false, true] {
                let (got, exit, _) = h.run(&code, &[], &state, flagm);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(got, want, "part={part}, recipe={recipe}, flagm={flagm}");
            }
        }
    }
}

fn j1_unit(code: &[u8]) -> Workspace {
    let mut mem = memory(code, &[]);
    let mut work = Workspace::default();
    let at = HOST_CODE as usize;
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[CODE + code.len() as u32],
        &|_| false,
        0,
        Features { flagm: false },
        at,
        Stubs {
            call: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            fault: at,
            opt_exit: at,
        },
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
    );
    work
}

// spec: A64A-FALL-5, A64F-FP-5 A64-REG-4 A64-REG-5
#[cfg(native)]
#[test]
fn register_value_call_preserves_cache_within_the_helper_path_count() {
    let guest = bytes(|a| {
        a.pxor(xmm7, xmm6).unwrap();
        a.addss(xmm0, xmm1).unwrap();
        a.pxor(xmm7, xmm0).unwrap();
    });
    let work = j1_unit(&guest);
    let mut code = Code::default();
    let at = HOST_CODE as usize;
    let cpu = CpuState {
        mxcsr: 0x3fe0,
        ..CpuState::default()
    };
    lower_tier1(
        &mut code,
        &work.unit,
        CODE,
        &|_| false,
        0,
        Features { flagm: false },
        at,
        Stubs {
            call: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            fault: at,
            opt_exit: at,
        },
        false,
        Key::read(&cpu, work.unit.fact_mask()),
        0,
    );
    let begin = code.asm.position(code.labels[1]) / 4;
    let end = code.asm.position(code.labels[2]) / 4;
    let site = &code.asm.words()[begin..end];
    assert_eq!(site.iter().filter(|&&word| word == blr(X16)).count(), 1);
    assert!(site.len() <= 3 + 64);
    for reg in 0..8 {
        let offset = crate::runtime::XMM + 16 * u32::from(reg);
        let v = VReg::new(16 + reg);
        assert!(!site.contains(&(vldur(VMem::Q, v, X5, offset as i32) & !(1 << 22))));
    }
}

// spec: A64-ISA-6, A64-REG-5
#[cfg(native)]
#[test]
fn shared_lock_value_calls_fit_the_helper_path_count_with_live_xmm() {
    for wide in [false, true] {
        let guest = bytes(|a| {
            for dst in [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7] {
                a.pxor(dst, dst).unwrap();
            }
            if wide {
                a.lock().cmpxchg8b(qword_ptr(ebx + 4093)).unwrap();
            } else {
                a.lock().cmpxchg(dword_ptr(ebx), ecx).unwrap();
            }
            a.pxor(xmm7, xmm0).unwrap();
        });
        let work = j1_unit(&guest);
        let mut code = Code::default();
        let at = HOST_CODE as usize;
        lower_tier1(
            &mut code,
            &work.unit,
            CODE,
            &|_| false,
            0,
            Features { flagm: false },
            at,
            Stubs {
                call: at,
                exit: at,
                dispatch: at,
                lookup: at,
                lookup_slow: at,
                ret_miss: at,
                access: at,
                fault: at,
                opt_exit: at,
            },
            false,
            Key::read(&CpuState::default(), work.unit.fact_mask()),
            0,
        );
        let end = code.asm.position(code.labels[9]) / 4;
        let at = code.asm.words()[..end]
            .iter()
            .rposition(|word| word & !31 == str(W, Reg::new(0), RUNTIME, runtime::VALUE_ADDRESS))
            .expect("access stub operand");
        let shared = &code.asm.words()[at..end];
        assert_eq!(shared.iter().filter(|&&word| word == blr(X16)).count(), 1);
        assert!(
            shared.len() <= 3 + 64,
            "shared LOCK path: {} words",
            shared.len()
        );
        for n in [16, 18, 20, 22] {
            let saves = shared.iter().filter(|&&word| {
                word & !(0x7f << 15)
                    == stp_v(
                        VSize::Q,
                        Index::Offset,
                        VReg::new(n),
                        VReg::new(n + 1),
                        Reg::ZR,
                        0,
                    )
            });
            assert_eq!(saves.count(), 1, "XMM pair {} remains cached", n - 16);
        }
    }
}

#[cfg(native)]
fn proved_division_code(memory_divisor: bool, simd: bool) -> Vec<u8> {
    bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        if simd {
            a.paddd(xmm0, xmm1).unwrap();
        }
        for offset in [0, 16] {
            a.mov(eax, esi).unwrap();
            a.cdq().unwrap();
            if memory_divisor {
                a.idiv(dword_ptr(ebp)).unwrap();
            } else {
                a.idiv(ecx).unwrap();
            }
            a.mov(dword_ptr(edi + offset), eax).unwrap();
            a.mov(dword_ptr(edi + offset + 4), edx).unwrap();
            a.mov(eax, edx).unwrap();
            if memory_divisor {
                a.add(eax, dword_ptr(ebp)).unwrap();
            } else {
                a.add(eax, ecx).unwrap();
            }
            a.cdq().unwrap();
            if memory_divisor {
                a.idiv(dword_ptr(ebp)).unwrap();
            } else {
                a.idiv(ecx).unwrap();
            }
            a.mov(dword_ptr(edi + offset + 8), eax).unwrap();
            a.mov(dword_ptr(edi + offset + 12), edx).unwrap();
        }
        if simd {
            a.movdqu(xmmword_ptr(edi + 32), xmm0).unwrap();
        }
        a.add(edi, if simd { 48 } else { 32 }).unwrap();
        a.dec(ebx).unwrap();
        a.jne(top).unwrap();
    })
}

// spec: A64-TR-4 A64-TR-9 A64-VER-1
#[test]
#[cfg(native)]
fn proved_divisions_keep_both_results_flags_memory_and_budget_prefixes() {
    let mut h = Harness::new();
    for memory_divisor in [false, true] {
        let code = proved_division_code(memory_divisor, true);
        for numerator in [i32::MIN, -1_073_741_825, -1, 0, 1, i32::MAX] {
            for divisor in [1u32, 2, 1 << 30] {
                let mut data = vec![0; 0x500];
                data[..4].copy_from_slice(&divisor.to_le_bytes());
                let mut state = initial();
                state.regs[1] = divisor;
                state.regs[3] = 2;
                state.regs[5] = DATA;
                state.regs[6] = numerator as u32;
                state.regs[7] = DATA + 0x400;
                state.xmm[0] = [0x21; 16];
                state.xmm[1] = [0x67; 16];
                let mut reference_memory = memory(&code, &data);
                let mut want = state.clone();
                let mut interpreter = Interpreter::new();
                let mut retired = 0;
                while want.eip != CODE + code.len() as u32 {
                    assert_eq!(
                        interpreter.step(&mut want, &mut reference_memory),
                        adv_cpu_core::interpreter::Step::Executed
                    );
                    retired += 1;
                }
                let want_data = reference_memory.read_data(DATA, data.len()).unwrap();
                for flagm in [false, true] {
                    let (got, exit, left) = h.run_form(&code, &data, &state, flagm, Some(100));
                    assert_eq!((exit, 100 - left), (Exit::Dispatch, retired));
                    assert_eq!(got, want, "n={numerator} d={divisor} mem={memory_divisor}");
                    assert_eq!(
                        h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                        want_data.as_ref()
                    );
                    if numerator == i32::MIN && divisor == 1 << 30
                        || numerator == -1 && divisor == 2
                    {
                        for fuel in 0..retired {
                            let (got, exit, left) =
                                h.run_form(&code, &data, &state, flagm, Some(fuel));
                            assert_eq!(exit, Exit::Recall);
                            let mut want = state.clone();
                            let mut reference_memory = memory(&code, &data);
                            for _ in 0..fuel - left {
                                assert_eq!(
                                    interpreter.step(&mut want, &mut reference_memory),
                                    adv_cpu_core::interpreter::Step::Executed
                                );
                            }
                            assert_eq!(got, want, "fuel={fuel}, left={left}, mem={memory_divisor}");
                            assert_eq!(
                                h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                                reference_memory
                                    .read_data(DATA, data.len())
                                    .unwrap()
                                    .as_ref()
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4 A64-TR-9
#[test]
#[cfg(native)]
fn proved_divisions_leave_unadmitted_values_to_the_ordinary_form() {
    let mut h = Harness::new();
    for memory_divisor in [false, true] {
        let code = proved_division_code(memory_divisor, false);
        let work = j1_unit(&code);
        let proof = computation::Proof::build(&work.unit.trace).unwrap();
        for divisor in [0u32, u32::MAX, (1 << 30) + 1] {
            let mut data = vec![0; 0x500];
            data[..4].copy_from_slice(&divisor.to_le_bytes());
            let mut state = initial();
            state.regs[1] = divisor;
            state.regs[3] = 2;
            state.regs[5] = DATA;
            state.regs[6] = 10;
            state.regs[7] = DATA + 0x400;
            let mem = memory(&code, &data);
            assert!(
                proof
                    .evaluate(&state, &mut computation::Evaluation::default(), |addr| {
                        Some(u32::from_le_bytes(
                            mem.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                        ))
                    })
                    .is_none()
            );
            if divisor != 0 {
                let want = reference(&code, &data, &state);
                for flagm in [false, true] {
                    let (got, exit, _) = h.run(&code, &data, &state, flagm);
                    assert_eq!(exit, Exit::Dispatch);
                    assert_eq!(got, want);
                }
            }
        }
    }
}

// spec: A64-TR-4 A64-TR-9 A64-REG-3 A64-MEM-10
#[test]
#[cfg(native)]
fn proved_divisions_shrink_emitted_sequences_and_retain_dirty_simd_fault_sites() {
    for (memory_divisor, simd) in [(false, false), (true, true)] {
        let guest = proved_division_code(memory_divisor, simd);
        let work = j1_unit(&guest);
        let proof = computation::Proof::build(&work.unit.trace).unwrap();
        let at = HOST_CODE as usize;
        let stubs = Stubs {
            call: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            fault: at,
            opt_exit: at,
        };
        let mut totals = Vec::new();
        for facts in [None, Some(&proof)] {
            let mut code = Code::default();
            lower_computation(
                &work.unit,
                &mut code,
                CODE,
                Features { flagm: false },
                at,
                stubs,
                (0x037f, false, facts),
            )
            .unwrap();
            let mut fixed = 0;
            let mut hot = 0;
            for (index, inst) in work.unit.trace.insts.iter().enumerate() {
                if !matches!(inst.op, Op::Cdq | Op::Integer(integer::Integer::Divide(..))) {
                    continue;
                }
                let begin = code.asm.position(code.labels[index]) / 4;
                let end = code.asm.position(code.labels[index + 1]) / 4;
                let words = &code.asm.words()[begin..end];
                if matches!(inst.op, Op::Cdq) {
                    assert!(words.contains(&sbfx(W, host(2), host(0), 31, 1)));
                    fixed += 1;
                    hot += words.len();
                } else if facts.is_none() {
                    let divide = words
                        .iter()
                        .position(|&w| w == sdiv(X, X17, X16, X14))
                        .unwrap();
                    assert_eq!(
                        words[divide - 5] & 0xff00_0000,
                        cbz(W, false, X0, 0) & 0xff00_0000
                    );
                    assert_eq!(words.len(), divide + 6);
                    fixed += 11;
                    hot += words.len();
                } else {
                    let fixed_words = match proof.division(index).unwrap() {
                        computation::Division::SignExtended => 3,
                        computation::Division::Normalized => 5,
                    };
                    fixed += fixed_words;
                    hot += words.len();
                }
            }
            if facts.is_some() && memory_divisor && simd {
                for inst in work
                    .unit
                    .trace
                    .insts
                    .iter()
                    .filter(|inst| matches!(inst.op, Op::Integer(integer::Integer::Divide(..))))
                {
                    let site = code
                        .sites
                        .iter()
                        .find(|(_, site)| site.eip == inst.pc)
                        .unwrap()
                        .1;
                    assert_ne!(
                        site.simd_dirty & (1 << 8),
                        0,
                        "live XMM0 at the divisor load"
                    );
                }
            }
            totals.push((fixed, hot));
        }
        assert_eq!((totals[0].0, totals[1].0), (48, 20));
        assert!(totals[0].1 - totals[1].1 >= 28);
        eprintln!(
            "four DIV + CDQ: fixed 48 -> 20 words; with wrappers {} -> {} (memory={memory_divisor}, simd={simd})",
            totals[0].1, totals[1].1
        );
    }
}

// spec: A64-FLAG-3, A64-FLAG-5, A64-CTL-7
#[test]
fn j1_hot_paths_omit_dead_logic_tests_and_loop_materialization() {
    let code = bytes(|a| {
        a.or(eax, ebx).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(ecx, 1).unwrap();
    });
    let work = j1_unit(&code);
    assert!(
        !work
            .words()
            .contains(&tst(W, host(0), host(0), Shift::Lsl, 0))
    );
    assert_eq!(work.sites()[0].1.nz, Some(Operand::Guest(0)));
    let code = bytes(|a| {
        a.or(eax, ebx).unwrap();
        a.shl(eax, 3).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(ecx, 1).unwrap();
    });
    let work = j1_unit(&code);
    assert!(!work.words().contains(&mov(W, X10, host(0))));
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let work = j1_unit(&code);
    let start = work.code.asm.position(work.code.labels[0]) / 4;
    let end = work.code.asm.position(work.code.labels[1]) / 4;
    assert_eq!(
        &work.words()[start..end],
        &[sub_imm(W, host(1), host(1), 1, false)]
    );
    assert!(
        work.words()[end..]
            .iter()
            .any(|word| word & 0xff00_001f == cbz(W, true, host(1), 0))
    );
}

// spec: A64F-FP-7
#[test]
fn j1_x87_run_admits_sahf_and_guards_once_until_control_changes() {
    for change in [false, true] {
        let code = bytes(|a| {
            let mut done = a.create_label();
            a.fld1().unwrap();
            a.fld1().unwrap();
            a.fadd_2(st0, st1).unwrap();
            a.fadd_2(st0, st1).unwrap();
            if change {
                a.fldcw(word_ptr(ebx)).unwrap();
            }
            a.fadd_2(st0, st1).unwrap();
            a.fcomp_2(st0, st1).unwrap();
            a.fnstsw(ax).unwrap();
            a.sahf().unwrap();
            a.jb(done).unwrap();
            a.fld1().unwrap();
            a.set_label(&mut done).unwrap();
            a.nop().unwrap();
        });
        let work = j1_unit(&code);
        let hot_end = work
            .code
            .marks
            .iter()
            .find(|mark| mark.part == Part::Cold)
            .unwrap()
            .offset as usize
            / 4;
        let hot = &work.words()[..hot_end];
        let guard = sub_imm(W, X16, temp(1), 0x037f, false);
        assert_eq!(
            hot.iter().filter(|&&word| word == guard).count(),
            if change { 2 } else { 1 }
        );
        assert_eq!(
            hot.iter()
                .filter(|&&word| word == ldrh(X16, X5, runtime::X87_TAG))
                .count(),
            1
        );
    }
}

/// Runs `code` with ESP inside the data window and sticky PE, comparing x87,
/// MMX, integer and SSE state with the interpreter; wide bodies trap.
fn double_loop(h: &mut Harness, code: &[u8], data: &[u8], control: u16) {
    let mut state = initial();
    state.regs[1] = 2;
    state.regs[4] = DATA + 0xff00;
    state.x87.set_control(control);
    // Sticky PE as in a running program: inexact conversions stay native.
    state.x87.status |= 0x20;
    state.mxcsr |= 0x20;
    let expected = reference(code, data, &state);
    let (got, exit, _) = h.run(code, data, &state, false);
    assert_eq!(exit, Exit::Dispatch);
    assert_eq!(got.eip, expected.eip);
    assert_eq!(got.x87, expected.x87);
    assert_eq!(got.mmx, expected.mmx);
    assert_eq!(got.regs, expected.regs);
    assert_eq!(got.eflags, expected.eflags);
    assert_eq!(got.xmm, expected.xmm);
}

// spec: A64F-FP-3, A64F-FP-7
#[test]
fn double_arithmetic_stays_native_after_control_restores_in_loops() {
    // The cached guard decision travels with the control word; a later
    // comparison must not count it as a control bit.
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.fld(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fnstcw(word_ptr(ebx + 16)).unwrap();
        a.movzx(eax, word_ptr(ebx + 16)).unwrap();
        a.or(eax, 0xc00).unwrap();
        a.mov(dword_ptr(ebx + 20), eax).unwrap();
        a.fldcw(word_ptr(ebx + 20)).unwrap();
        a.fistp(qword_ptr(ebx + 24)).unwrap();
        a.fldcw(word_ptr(ebx + 16)).unwrap();
        a.fld(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fstp(qword_ptr(ebx + 32)).unwrap();
        a.movsd_2(xmm1, qword_ptr(ebx + 32)).unwrap();
        a.mov(edx, dword_ptr(ebx + 24)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let mut data = vec![0; 40];
    data[..8].copy_from_slice(&1.25f64.to_le_bytes());
    data[8..16].copy_from_slice(&(-0.5f64).to_le_bytes());
    let mut h = Harness::new();
    for control in [0x007f, 0x027f] {
        double_loop(&mut h, &code, &data, control);
    }
}

// spec: A64F-FP-7
#[test]
fn double_fraction_loops_stay_in_one_run_across_calls_sse_and_truncation() {
    let code = bytes(|a| {
        let (mut top, mut floor, mut convert) =
            (a.create_label(), a.create_label(), a.create_label());
        a.jmp(top).unwrap();
        a.set_label(&mut floor).unwrap();
        a.stmxcsr(dword_ptr(ebx + 40)).unwrap();
        a.fnstcw(word_ptr(ebx + 44)).unwrap();
        a.movsd_2(xmm0, qword_ptr(ebx)).unwrap();
        a.cvttsd2si(eax, xmm0).unwrap();
        a.cvtsi2sd(xmm0, eax).unwrap();
        a.movq(qword_ptr(ebx + 48), xmm0).unwrap();
        a.fld(qword_ptr(ebx + 48)).unwrap();
        a.ret().unwrap();
        a.set_label(&mut convert).unwrap();
        a.push(ebp).unwrap();
        a.mov(ebp, esp).unwrap();
        a.sub(esp, 8).unwrap();
        a.and(esp, -8).unwrap();
        a.fstp(qword_ptr(esp)).unwrap();
        a.cvttsd2si(eax, qword_ptr(esp)).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
        a.set_label(&mut top).unwrap();
        a.call(floor).unwrap();
        a.fsubr(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fld(qword_ptr(ebx + 16)).unwrap();
        a.fmulp(st1, st0).unwrap();
        a.fld1().unwrap();
        a.fsubrp(st1, st0).unwrap();
        a.fld(st0).unwrap();
        a.call(convert).unwrap();
        a.fnstcw(word_ptr(ebx + 24)).unwrap();
        a.movzx(eax, word_ptr(ebx + 24)).unwrap();
        a.or(eax, 0xc00).unwrap();
        a.mov(dword_ptr(ebx + 28), eax).unwrap();
        a.fldcw(word_ptr(ebx + 28)).unwrap();
        a.fistp(qword_ptr(ebx + 32)).unwrap();
        a.fldcw(word_ptr(ebx + 24)).unwrap();
        a.mov(edx, dword_ptr(ebx + 32)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let mut h = Harness::new();
    for x in [0.0f64, -0.0, 2.0, 2.75] {
        let mut data = vec![0; 56];
        for (at, value) in [(0, x), (8, 0.5), (16, 1.25)] {
            data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        double_loop(&mut h, &code, &data, 0x027f);
    }
    let work = j1_unit(&code);
    let marks = work.marks();
    let hot = marks.iter().take_while(|m| m.part != Part::Cold);
    assert_eq!(hot.clone().filter(|m| m.part == Part::X87Entry).count(), 1);
    assert!(!hot.clone().any(|m| m.part == Part::X87Publish));
}

// spec: A64F-FP-5, A64-REG-5
#[test]
fn j1_sse_reuses_mxcsr_and_pe_checks_until_a_control_write() {
    for control_write in [false, true] {
        let code = bytes(|a| {
            a.subss(xmm0, xmm1).unwrap();
            if control_write {
                a.ldmxcsr(dword_ptr(ebx)).unwrap();
                a.subss(xmm0, xmm1).unwrap();
            }
            a.mulss(xmm0, xmm1).unwrap();
            a.addss(xmm0, xmm1).unwrap();
        });
        let work = j1_unit(&code);
        let hot_end = work
            .code
            .marks
            .iter()
            .find(|mark| mark.part == Part::Cold)
            .unwrap()
            .offset as usize
            / 4;
        let loads = work.words()[..hot_end]
            .iter()
            .filter(|&&word| word == ldr(W, X13, X5, runtime::MXCSR))
            .count();
        assert_eq!(loads, if control_write { 4 } else { 2 });
    }
}

// spec: A64-CTL-7, A64-TR-5, A64-REG-5, A64F-FP-4
#[test]
fn j1_forward_internal_jcc_inlines_and_peeled_mmx_loop_minimizes_guards() {
    let branch_pc = CODE + bytes(|a| a.test(ecx, 1).unwrap()).len() as u32;
    let branching_loop = bytes(|a| {
        let (mut top, mut skip) = (a.create_label(), a.create_label());
        a.set_label(&mut top).unwrap();
        a.test(ecx, 1).unwrap();
        a.jz(skip).unwrap();
        a.add(eax, 3).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(ebx, 1).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let work = j1_unit(&branching_loop);
    assert!(
        !work
            .marks()
            .iter()
            .any(|mark| mark.part == Part::Cold && mark.eip == branch_pc),
        "forward internal jcc should inline instead of hopping through Cold::Edge"
    );

    let mmx_loop = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.movq(mm0, qword_ptr(esi)).unwrap();
        a.paddw(mm0, qword_ptr(edi)).unwrap();
        a.movq(qword_ptr(ebx), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.add(ebx, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.mov(eax, eax).unwrap();
    });
    let work = j1_unit(&mmx_loop);
    let marks = work.marks();
    let hot = &marks[..marks.iter().position(|m| m.part == Part::Cold).unwrap()];
    let warm_head_offset = hot
        .iter()
        .rfind(|m| m.part == Part::Body && m.eip == CODE)
        .expect("peeled loop should emit a steady-state warm copy of the head")
        .offset;
    assert!(
        !hot.iter().any(|m| m.part == Part::SimdLoad),
        "peeled write-first loop must eliminate SimdLoad from entry and backedge"
    );
    assert_eq!(
        hot.iter().filter(|m| m.part == Part::SimdGuard).count(),
        2,
        "peeled entry emits one SimdGuard pre-check and one TOP/tag clear"
    );
    assert!(
        !hot.iter()
            .any(|m| m.part == Part::SimdGuard && m.offset >= warm_head_offset),
        "steady-state warm loop must not re-check x87/mmx guard; warm_head_offset={warm_head_offset} hot={hot:?}"
    );
    let warm_end_pc = hot.last().unwrap().eip;
    assert!(
        !hot.iter().any(|m| m.part == Part::SimdPublish
            && m.offset >= warm_head_offset
            && m.eip != warm_end_pc),
        "peeled steady-state backedge must keep MMX resident until loop exit"
    );
}

// spec: A64-TR-9 A64-CTL-8
#[test]
#[cfg(native)]
fn admitted_frame_values_are_reloaded_at_each_outer_iteration() {
    let mut h = Harness::new();
    for overlapping_store in [false, true] {
        let code = bytes(|a| computation::tests::frame_cached_rows(a, overlapping_store));
        let work = j1_unit(&code);
        let proof = computation::Proof::build(&work.unit.trace).expect("nested proof");
        let cache = proof.frame_cache().expect("inner loop residence");
        assert!(cache.offsets.contains(&8) && cache.offsets.contains(&12));
        assert_eq!(cache.offsets.contains(&4), !overlapping_store);

        let mut state = initial();
        state.regs[3] = 3;
        state.regs[4] = DATA + 0x100;
        state.regs[7] = DATA + 0x200;
        let data = vec![0; 0x300];
        let mut interpreter = Interpreter::new();
        let mut want = state.clone();
        let mut mem = memory(&code, &data);
        let mut prefixes = vec![(want.clone(), data.clone())];
        while want.eip != CODE + code.len() as u32 {
            assert_eq!(
                interpreter.step(&mut want, &mut mem),
                adv_cpu_core::interpreter::Step::Executed
            );
            prefixes.push((
                want.clone(),
                mem.read_data(DATA, data.len()).unwrap().to_vec(),
            ));
        }
        let retired = prefixes.len() as u64 - 1;
        for fuel in 0..=retired {
            let (got, exit, left) = h.run_form(&code, &data, &state, false, Some(fuel));
            let completed = fuel - left;
            let (want, bytes) = &prefixes[completed as usize];
            assert_eq!(got, *want, "overlap={overlapping_store} fuel={fuel}");
            assert_eq!(
                h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                *bytes
            );
            assert_eq!(
                exit,
                if completed == retired {
                    Exit::Dispatch
                } else {
                    Exit::Recall
                }
            );
        }
    }
}

// spec: A64-TR-9
#[test]
#[cfg(native)]
fn table_row_loops_admit_gathered_entries_mirrored_pointers_and_inclusive_bounds() {
    // Rows run from a nonzero first row to an inclusive last row. Each
    // reads its source through a row table, biased by a negative offset
    // that wraps the address sum, and its columns through a column table.
    // It updates its output row in place through a pointer that the loop
    // also keeps in a frame slot.
    let code = bytes(|a| {
        let mut outer = a.create_label();
        let mut inner = a.create_label();
        let mut skip = a.create_label();
        a.set_label(&mut outer).unwrap();
        a.mov(ebp, dword_ptr(esp)).unwrap();
        a.mov(edx, dword_ptr(esp + 20)).unwrap();
        a.mov(edx, dword_ptr(edx + ebp * 4)).unwrap();
        a.add(edx, dword_ptr(esp + 32)).unwrap();
        a.xor(ebp, ebp).unwrap();
        a.mov(ecx, dword_ptr(esp + 28)).unwrap();
        a.test(ecx, ecx).unwrap();
        a.jle(skip).unwrap();
        a.set_label(&mut inner).unwrap();
        a.mov(ebx, dword_ptr(esp + 24)).unwrap();
        a.mov(ebx, dword_ptr(ebx + ebp * 4)).unwrap();
        a.movzx(eax, byte_ptr(ebx + edx)).unwrap();
        a.mov(esi, dword_ptr(edi)).unwrap();
        a.add(eax, esi).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, dword_ptr(esp + 16)).unwrap();
        a.inc(ebp).unwrap();
        a.cmp(ebp, ecx).unwrap();
        a.jl(inner).unwrap();
        a.set_label(&mut skip).unwrap();
        a.mov(edx, dword_ptr(esp)).unwrap();
        a.mov(edi, dword_ptr(esp + 8)).unwrap();
        a.add(edi, dword_ptr(esp + 12)).unwrap();
        a.inc(edx).unwrap();
        a.cmp(edx, dword_ptr(esp + 4)).unwrap();
        a.mov(dword_ptr(esp), edx).unwrap();
        a.mov(dword_ptr(esp + 8), edi).unwrap();
        a.jle(outer).unwrap();
    });
    let work = j1_unit(&code);
    let proof = computation::Proof::build(&work.unit.trace).expect("table row proof");
    const FRAME: u32 = 0x100;
    const ROWS: u32 = 0x180;
    const COLUMNS: u32 = 0x1c0;
    const OUTPUT: u32 = 0x200;
    const SOURCE: u32 = 0x300;
    let setup = |first: u32, last: u32, mirror: u32, rows: u32| {
        let mut data = vec![0u8; 0x400];
        let mut put = |at: u32, value: u32| {
            data[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes())
        };
        let output = DATA + OUTPUT + first * 0x20;
        for (slot, value) in [
            first,
            last,
            output,
            0x20,
            4,
            DATA + rows,
            DATA + COLUMNS,
            3,
            0x10u32.wrapping_neg(),
        ]
        .into_iter()
        .enumerate()
        {
            put(FRAME + slot as u32 * 4, value);
        }
        for row in 0..8 {
            put(rows + row * 4, DATA + SOURCE + row * 0x10 + 0x10);
        }
        for (n, offset) in [2, 5, 1].into_iter().enumerate() {
            put(COLUMNS + n as u32 * 4, offset);
        }
        for at in SOURCE..0x400 {
            data[at as usize] = at as u8;
        }
        for at in OUTPUT..SOURCE {
            data[at as usize] = (at * 7) as u8;
        }
        let mut state = initial();
        state.regs[4] = DATA + FRAME;
        state.regs[7] = output + mirror;
        (state, data)
    };
    let admits = |(state, data): &(CpuState, Vec<u8>)| {
        let mem = memory(&code, data);
        proof
            .evaluate(state, &mut computation::Evaluation::default(), |addr| {
                Some(u32::from_le_bytes(
                    mem.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                ))
            })
            .is_some()
    };
    // The pointer must start equal to its slot; an inclusive bound may not
    // be i32::MAX; a first row past the last still runs once in the guest.
    assert!(!admits(&setup(2, 4, 4, ROWS)));
    assert!(!admits(&setup(2, i32::MAX as u32, 0, ROWS)));
    assert!(!admits(&setup(5, 4, 0, ROWS)));
    // Rows may not write the table that locates them.
    assert!(!admits(&setup(2, 4, 0, OUTPUT + 0x40)));
    let mut h = Harness::new();
    let (state, data) = setup(2, 4, 0, ROWS);
    let mut interpreter = Interpreter::new();
    let mut want = state.clone();
    let mut mem = memory(&code, &data);
    let mut prefixes = vec![(want.clone(), data.clone())];
    while want.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut want, &mut mem),
            adv_cpu_core::interpreter::Step::Executed
        );
        prefixes.push((
            want.clone(),
            mem.read_data(DATA, data.len()).unwrap().to_vec(),
        ));
    }
    let retired = prefixes.len() as u64 - 1;
    for fuel in 0..=retired {
        let (got, exit, left) = h.run_form(&code, &data, &state, false, Some(fuel));
        let completed = fuel - left;
        let (want, bytes) = &prefixes[completed as usize];
        assert_eq!(got, *want, "fuel={fuel}");
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
            *bytes
        );
        assert_eq!(
            exit,
            if completed == retired {
                Exit::Dispatch
            } else {
                Exit::Recall
            }
        );
    }
}

pub(in crate::translate) mod opt_back {
    pub(in crate::translate) mod inline;
    use super::super::opt::{back, eval, ir::*, recovery::*};
    use super::super::opt::{ir::Op, recovery::Recipe};
    use super::*;
    use crate::encode::bl;

    mod resident {
        use super::*;
        use crate::translate::opt::front::Assumed;
        use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};

        const ALIAS: u64 = 8 << 32;
        const SHADOW_TOP: u64 = 0x5_5000_1230;
        const CLASSES: u64 = CONTEXT + 0x2000;
        const RAW: u64 = CLASSES + 0x100;
        const LINES: u64 = CLASSES + 0x200;
        const PLAIN: u64 = CLASSES + 0x600;
        const HELPER_VALUE: u32 = 0x2700;
        const HELPER_KIND: u32 = 0x2704;
        const SLOT: u64 = CONTEXT + 0x2800;

        fn start(sources: &[Source]) -> (Func, Block, Vec<Value>) {
            let mut f = Func {
                resident: true,
                kept: (1 << 24) - 1,
                entry: sources.iter().copied().chain([Source::Eflags]).collect(),
                ..Func::default()
            };
            let b = f.block(Some(Head {
                record: RecordId(0),
            }));
            let params = sources.iter().map(|s| f.param(b, s.ty())).collect();
            let flags = f.param(b, Type::Flags);
            f.record(Record {
                eip: CODE,
                refund: 0,
                regs: [None; 8],
                eflags: Some(Location::At(flags)),
                flag_groups: Vec::new(),
                x87: None,
                xmm: [None; 8],
                mmx: [None; 8],
                mmx_meta: None,
                frame: vec![],
            });
            (f, b, params)
        }

        fn state_at(f: &Func, eip: u32, result: Value) -> IrRecord {
            let mut state = f.records[0].clone();
            state.eip = eip;
            state.regs[0] = Some(Location::At(result));
            state
        }

        fn assumptions(cpu: &CpuState) -> Assumed {
            Assumed {
                control: cpu.x87.control,
                status: cpu.x87.status,
                mxcsr: cpu.mxcsr,
                top: 0,
                tags: cpu.x87.tag,
                esp_low: None,
                ebp: None,
                denormal: false,
                x87: false,
                mmx: false,
                mxcsr_mask: 0,
            }
        }

        fn lower(f: &Func, cpu: &CpuState) -> back::Lowered {
            back::lower_resident(f, &assumptions(cpu), &f.entry, false)
        }

        fn word(h: &Harness, address: u64) -> u32 {
            u32::from_le_bytes(h.emu.read(address, 4).try_into().unwrap())
        }

        fn run(
            h: &mut Harness,
            form: &back::Lowered,
            cpu: &CpuState,
            countdown: u64,
            fault: Option<u32>,
        ) -> Exit {
            let mut code = h.prefix.clone();
            // Stand in for the C owner, including its caller-saved clobbers.
            let helper = code.len() * 4;
            let shared_read = helper + 12;
            code.extend([
                str(W, X4, X0, HELPER_KIND),
                tbz(
                    false,
                    X4,
                    runtime::ACCESS_STORE.trailing_zeros(),
                    (shared_read - helper - 4) as i32,
                ),
                str(W, X3, X0, HELPER_VALUE),
                ldr(W, X0, X0, HELPER_VALUE),
            ]);
            for n in 6..16 {
                code.push(movz(W, Reg::new(n), 0xffff, 0));
            }
            code.extend([
                fmov_to_v(X, VReg::new(0), Reg::ZR),
                fmov_to_v(X, VReg::new(31), Reg::ZR),
                vins(3, VReg::new(8), 1, VReg::new(31), 0),
                cmp(W, X0, Reg::ZR, Lsl, 0),
                ret(Reg::LR),
            ]);
            let access = code.len() * 4;
            code.extend(back::access_stub(HOST_CODE + helper as u64));
            let mut leave = |exit: Exit| {
                let at = code.len() * 4;
                code.extend([
                    movz(W, X1, cpu.eip as u16, 0),
                    movk(W, X1, (cpu.eip >> 16) as u16, 16),
                    movz(W, X0, exit.word(), 0),
                    b(h.stubs.exit as i32 - at as i32 - 12),
                ]);
                at
            };
            let miss = leave(Exit::Recall);
            let env_miss = code.len() * 4;
            code.extend([
                movz(W, X1, cpu.eip as u16, 0),
                movk(W, X1, (cpu.eip >> 16) as u16, 16),
                movz(
                    W,
                    X0,
                    Exit::Plan(runtime::PlanEvent::EnvironmentMiss).word(),
                    0,
                ),
                orr(X, X0, X0, X3, Lsl, 32),
                b(h.stubs.exit as i32 - env_miss as i32 - 16),
            ]);
            let at = code.len() * 4;
            let mut words = form.words.clone();
            let branch = |from: usize, to: usize| b(to as i32 - (at + from * 4) as i32);
            words[form.exit] = branch(form.exit, h.stubs.opt_exit);
            words[form.miss] = branch(form.miss, miss);
            words[form.cold] = branch(form.cold, h.forbidden);
            words[form.env_miss] = branch(form.env_miss, env_miss);
            words[form.probation_slot] = SLOT as u32;
            words[form.probation_slot + 1] = (SLOT >> 32) as u32;
            for &(_, literal) in &form.environment {
                let counter = SLOT + 24;
                words[literal] = counter as u32;
                words[literal + 1] = (counter >> 32) as u32;
                h.emu.write(counter, &1u32.to_le_bytes());
            }
            for &point in &form.probation_sensor.as_ref().unwrap().0 {
                words[point] = b(runtime::SENSOR_BYTES as i32);
            }
            for &(offset, _) in &form.links {
                words[offset / 4 + 3] = branch(offset / 4 + 3, h.dispatch);
            }
            code.extend(words);
            let mut runtime = vec![0; size_of::<runtime::Runtime>()];
            for (offset, value) in [
                (offset_of!(runtime::Runtime, cpu), CPU),
                (offset_of!(runtime::Runtime, window), WINDOW_BASE),
                (offset_of!(runtime::Runtime, shadow), SHADOW_TOP),
                (offset_of!(runtime::Runtime, alias), ALIAS),
                (offset_of!(runtime::Runtime, countdown), countdown),
                (offset_of!(runtime::Runtime, recall), CONTEXT),
                (offset_of!(runtime::Runtime, computation_pending), CONTEXT),
                (
                    offset_of!(runtime::Runtime, recovery.saved),
                    CONTEXT + 0x3000,
                ),
                (
                    offset_of!(runtime::Runtime, recovery.cells),
                    form.spill_cells as u64,
                ),
                (offset_of!(runtime::Runtime, classes), CLASSES),
                (offset_of!(runtime::Runtime, alias_classes), RAW),
                (
                    offset_of!(runtime::Runtime, body)
                        + offset_of!(adv_cpu_core::body::BodyContext, classes),
                    CLASSES,
                ),
                (offset_of!(runtime::Runtime, code_lines), LINES),
                (
                    offset_of!(runtime::Runtime, resident_access),
                    HOST_CODE + access as u64,
                ),
                (
                    offset_of!(runtime::Runtime, memory_base),
                    u64::from(CODE) | (80 << 32),
                ),
                (offset_of!(runtime::Runtime, plain_pages), PLAIN),
            ] {
                runtime[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            let limit = offset_of!(runtime::Runtime, completed_limit);
            runtime[limit..limit + 8].copy_from_slice(&h.emu.read(CONTEXT + limit as u64, 8));
            h.emu.write(CONTEXT, &runtime);
            h.emu.write(CONTEXT + u64::from(HELPER_KIND), &[0; 4]);
            h.emu.write(CPU, &state_bytes(cpu));
            h.emu.set_x(X0, CONTEXT);
            h.emu.set_x(X1, HOST_CODE + at as u64);
            h.emu.set_sp(HOST_STACK);
            let result = h.emu.call_at(&code, h.stubs.enter);
            if let Some(eip) = fault {
                assert_eq!(
                    result.unwrap_err(),
                    unicorn_engine::unicorn_const::uc_error::READ_UNMAPPED
                );
                let (offset, site) = form.sites.iter().find(|(_, site)| site.eip == eip).unwrap();
                h.emu
                    .call_loaded_at(at + offset.wrapping_add_signed(site.resume as isize))
                    .unwrap();
            } else {
                result.unwrap();
            }
            assert_eq!(h.emu.sp(), HOST_STACK);
            assert_eq!(
                h.emu
                    .read(CONTEXT + offset_of!(runtime::Runtime, shadow) as u64, 8),
                SHADOW_TOP.to_le_bytes(),
            );
            Exit::from_raw(h.emu.x(X0) as u32).unwrap()
        }

        fn recovered(h: &Harness, f: &back::Lowered, mut cpu: CpuState, id: RecordId) -> CpuState {
            assert_eq!(
                word(
                    h,
                    CONTEXT + offset_of!(runtime::Runtime, recovery.record) as u64
                ),
                id.0
            );
            let dump = CONTEXT + offset_of!(runtime::Runtime, recovery.dump) as u64;
            let x = |i| u64::from_le_bytes(h.emu.read(dump + 8 * i, 8).try_into().unwrap());
            assert_eq!(x(28), SHADOW_TOP);
            let regs = runtime::RegisterDump {
                x: std::array::from_fn(|i| x(i as u64)),
                ..Default::default()
            };
            assert_eq!(f.spill_cells, 0);
            materialize(
                &f.records[id.0 as usize],
                &regs,
                &[],
                &f.flags,
                &mut cpu,
                |_, _| unreachable!("no dirty frame cells"),
            );
            cpu
        }

        fn load(f: &mut Func, b: Block, addr: Value, pc: u32, checked: bool) -> Value {
            f.push(
                b,
                Op::Load {
                    ty: Type::I32,
                    width: 4,
                    addr,
                    checked,
                    pc,
                    clock: false,
                },
            )
            .unwrap()
        }

        fn store(
            f: &mut Func,
            b: Block,
            addr: Value,
            value: Value,
            after: RecordId,
            checked: bool,
        ) {
            f.push(
                b,
                Op::Store {
                    width: 4,
                    addr,
                    value,
                    after,
                    checked,
                    pc: CODE + 6,
                },
            );
        }

        // spec: A64-OPT-9, A64-OPT-7
        #[test]
        fn window_memory_and_fault_recovery() {
            let (mut f, b, p) = start(&[Source::Reg(0), Source::Reg(1), Source::Reg(2)]);
            let first = load(&mut f, b, p[0], CODE + 2, false);
            let after = f.record(state_at(&f, CODE + 8, first));
            store(&mut f, b, p[1], first, after, false);
            let host = f.push(b, Op::Host(p[1])).unwrap();
            let from_frame = f
                .push(
                    b,
                    Op::FrameLoad {
                        ty: Type::I32,
                        base: host,
                        offset: 0,
                    },
                )
                .unwrap();
            let last = load(&mut f, b, p[2], CODE + 10, false);
            let sum = f.push(b, Op::Int(IntOp::Add, last, from_frame)).unwrap();
            let end = f.record(state_at(&f, CODE + 12, sum));
            f.end(b, End::Exit(end));

            let mut cpu = initial();
            cpu.regs[..3].copy_from_slice(&[DATA, DATA + 4, DATA + 8]);
            let form = lower(&f, &cpu);
            assert_eq!(form.sites.len(), 3);
            for &(offset, _) in &form.sites {
                assert_eq!(form.words[offset / 4] >> 5 & 31, 27);
            }
            let mut h = harness();
            h.emu.map(ALIAS + u64::from(DATA), 0x20000);
            h.emu
                .write(WINDOW_BASE + u64::from(DATA), &7u32.to_le_bytes());
            h.emu
                .write(WINDOW_BASE + u64::from(DATA + 8), &11u32.to_le_bytes());
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, CPU), 18);
            assert_eq!(word(&h, WINDOW_BASE + u64::from(DATA + 4)), 7);
            assert_eq!(word(&h, ALIAS + u64::from(DATA + 4)), 0);

            cpu.regs[2] = DATA + 0x10000; // Its alias exists, but its window page is protected.
            assert_eq!(
                run(&mut h, &form, &cpu, 100, Some(CODE + 10)),
                Exit::Optimized
            );
            let mut expected = cpu.clone();
            expected.eip = CODE + 8;
            expected.regs[0] = 7;
            assert_eq!(
                state_bytes(&recovered(&h, &form, cpu, after)),
                state_bytes(&expected)
            );
        }

        // spec: A64-OPT-9, A64-MEM-7
        #[test]
        fn checked_window_alias_owner_and_blocked_paths() {
            let mut sources = vec![Source::Reg(0), Source::Reg(1)];
            sources.extend((0..9).map(|i| Source::Xmm(i % 8)));
            let (mut f, b, p) = start(&sources);
            let first = load(&mut f, b, p[0], CODE + 2, true);
            let after = f.record(f.records[0].clone());
            store(&mut f, b, p[1], first, after, true);
            let mut state = state_at(&f, CODE + 8, first);
            for (dst, &value) in state.xmm.iter_mut().zip(&p[3..11]) {
                *dst = Some(Location::At(value));
            }
            let exit = f.record(state);
            f.end(b, End::Exit(exit));

            let mut cpu = initial();
            for (i, xmm) in cpu.xmm.iter_mut().enumerate() {
                *xmm = [i as u8 + 1; 16];
            }
            let normal = DATA;
            let watched = DATA + 4096;
            let shared = DATA + 0x10000;
            let mut h = harness();
            h.emu.map(ALIAS + u64::from(DATA), 0x20000);
            for (page, class) in [
                (normal, READ | WRITE),
                (watched, READ | WRITE | EXEC),
                (shared, READ | WRITE | SHARED),
            ] {
                h.emu.write(CLASSES + u64::from(page >> 12), &[class]);
                h.emu.write(RAW + u64::from(page >> 12), &[class]);
            }
            h.emu
                .write(LINES + 8 * u64::from(watched >> 12), &1u64.to_le_bytes());
            h.emu
                .write(WINDOW_BASE + u64::from(normal), &0x1234u32.to_le_bytes());
            let form = lower(&f, &cpu);
            assert_eq!(form.sites.len(), 4);
            assert!(form.sites.iter().all(|(_, site)| site.checked));

            cpu.regs[..2].copy_from_slice(&[normal, normal + 4]);
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, WINDOW_BASE + u64::from(normal + 4)), 0x1234);
            assert_eq!(word(&h, CONTEXT + u64::from(HELPER_KIND)), 0);

            cpu.regs[1] = watched + 64;
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, ALIAS + u64::from(watched + 64)), 0x1234);
            assert_eq!(word(&h, WINDOW_BASE + u64::from(watched + 64)), 0);
            assert_eq!(word(&h, CONTEXT + u64::from(HELPER_KIND)), 0);

            cpu.regs[1] = watched;
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Optimized);
            assert_eq!(
                word(
                    &h,
                    CONTEXT + offset_of!(runtime::Runtime, recovery.detail) as u64
                ),
                CODE + 6
            );
            assert_eq!(word(&h, CONTEXT + u64::from(HELPER_KIND)), 0);
            assert_eq!(
                state_bytes(&recovered(&h, &form, cpu.clone(), RecordId(0))),
                state_bytes(&cpu)
            );

            cpu.regs[..2].copy_from_slice(&[shared, shared + 4]);
            h.emu
                .write(CONTEXT + u64::from(HELPER_VALUE), &0x5678u32.to_le_bytes());
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, CPU), 0x5678);
            assert_eq!(
                word(&h, CONTEXT + u64::from(HELPER_KIND)),
                4 | runtime::ACCESS_STORE
            );
            let xmm = h.emu.read(CPU + u64::from(runtime::XMM), 128);
            assert_eq!(
                xmm,
                cpu.xmm[1..]
                    .iter()
                    .chain(&cpu.xmm[..1])
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>()
            );

            cpu.regs[..2].copy_from_slice(&[normal + 4093, normal + 4]);
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, CONTEXT + u64::from(HELPER_KIND)), 4);
            assert_eq!(word(&h, WINDOW_BASE + u64::from(normal + 4)), 0x5678);
        }

        // spec: A64-OPT-9
        #[test]
        #[cfg(unix)]
        fn plain_page_checks_every_page_and_the_requested_permission() {
            let (mut f, b, p) = start(&[
                Source::Reg(0),
                Source::Reg(1),
                Source::Frame {
                    offset: 0,
                    width: 4,
                },
            ]);
            f.push(
                b,
                Op::PlainPage {
                    addr: p[0],
                    bytes: 4,
                    write: false,
                },
            );
            f.push(
                b,
                Op::PlainPage {
                    addr: p[1],
                    bytes: 4,
                    write: true,
                },
            );
            let exit = f.record(state_at(&f, CODE + 8, p[2]));
            f.end(b, End::Exit(exit));
            let mut cpu = initial();
            let last = CODE + 64 * 4096;
            cpu.regs[..2].copy_from_slice(&[CODE, last - 2]);
            cpu.regs[4] = DATA;
            let form = lower(&f, &cpu);
            assert_ne!(form.miss, form.env_miss);
            let mut h = harness();
            h.emu
                .write(WINDOW_BASE + u64::from(DATA), &0x3344u32.to_le_bytes());
            let mut memory = Memory::window(CODE, 80 * 4096).unwrap();
            memory.map(CODE, 80 * 4096, READ | WRITE).unwrap();
            memory.protect(CODE, 4096, READ).unwrap();
            let snapshot = |h: &mut Harness, memory: &mut Memory| {
                // SAFETY: this Memory has two live groups of three bitmap words.
                let bitmap = unsafe { std::slice::from_raw_parts(memory.plain_pages(), 6) };
                h.emu.write(
                    PLAIN,
                    &bitmap
                        .iter()
                        .flat_map(|w| w.to_le_bytes())
                        .collect::<Vec<_>>(),
                );
                h.emu.write(
                    CLASSES + u64::from(CODE >> 12),
                    &memory.native_permissions(),
                );
            };
            snapshot(&mut h, &mut memory);
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            assert_eq!(word(&h, CPU), 0x3344);

            memory.unmap(last, 4096).unwrap();
            memory.map_shared_audio(last, 4096).unwrap();
            snapshot(&mut h, &mut memory);
            cpu.regs[4] = DATA + 0x20000; // A missed fact leaves before reading the frame.
            assert_eq!(
                run(&mut h, &form, &cpu, 100, None),
                Exit::Plan(runtime::PlanEvent::EnvironmentMiss)
            );
            assert_eq!(h.emu.x(X0) >> 32, 1);
            assert_eq!(h.emu.read(CPU, size_of::<CpuState>()), state_bytes(&cpu));
        }

        // spec: A64-OPT-9
        #[test]
        fn page_cross_and_countdown_leave_at_their_exact_record() {
            let (mut f, b, p) = start(&[Source::Reg(0)]);
            f.records[0].refund = 1;
            let next = f.push(b, Op::Constant(Type::I32, 0x1234)).unwrap();
            let old = load(&mut f, b, p[0], CODE + 2, false);
            f.push(
                b,
                Op::PageCross {
                    addr: p[0],
                    width: 4,
                    record: RecordId(0),
                },
            );
            let after = f.record(f.records[0].clone());
            store(&mut f, b, p[0], next, after, false);
            let exit = f.record(state_at(&f, CODE + 9, old));
            f.records[exit.0 as usize].refund = 0;
            f.end(b, End::Exit(exit));
            let mut cpu = initial();
            let mut form = lower(&f, &cpu);
            assert_eq!(form.probation.len(), 3);
            assert!(form.entry_words > 0 && form.exit_words > 0);
            assert_eq!((form.iteration_words, form.iteration_retired), (0, 0));
            let mut h = harness();
            cpu.regs[0] = DATA + 4092;
            assert_eq!(run(&mut h, &form, &cpu, 1, None), Exit::Dispatch);
            assert_eq!(word(&h, CPU + u64::from(runtime::EIP)), CODE + 9);
            assert_eq!(word(&h, WINDOW_BASE + u64::from(cpu.regs[0])), 0x1234);

            cpu.regs[0] += 1;
            assert_eq!(run(&mut h, &form, &cpu, 1, None), Exit::Optimized);
            assert_eq!(
                state_bytes(&recovered(&h, &form, cpu.clone(), RecordId(0))),
                state_bytes(&cpu)
            );
            assert_eq!(word(&h, WINDOW_BASE + u64::from(cpu.regs[0])), 0x12);
            assert_eq!(run(&mut h, &form, &cpu, 0, None), Exit::Dispatch);
            assert_eq!(h.emu.read(CPU, size_of::<CpuState>()), state_bytes(&cpu));
            assert_eq!(word(&h, SLOT + 8), 1);
            for &word in &form.probation {
                form.words[word] = NOP;
            }
            cpu.regs[0] -= 1;
            assert_eq!(run(&mut h, &form, &cpu, 1, None), Exit::Dispatch);
            assert_eq!(word(&h, SLOT), 0);
            assert_eq!(word(&h, SLOT + 8), 1);
        }

        // spec: A64-OPT-8, A64A-PLACE-3
        #[test]
        fn probation_prices_the_settled_exit() {
            let (mut f, block, values) = start(&[Source::Reg(0)]);
            let eip = CODE + 8;
            let exit = f.record(state_at(&f, eip, values[0]));
            f.end(block, End::Exit(exit));
            let form = lower(&f, &initial());
            let start = form
                .marks
                .iter()
                .find(|mark| mark.eip == eip)
                .unwrap()
                .offset
                / 4;
            let sensors = &form.probation_sensor.as_ref().unwrap().0;
            let [sensor] = sensors.as_slice() else {
                panic!("one normal exit");
            };
            assert_eq!(form.exit_words, *sensor as u32 - start + 1);
        }

        // spec: A64-OPT-9
        #[test]
        fn probation_accounts_for_a_refill_before_a_normal_exit() {
            let (mut f, head, p) = start(&[Source::Reg(0)]);
            f.head_exit = Some(head);
            let tail = f.block(None);
            let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
            let left = f.push(head, Op::Int(IntOp::Sub, p[0], one)).unwrap();
            let flags = f.blocks[0].params[1];
            f.end(
                head,
                End::Branch {
                    cond: left,
                    taken: Target {
                        block: head,
                        args: vec![left, flags],
                        charge: 3,
                    },
                    fall: Target {
                        block: tail,
                        args: vec![],
                        charge: 0,
                    },
                },
            );
            let exit = f.record(state_at(&f, CODE + 8, left));
            f.end(tail, End::Exit(exit));
            let mut cpu = initial();
            cpu.regs[0] = 2;
            let form = lower(&f, &cpu);
            let mut h = harness();
            h.emu.write(
                CONTEXT + offset_of!(runtime::Runtime, completed_limit) as u64,
                &u64::MAX.to_le_bytes(),
            );
            assert_eq!(run(&mut h, &form, &cpu, 1, None), Exit::Dispatch);
            assert_eq!(word(&h, CPU), 0);
            assert_eq!(
                word(&h, CONTEXT + offset_of!(runtime::Runtime, completed) as u64),
                runtime::COMPUTATION_SLICE + 2
            );
            assert_eq!(word(&h, SLOT + 8), 3);
        }

        // spec: A64-OPT-9
        #[test]
        fn ebp_relative_to_esp_is_a_context_fact() {
            let (mut f, b, _) = start(&[]);
            let exit = f.record(f.records[0].clone());
            f.end(b, End::Exit(exit));
            let mut cpu = initial();
            let mut assumed = assumptions(&cpu);
            assumed.ebp = Some(-12);
            cpu.regs[4] = 8;
            cpu.regs[5] = 8_u32.wrapping_sub(12);
            let form = back::lower_resident(&f, &assumed, &f.entry, false);
            let mut h = harness();
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Dispatch);
            cpu.regs[5] ^= 1;
            assert_eq!(run(&mut h, &form, &cpu, 100, None), Exit::Recall);
            assert_eq!(h.emu.read(CPU, size_of::<CpuState>()), state_bytes(&cpu));
        }

        // spec: A64-OPT-9
        #[test]
        fn frame_disjoint_checks_both_ends_of_a_wrapping_interval() {
            let (mut f, b, p) = start(&[Source::Reg(0), Source::Reg(4)]);
            f.push(
                b,
                Op::FrameDisjoint {
                    addr: p[0],
                    width: 4,
                    base: p[1],
                    lo: -4,
                    hi: 8,
                    record: RecordId(0),
                },
            );
            let after = f.record(f.records[0].clone());
            store(&mut f, b, p[0], p[1], after, false);
            let exit = f.record(f.records[0].clone());
            f.end(b, End::Exit(exit));
            let mut cpu = initial();
            cpu.regs[4] = 0xffff_fffc;
            let form = lower(&f, &cpu);
            let mut h = harness();
            h.emu.map(WINDOW_BASE, 0x1000);
            h.emu.map(WINDOW_BASE + 0xffff_f000, 0x1000);
            for (addr, expected) in [
                (0xffff_fff4, Exit::Dispatch),
                (0xffff_fff5, Exit::Optimized),
                (3, Exit::Optimized),
                (4, Exit::Dispatch),
            ] {
                cpu.regs[0] = addr;
                assert_eq!(run(&mut h, &form, &cpu, 100, None), expected);
                if expected == Exit::Optimized {
                    assert_eq!(
                        state_bytes(&recovered(&h, &form, cpu.clone(), RecordId(0))),
                        state_bytes(&cpu)
                    );
                }
            }
        }

        // spec: A64-OPT-9
        #[test]
        fn iteration_cost_carries_the_retirement_of_the_longest_path() {
            let (mut f, head, p) = start(&[Source::Reg(0)]);
            f.head_exit = Some(head);
            let slow = f.block(None);
            f.blocks[head.0 as usize].pcs.push(CODE);
            f.blocks[slow.0 as usize].pcs.push(CODE + 4);
            let flags = f.blocks[0].params[1];
            f.end(
                head,
                End::Branch {
                    cond: p[0],
                    taken: Target {
                        block: head,
                        args: vec![p[0], flags],
                        charge: 1,
                    },
                    fall: Target {
                        block: slow,
                        args: vec![],
                        charge: 0,
                    },
                },
            );
            let value = load(&mut f, slow, p[0], CODE + 4, false);
            let sum = f.push(slow, Op::Int(IntOp::Add, p[0], value)).unwrap();
            f.end(
                slow,
                End::Jump(Target {
                    block: head,
                    args: vec![sum, flags],
                    charge: 4,
                }),
            );
            let form = lower(&f, &initial());
            assert_eq!(form.iteration_retired, 4);
            assert!(form.iteration_words >= 5);
            assert_eq!(form.iteration_pcs, [CODE, CODE + 4]);
        }
    }

    const AREA: u64 = 0x4000_0000;
    const PARAMETER: u64 = AREA;

    struct Captured {
        cpu: CpuState,
        frame: Vec<(i32, Vec<u8>)>,
        record: RecordId,
        countdown: u64,
        completed: u64,
        registers: runtime::RegisterDump,
        spills: Vec<u128>,
        lowered: back::Lowered,
    }

    impl Harness {
        fn run_opt(
            &mut self,
            func: &Func,
            params: &[u128],
            budget: u64,
            recall: u32,
            head_exit: u32,
        ) -> Captured {
            inline::check_backend_snapshots(func);
            self.run_opt_inner(func, params, budget, recall, head_exit)
        }

        fn run_opt_inner(
            &mut self,
            func: &Func,
            params: &[u128],
            budget: u64,
            recall: u32,
            head_exit: u32,
        ) -> Captured {
            let mut code = self.prefix.clone();
            let wrapper = code.len() * 4;
            code.extend_from_slice(&[
                stp(X, Index::Pre, Reg::LR, Reg::ZR, Reg::new(31), -16),
                msr_fpcr(Reg::ZR),
                bl(self.stubs.enter as i32 - (wrapper + 8) as i32),
                mrs_fpcr(X16),
                ldp(X, Index::Post, Reg::LR, Reg::ZR, Reg::new(31), 16),
                ret(Reg::LR),
            ]);
            let at = code.len() * 4;
            let mut lowered = back::lower(func);
            lowered.place(at, self.stubs.opt_exit);
            code.extend_from_slice(&lowered.words);
            let mut context = vec![0; size_of::<runtime::Runtime>()];
            let mut put = |offset: usize, value: &[u8]| {
                context[offset..offset + value.len()].copy_from_slice(value)
            };
            for (offset, value) in [
                (offset_of!(runtime::Runtime, cpu), CPU),
                (offset_of!(runtime::Runtime, window), WINDOW_BASE),
                (offset_of!(runtime::Runtime, shadow), WINDOW_BASE),
                (offset_of!(runtime::Runtime, countdown), budget),
                (offset_of!(runtime::Runtime, completed_limit), u64::MAX),
                (offset_of!(runtime::Runtime, opt_params), PARAMETER),
                (offset_of!(runtime::Runtime, recall), CONTEXT + 0x3f00),
                (
                    offset_of!(runtime::Runtime, computation_pending),
                    CONTEXT + 0x3f04,
                ),
            ] {
                put(offset, &value.to_le_bytes());
            }
            put(
                offset_of!(runtime::Runtime, recall_mask),
                &1u32.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, computation_pending_mask),
                &2u32.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, head_exit),
                &head_exit.to_le_bytes(),
            );
            self.emu.write(CONTEXT, &context);
            self.emu.write(CONTEXT + 0x3f00, &recall.to_le_bytes());
            self.emu
                .write(CONTEXT + 0x3f04, &(recall & 2).to_le_bytes());
            self.emu.write(
                PARAMETER,
                &params
                    .iter()
                    .flat_map(|p| p.to_le_bytes())
                    .collect::<Vec<_>>(),
            );
            let state = vec![0; size_of::<CpuState>()];
            self.emu.write(CPU, &state);
            self.emu.set_sp(HOST_STACK);
            self.emu.set_x(X0, CONTEXT);
            self.emu.set_x(X1, HOST_CODE + at as u64);
            self.emu.set_x(Reg::new(18), 0x1818);
            for n in 19..30 {
                self.emu.set_x(Reg::new(n), 0xcafe_0000 + u64::from(n));
            }
            for n in 8..16 {
                self.emu.set_q(n, 0xbeef_0000 + u128::from(n));
            }
            let result = self.emu.call_at(&code, wrapper);
            if result.is_err() {
                // Model signal-context capture and resume through opt_exit. This
                // test has a single faulting site; native signal wiring is separate.
                assert_eq!(lowered.sites.len(), 1);
                let id = lowered.sites[0].1.opt_record.unwrap();
                self.emu.write(
                    CONTEXT + offset_of!(runtime::Runtime, recovery.record) as u64,
                    &id.to_le_bytes(),
                );
                let mut tail = code.clone();
                let fault = tail.len() * 4;
                tail.push(b(self.stubs.opt_exit as i32 - fault as i32));
                self.emu
                    .call_at(&tail, fault)
                    .expect("fault recovery returns");
            }
            assert_eq!(self.emu.x(X16), 0, "canonical FPCR");
            assert_eq!(self.emu.x(Reg::new(18)), 0x1818);
            for n in 19..30 {
                assert_eq!(
                    self.emu.x(Reg::new(n)),
                    0xcafe_0000 + u64::from(n),
                    "host x{n}"
                );
            }
            for n in 8..16 {
                assert_eq!(
                    self.emu.q(n) as u64,
                    0xbeef_0000 + u64::from(n),
                    "host d{n}"
                );
            }
            assert_eq!(self.emu.sp(), HOST_STACK);
            assert_eq!(
                self.emu.read(CPU, state.len()),
                state,
                "no canonical write-back"
            );
            let context = self.emu.read(CONTEXT, context.len());
            let word =
                |offset: usize| u32::from_le_bytes(context[offset..][..4].try_into().unwrap());
            let wide =
                |offset: usize| u64::from_le_bytes(context[offset..][..8].try_into().unwrap());
            assert_eq!(self.emu.x(X0), u64::from(Exit::Optimized.raw()));
            let base = offset_of!(runtime::Runtime, recovery.dump);
            let registers = runtime::RegisterDump {
                sp: wide(base + offset_of!(runtime::RegisterDump, sp)),
                x: std::array::from_fn(|i| wide(base + i * 8)),
                v: std::array::from_fn(|i| {
                    let offset = base + offset_of!(runtime::RegisterDump, v) + i * 16;
                    u128::from_le_bytes(context[offset..][..16].try_into().unwrap())
                }),
            };
            let spills = self
                .emu
                .read(registers.sp, lowered.spill_cells * 16)
                .as_chunks::<16>()
                .0
                .iter()
                .map(|b| u128::from_le_bytes(*b))
                .collect::<Vec<_>>();
            let record = RecordId(word(offset_of!(runtime::Runtime, recovery.record)));
            let mut cpu = initial();
            let mut frame = Vec::new();
            let refund = materialize(
                &lowered.records[record.0 as usize],
                &registers,
                &spills,
                &lowered.flags,
                &mut cpu,
                |offset, bytes| frame.push((offset, bytes.to_vec())),
            );
            Captured {
                cpu,
                frame,
                record,
                countdown: self
                    .emu
                    .x(X1)
                    .wrapping_add(wide(offset_of!(runtime::Runtime, completed)))
                    .wrapping_add(u64::from(refund)),
                completed: wide(offset_of!(runtime::Runtime, completed)),
                registers,
                spills,
                lowered,
            }
        }
    }

    fn checked_lower(func: &Func) -> back::Lowered {
        inline::check_backend_snapshots(func);
        back::lower(func)
    }

    fn harness() -> Harness {
        let mut h = Harness::new();
        h.emu.map(AREA, 0x20000);
        h
    }

    fn record(eip: u32) -> IrRecord {
        IrRecord {
            eip,
            refund: 0,
            regs: [Some(Location::Constant(0)); 8],
            eflags: Some(Location::Constant(0x202)),
            flag_groups: Vec::new(),
            x87: Some(X87 {
                top: 0,
                valid: 0,
                control: 0x027f,
                stack: [None; 8],
                status: 0x20,
                c1: Some(Location::Constant(0)),
                ip: Some(Location::Constant(0x1234)),
                opcode: Some(Location::Constant(0x321)),
            }),
            xmm: [Some(Location::Constant(0)); 8],
            mmx: [Some(Location::Constant(0)); 8],
            mmx_meta: None,
            frame: Vec::new(),
        }
    }

    // spec: A64-OPT-4
    #[test]
    fn opt_back_fp_state_is_never_accessed() {
        // Lowering checks every form built by the back-end and front-end
        // tests. The entry and optimized exit stubs complete that path.
        let stubs = stubs::stubs();
        back::assert_no_fp_state(&stubs.code[stubs.enter / 4..stubs.exit / 4]);
        back::assert_no_fp_state(&stubs.code[stubs.opt_exit / 4..]);
    }

    fn floating_pairs() -> [(f64, f64); 23] {
        let near = |x: f64| {
            let scale = 2f64.powi(-511);
            ((1.0 - x) * scale, (1.0 + x) * scale)
        };
        let min = f64::MIN_POSITIVE;
        let max = f64::MAX;
        let inf = f64::INFINITY;
        let sub = f64::from_bits(1);
        let quiet = f64::from_bits(0xfff8_0000_0000_0123);
        let signaling = f64::from_bits(0x7ff0_0000_0000_0456);
        [
            (1.5, -2.25),
            (0.0, -0.0),
            (-0.0, -2.0),
            (min, 1.0),
            (-min, 0.5),
            (min, -min.next_up()),
            (sub, -sub),
            (-sub, max),
            (1.0, sub),
            (max, max),
            (-max, -max),
            (inf, -inf),
            (-inf, 0.0),
            (1.0, inf),
            (1.0, -0.0),
            (quiet, f64::NAN),
            (1.0, quiet),
            (signaling, quiet),
            (quiet, signaling),
            (1.0, signaling),
            (min, max),
            near(2f64.powi(-27) + 2f64.powi(-29)),
            near(2f64.powi(-27)),
        ]
    }

    fn guest_fp_form(code: &[u8], at: &CpuState) -> opt::front::Built {
        let mut unit = Unit::default();
        unit.build(
            &mut memory(code, &[]),
            CODE,
            &[CODE + code.len() as u32],
            &|_, _| None,
            &|_| None,
            true,
        );
        let mut proof = computation::Proof::build(&unit.trace).unwrap();
        proof.prove_split(&unit.trace, |pc, len| {
            let start = (pc - CODE) as usize;
            let bytes = &code[start..start + len as usize];
            crate::translate::front::summary::from_bytes(bytes, pc)
        });
        let built = opt::front::build(&unit, &proof, at).unwrap();
        inline::check_guest_snapshots(&built.func, &built.assumed, at);
        built
    }

    fn guest_fp_pair(
        h: &mut Harness,
        code: &[u8],
        built: &opt::front::Built,
        at: &CpuState,
        data: &[u8; 48],
        frame: &[u8; 8],
    ) -> bool {
        let frame_addr = at.regs[4] - 8;
        let mut reference = memory(code, data);
        reference.write(frame_addr, frame).unwrap();
        let params = opt::admit(&built.func.entry, &built.assumed, at, |addr, width| {
            let mut bytes = [0; 8];
            reference
                .read_into(addr, &mut bytes[..width as usize])
                .unwrap();
            u64::from_le_bytes(bytes)
        })
        .unwrap();
        let mut ir_memory = memory(code, data);
        ir_memory.write(frame_addr, frame).unwrap();
        let run = eval::run(&built.func, &params, &mut ir_memory, 1000);
        let (want_record, failed) = match run.outcome {
            eval::Outcome::Exit(record) => (record, false),
            eval::Outcome::Fail(record) => (record, true),
            _ => panic!("optimized form did not finish"),
        };

        h.emu.write(WINDOW_BASE + u64::from(DATA), data);
        h.emu.write(WINDOW_BASE + u64::from(frame_addr), frame);
        if !failed {
            inline::compare_guest_exit(built, at, &reference, want_record);
        }
        let captured = h.run_opt_inner(&built.func, &params, 1000, 0, 0);
        assert_eq!(captured.record, want_record);
        let data_after = h.emu.read(WINDOW_BASE + u64::from(DATA), data.len());
        let mut actual = memory(code, &data_after);
        actual.write(frame_addr, frame).unwrap();
        let mut got = at.clone();
        materialize(
            &captured.lowered.records[captured.record.0 as usize],
            &captured.registers,
            &captured.spills,
            &captured.lowered.flags,
            &mut got,
            |offset, bytes| {
                actual
                    .write(at.regs[4].wrapping_add_signed(offset), bytes)
                    .unwrap()
            },
        );

        let stop = CODE + code.len() as u32;
        let mut want = at.clone();
        let mut interpreter = Interpreter::new();
        for (cpu, memory) in [(&mut got, &mut actual), (&mut want, &mut reference)] {
            for _ in 0..100 {
                if cpu.eip == stop {
                    break;
                }
                assert_eq!(
                    interpreter.step(cpu, memory),
                    adv_cpu_core::interpreter::Step::Executed,
                );
            }
            assert_eq!(cpu.eip, stop);
        }
        assert_eq!(got.regs, want.regs);
        assert_eq!(got.eflags, want.eflags);
        assert_eq!(got.xmm, want.xmm);
        assert_eq!(got.mxcsr, want.mxcsr);
        assert_eq!(got.x87.status & !0x200, want.x87.status & !0x200);
        assert_eq!(got.x87.tag, want.x87.tag);
        assert_eq!(
            actual.read_data(DATA, data.len()).unwrap(),
            reference.read_data(DATA, data.len()).unwrap()
        );
        failed
    }

    // spec: A64-OPT-4, A64-OPT-5, A64-OPT-6, EXEC-CORE-9
    #[test]
    fn opt_back_guest_sse_value_pairs() {
        let mut h = harness();
        let mut at = initial();
        let pairs = floating_pairs();
        at.regs[7] = DATA + 32;
        h.emu
            .map(WINDOW_BASE + u64::from(at.regs[4] - 0x1000), 0x1000);
        for op in 0..12 {
            let code = bytes(|a| {
                let mut head = a.create_label();
                a.set_label(&mut head).unwrap();
                if op == 9 {
                    a.mov(dword_ptr(edi + 8), ecx).unwrap();
                }
                match op {
                    0 => a.addsd(xmm0, xmm1),
                    1 => a.subsd(xmm0, xmm1),
                    2 => a.mulsd(xmm0, xmm1),
                    3 => a.divsd(xmm0, xmm1),
                    4 => a.cmpltpd(xmm0, xmm1),
                    5 => a.ucomisd(xmm0, xmm1),
                    6 => a.comisd(xmm0, xmm1),
                    7 => a.cvttsd2si(eax, xmm0),
                    8 => a.cvtsi2sd(xmm0, edx),
                    9 => a.addsd(xmm0, qword_ptr(esi)),
                    10 => a.addsd(xmm0, qword_ptr(esp - 8)),
                    _ => {
                        a.cvtsi2sd(xmm1, edx).unwrap();
                        a.addsd(xmm0, xmm1).unwrap();
                        a.cvttsd2si(eax, xmm0)
                    }
                }
                .unwrap();
                if matches!(op, 5 | 6) {
                    a.adc(eax, 0).unwrap();
                }
                a.movsd_2(qword_ptr(edi), xmm0).unwrap();
                a.dec(ecx).unwrap();
                a.jne(head).unwrap();
            });
            for mxcsr in [0x1fa0, 0x1fa2] {
                at.mxcsr = mxcsr;
                let built = guest_fp_form(&code, &at);
                if op == 11 {
                    let checks: Vec<_> = built
                        .func
                        .blocks
                        .iter()
                        .flat_map(|block| &block.insts)
                        .filter_map(|(_, op)| match op {
                            Op::SseInput { b, .. } => Some(b),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(checks.len(), usize::from(mxcsr & 2 == 0));
                    assert!(checks.iter().all(|b| b.is_none()));
                }
                let extra = [
                    (i32::MIN as f64 - 0.75, 0.0),
                    (i32::MIN as f64 - 1.0, 0.0),
                    (-(i32::MIN as f64), 0.0),
                ];
                for (i, (lhs, rhs)) in pairs.into_iter().chain(extra).enumerate() {
                    at.regs[2] = [0, 1, u32::MAX, i32::MIN as u32, i32::MAX as u32][i % 5];
                    let lanes = |lo: f64, hi: f64| {
                        (u128::from(lo.to_bits()) | u128::from(hi.to_bits()) << 64).to_le_bytes()
                    };
                    at.xmm[0] = if op == 4 && i % 2 == 0 {
                        lanes(2.0, lhs)
                    } else {
                        lanes(lhs, 2.0)
                    };
                    at.xmm[1] = if op == 4 && i % 2 == 0 {
                        lanes(1.0, rhs)
                    } else {
                        lanes(rhs, 1.0)
                    };
                    let mut data = [0; 48];
                    data[..8].copy_from_slice(&rhs.to_le_bytes());
                    let failed =
                        guest_fp_pair(&mut h, &code, &built, &at, &data, &rhs.to_le_bytes());
                    if op == 4 && (lhs.is_subnormal() || rhs.is_subnormal()) {
                        assert_eq!(failed, mxcsr & 2 == 0);
                    }
                }
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-5, A64-OPT-6
    #[test]
    fn opt_back_guest_x87_value_pairs() {
        let mut h = harness();
        let mut at = initial();
        h.emu
            .map(WINDOW_BASE + u64::from(at.regs[4] - 0x1000), 0x1000);
        for op in 0..9 {
            let mut pairs: Vec<(u64, u64)> = floating_pairs()
                .into_iter()
                .map(|(a, b)| (a.to_bits(), b.to_bits()))
                .collect();
            let code = bytes(|a| {
                let mut head = a.create_label();
                a.set_label(&mut head).unwrap();
                match op {
                    6 => a.fild(qword_ptr(esi)),
                    8 => a.fld(dword_ptr(esi)),
                    _ => a.fld(qword_ptr(esi)),
                }
                .unwrap();
                match op {
                    0 => a.fadd(qword_ptr(esi + 8)),
                    1 => a.fsub(qword_ptr(esi + 8)),
                    2 => a.fmul(qword_ptr(esi + 8)),
                    3 => a.fdiv(qword_ptr(esi + 8)),
                    _ => Ok(()),
                }
                .unwrap();
                match op {
                    4 => a.fistp(dword_ptr(edi)),
                    5 => a.fistp(qword_ptr(edi)),
                    _ => a.fstp(qword_ptr(edi)),
                }
                .unwrap();
                a.dec(ecx).unwrap();
                a.jne(head).unwrap();
            });
            at.regs[7] = DATA + 32;
            at.x87.control = if matches!(op, 4 | 5) { 0x0e7f } else { 0x027f };
            if op == 4 {
                pairs.extend(
                    [
                        i32::MIN as f64 - 0.75,
                        i32::MIN as f64 - 1.0,
                        -(i32::MIN as f64),
                    ]
                    .map(|a| (a.to_bits(), 0)),
                );
            } else if op == 5 {
                let min = i64::MIN as f64;
                pairs.extend(
                    [min.next_down(), min, -min, (-min).next_down()].map(|a| (a.to_bits(), 0)),
                );
            } else if op == 6 {
                pairs.clear();
                for n in [
                    1i64,
                    (1 << 53) - 1,
                    1 << 53,
                    (1 << 53) + 1,
                    (1 << 53) + 2,
                    i64::MAX,
                ] {
                    pairs.extend([(n as u64, 0), ((-n) as u64, 0)]);
                }
                pairs.push((i64::MIN as u64, 0));
            } else if op == 8 {
                pairs = floating_pairs()
                    .into_iter()
                    .map(|(a, b)| (((a as f32).to_bits() as u64), (b as f32).to_bits() as u64))
                    .collect();
                pairs.extend([(1, 0), (0x8000_0001, 0), (0x7f80_0001, 0)]);
            }
            for status in [0x20, 0x22] {
                at.x87.status = status;
                let built = guest_fp_form(&code, &at);
                for (lhs, rhs) in pairs.iter().copied() {
                    let mut data = [0; 48];
                    data[..8].copy_from_slice(&lhs.to_le_bytes());
                    data[8..16].copy_from_slice(&rhs.to_le_bytes());
                    guest_fp_pair(&mut h, &code, &built, &at, &data, &[0; 8]);
                }
            }
        }
    }

    fn start(types: &[Type]) -> (Func, Block, Vec<Value>, RecordId) {
        let mut f = Func::default();
        let entry = f.record(record(0x1000));
        let b = f.block(Some(Head { record: entry }));
        let values = types.iter().map(|&ty| f.param(b, ty)).collect();
        f.entry = types
            .iter()
            .map(|ty| match ty {
                Type::I32 => Source::Reg(0),
                Type::I64 => Source::Frame {
                    offset: 0,
                    width: 8,
                },
                Type::F64 => Source::X87(0),
                Type::V128 => Source::Xmm(0),
                Type::Flags => Source::Eflags,
            })
            .collect();
        (f, b, values, entry)
    }

    fn finish(f: &mut Func, b: Block, value: Value) -> RecordId {
        let mut r = record(0x2000);
        match f.ty(value) {
            Type::I32 | Type::I64 => {
                r.regs[0] = Some(Location::At(value));
                r.frame.push((-8, 8, Location::At(value)));
            }
            Type::Flags => r.eflags = Some(Location::Recipe(Recipe::Eflags(value))),
            Type::F64 => {
                let x87 = r.x87.as_mut().unwrap();
                x87.valid = 1;
                x87.stack[0] = Some(Location::At(value));
            }
            Type::V128 => r.xmm[0] = Some(Location::At(value)),
        }
        let exit = f.record(r);
        f.end(b, End::Exit(exit));
        exit
    }

    fn compare(f: &Func, params: &[u128], h: &mut Harness) -> Captured {
        let mut memory = memory(&[], &[]);
        let run = eval::run(f, params, &mut memory, 10000);
        let got = h.run_opt(f, params, 10000, 0, 0);
        let want = match run.outcome {
            eval::Outcome::Exit(r) | eval::Outcome::Fail(r) => r,
            _ => panic!("reference terminates"),
        };
        assert_eq!(got.record, want);
        compare_record(f, &run.values, &got);
        assert_eq!(
            10000u64
                .wrapping_sub(got.countdown)
                .wrapping_add(got.completed),
            (run.charged - i64::from(f.records[want.0 as usize].refund)) as u64
        );
        got
    }

    fn compare_record(f: &Func, values: &[u128], got: &Captured) {
        let r = &f.records[got.record.0 as usize];
        let x87 = r.x87.as_ref().expect("harness records hold x87 state");
        let locate = |l: Location<Value>| eval::locate(&l, values);
        for (i, l) in r.regs.iter().enumerate() {
            if let Some(l) = l {
                assert_eq!(got.cpu.regs[i], locate(*l) as u32, "reg {i}");
            }
        }
        if let Some(l) = r.eflags {
            assert_eq!(got.cpu.eflags, locate(l) as u32, "eflags");
        }
        assert_eq!(got.cpu.eip, r.eip);
        assert_eq!(got.cpu.x87.top(), x87.top as usize);
        assert_eq!(got.cpu.x87.control, x87.control);
        for i in 0..8 {
            if x87.valid >> i & 1 != 0 {
                if x87.stack[i].is_some() {
                    assert_eq!(got.cpu.x87.tag >> (2 * i) & 3, got.cpu.x87.regs[i].tag());
                }
            } else {
                assert_eq!(
                    got.cpu.x87.tag >> (2 * i) & 3,
                    initial().x87.tag >> (2 * i) & 3
                );
            }
        }
        assert_eq!(
            got.cpu.x87.status & !((7 << 11) | (1 << 9)),
            x87.status & !((7 << 11) | (1 << 9))
        );
        if let Some(l) = x87.ip {
            assert_eq!(got.cpu.x87.ip, locate(l) as u32);
        }
        if let Some(l) = x87.opcode {
            assert_eq!(got.cpu.x87.opcode, locate(l) as u16);
        }
        if let Some(l) = x87.c1 {
            assert_eq!(u128::from(got.cpu.x87.status >> 9 & 1), locate(l));
        }
        for (i, l) in x87.stack.iter().enumerate() {
            if let Some(l) = l {
                let bits = locate(*l) as u64;
                let reg = got.cpu.x87.regs[i];
                // Independent exact-double representation check, including zeros,
                // infinities and denormals.
                let exponent = bits >> 52 & 0x7ff;
                let fraction = bits & ((1 << 52) - 1);
                assert_eq!(reg.sign_exponent() >> 15, (bits >> 63) as u16);
                if exponent == 0 && fraction == 0 {
                    assert_eq!(reg.significand(), 0);
                    assert_eq!(reg.sign_exponent() & 0x7fff, 0);
                } else if exponent == 0x7ff {
                    assert_eq!(reg.sign_exponent() & 0x7fff, 0x7fff);
                    assert_eq!(reg.significand(), 1 << 63 | fraction << 11);
                } else {
                    let exponent = i32::from(reg.sign_exponent() & 0x7fff) - 16383;
                    let mantissa = reg.significand() as f64 / 2f64.powi(63);
                    let actual = if exponent < -1022 {
                        (mantissa * 2f64.powi(exponent + 1022)) * f64::MIN_POSITIVE
                    } else {
                        mantissa * 2f64.powi(exponent)
                    };
                    assert_eq!(actual, f64::from_bits(bits).abs());
                }
            }
        }
        for (i, l) in r.xmm.iter().enumerate() {
            if let Some(l) = l {
                assert_eq!(got.cpu.xmm[i], locate(*l).to_le_bytes());
            }
        }
        assert_eq!(
            got.frame,
            r.frame
                .iter()
                .map(|&(o, w, l)| (o, locate(l).to_le_bytes()[..w as usize].to_vec()))
                .collect::<Vec<_>>()
        );
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_integer_operations_and_conversions_match_reference() {
        let mut h = harness();
        for ty in [Type::I32, Type::I64] {
            for op in [
                IntOp::Add,
                IntOp::Sub,
                IntOp::And,
                IntOp::Or,
                IntOp::Xor,
                IntOp::Shl,
                IntOp::Shr,
                IntOp::Sar,
                IntOp::Mul,
                IntOp::MulHighSigned,
                IntOp::MulHighUnsigned,
            ] {
                let (mut f, b, p, _) = start(&[ty, ty]);
                let v = f.push(b, Op::Int(op, p[0], p[1])).unwrap();
                finish(&mut f, b, v);
                for (a, b) in [
                    (0, 0),
                    (0xffff_ffff, 37),
                    (0x8000_0000, 31),
                    (0x8000_0000_0000_0000, 63),
                    (u64::MAX, u64::MAX),
                ] {
                    let mask = if ty == Type::I32 {
                        u32::MAX as u128
                    } else {
                        u64::MAX as u128
                    };
                    compare(&f, &[u128::from(a) & mask, u128::from(b) & mask], &mut h);
                }
            }
            for op in [
                IntUnary::Not,
                IntUnary::Neg,
                IntUnary::ZeroExtend8,
                IntUnary::ZeroExtend16,
                IntUnary::SignExtend8,
                IntUnary::SignExtend16,
                IntUnary::Low32,
                IntUnary::High32,
                IntUnary::Widen,
                IntUnary::SignWiden,
            ] {
                let (mut f, b, p, _) = start(&[ty]);
                let v = f.push(b, Op::IntUnary(op, p[0])).unwrap();
                finish(&mut f, b, v);
                compare(
                    &f,
                    &[if ty == Type::I32 {
                        0xdead_ba98
                    } else {
                        0x1234_5678_dead_ba98
                    }],
                    &mut h,
                );
            }
        }
        // A remainder reads its dividend from a register, or else from a
        // constant rebuilt beside the quotient.
        for signed in [false, true] {
            for remainder in [None, Some(false), Some(true)] {
                for proved in [false, true] {
                    let (mut f, b, p, _) = start(&[Type::I64, Type::I32]);
                    let quotient = f
                        .push(
                            b,
                            Op::Div {
                                signed,
                                dividend: p[0],
                                divisor: p[1],
                                proved,
                            },
                        )
                        .unwrap();
                    let v = match remainder {
                        None => quotient,
                        Some(constant) => {
                            let dividend = if constant {
                                f.push(b, Op::Constant(Type::I64, 17)).unwrap()
                            } else {
                                p[0]
                            };
                            f.push(
                                b,
                                Op::Remainder {
                                    quotient,
                                    dividend,
                                    divisor: p[1],
                                },
                            )
                            .unwrap()
                        }
                    };
                    finish(&mut f, b, v);
                    for (n, d) in [(17u64, 3u32), ((-17i64) as u64, 3), (u64::MAX, 0xffff_ffff)] {
                        if proved && (!signed && n / u64::from(d) > u64::from(u32::MAX)) {
                            continue;
                        }
                        compare(&f, &[u128::from(n), u128::from(d)], &mut h);
                    }
                }
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_floating_vector_bits_and_select_match_reference() {
        let mut h = harness();
        for op in [FpOp::Add, FpOp::Sub, FpOp::Mul, FpOp::Div] {
            let (mut f, b, p, _) = start(&[Type::F64, Type::F64]);
            let v = f.push(b, Op::Fp(op, p[0], p[1])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for (a, b) in [
                (1.5f64, -2.0f64),
                (0.0, -0.0),
                (f64::INFINITY, 1.0),
                (f64::from_bits(0x7ff8_0000_0000_0123), 1.0),
            ] {
                compare(
                    &f,
                    &[u128::from(a.to_bits()), u128::from(b.to_bits())],
                    &mut h,
                );
            }
        }
        for op in [FpUnary::Neg, FpUnary::Abs] {
            let (mut f, b, p, _) = start(&[Type::F64]);
            let v = f.push(b, Op::FpUnary(op, p[0])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            compare(&f, &[u128::from((-2.7f64).to_bits())], &mut h);
        }
        for ty in [Type::I32, Type::I64] {
            let (mut f, b, p, _) = start(&[ty]);
            let v = f.push(b, Op::FromInt(p[0])).unwrap();
            finish(&mut f, b, v);
            for n in [
                0,
                17,
                0xffff_ffff,
                1 << 53,
                (1 << 53) + 1,
                i64::MAX as u128,
                i64::MIN as u64 as u128,
            ] {
                compare(
                    &f,
                    &[if ty == Type::I32 {
                        n & u128::from(u32::MAX)
                    } else {
                        n
                    }],
                    &mut h,
                );
            }
        }
        for wide in [false, true] {
            let (mut f, b, p, _) = start(&[Type::F64]);
            let v = f
                .push(
                    b,
                    Op::ToInt {
                        value: p[0],
                        wide,
                        sse: false,
                    },
                )
                .unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for x in [12.7, -8.9, f64::INFINITY, f64::NAN, 1e100] {
                compare(&f, &[u128::from(x.to_bits())], &mut h);
            }
        }
        for op in [
            VecOp::And(true),
            VecOp::AndNot(true),
            VecOp::Or(true),
            VecOp::Xor(true),
            VecOp::Same(crate::encode::Same::Sub, 2, true),
            VecOp::Shift(crate::encode::ShiftImm::Shl, 3, true),
            VecOp::Shift(crate::encode::ShiftImm::Ushr, 3, true),
            VecOp::LessF64,
        ] {
            let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
            let v = f.push(b, Op::Vec(op, p[0], p[1])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for count in [0, 1, 31, 63, 64, 256, u64::MAX] {
                compare(
                    &f,
                    &[0x3ff0_0000_0000_0000_ffff_0000_1234_5678, u128::from(count)],
                    &mut h,
                );
            }
        }
        for from in [Type::I32, Type::I64, Type::F64, Type::V128] {
            for to in [Type::I32, Type::I64, Type::F64, Type::V128] {
                let (mut f, b, p, _) = start(&[from]);
                let v = f.push(b, Op::Bits { to, value: p[0] }).unwrap();
                finish(&mut f, b, v);
                let bits = match from {
                    Type::I32 => 123,
                    Type::V128 => 0x4020_0000_0000_0000_4030_0000_0000_0000,
                    _ => 0x4030_0000_0000_0000,
                };
                compare(&f, &[bits], &mut h);
            }
            let (mut f, b, p, _) = start(&[Type::I32, from, from]);
            let v = f
                .push(
                    b,
                    Op::Select {
                        cond: p[0],
                        a: p[1],
                        b: p[2],
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for cond in [0, 1] {
                compare(&f, &[cond, 123, 456], &mut h);
            }
        }
    }

    // spec: A64-OPT-6
    #[test]
    fn opt_back_packed_integer_vector_ops_match_reference() {
        use crate::encode::{Narrowing, Permute, Same, ShiftImm};
        let mut h = harness();
        let pairs = [
            (
                0x80ff_7fff_0001_fe10_7f80_0100_ffff_0081u128,
                0x0001_7fff_8000_01ff_7f80_ff01_0002_807fu128,
            ),
            (
                0xffff_8000_0000_1234_abcd_ef01_8000_7fffu128,
                0xffff_8000_7fff_fedc_0000_8000_0001_0002u128,
            ),
        ];
        for full in [false, true] {
            let (mut f, b, p, _) = start(&[Type::V128]);
            let v = f.push(b, Op::VecUnary(VecUnary::Low64, p[0])).unwrap();
            finish(&mut f, b, v);
            for &(a, _) in &pairs {
                compare(&f, &[a], &mut h);
            }
            for op in [
                VecOp::And(full),
                VecOp::AndNot(full),
                VecOp::Or(full),
                VecOp::Xor(full),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f.push(b, Op::Vec(op, p[0], p[1])).unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                }
            }
            for (op, log2) in [
                (Same::Add, 0),
                (Same::Add, 1),
                (Same::Add, 2),
                (Same::Add, 3),
                (Same::Sub, 0),
                (Same::Sub, 1),
                (Same::Sub, 2),
                (Same::Sub, 3),
                (Same::Sqadd, 0),
                (Same::Sqadd, 1),
                (Same::Uqadd, 0),
                (Same::Uqadd, 1),
                (Same::Sqsub, 0),
                (Same::Sqsub, 1),
                (Same::Uqsub, 0),
                (Same::Uqsub, 1),
                (Same::Cmeq, 0),
                (Same::Cmeq, 1),
                (Same::Cmeq, 2),
                (Same::Cmgt, 0),
                (Same::Cmgt, 1),
                (Same::Cmgt, 2),
                (Same::Mul, 1),
                (Same::Smax, 1),
                (Same::Smin, 1),
                (Same::Umax, 0),
                (Same::Umin, 0),
                (Same::Urhadd, 0),
                (Same::Urhadd, 1),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f
                    .push(b, Op::Vec(VecOp::Same(op, log2, full), p[0], p[1]))
                    .unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                }
            }
            for (op, log2) in [
                (Permute::Zip1, 0),
                (Permute::Zip1, 1),
                (Permute::Zip1, 2),
                (Permute::Zip2, 0),
                (Permute::Zip2, 1),
                (Permute::Zip2, 2),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f
                    .push(b, Op::Vec(VecOp::Unpack(op, log2, full), p[0], p[1]))
                    .unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                }
            }
            for (op, log2) in [
                (Narrowing::Sqxtn, 0),
                (Narrowing::Sqxtn, 1),
                (Narrowing::Sqxtun, 0),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f
                    .push(b, Op::Vec(VecOp::Pack(op, log2, full), p[0], p[1]))
                    .unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                }
            }
            for op in [
                VecOp::MulHigh(true, full),
                VecOp::MulHigh(false, full),
                VecOp::MulAdd(full),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f.push(b, Op::Vec(op, p[0], p[1])).unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                }
            }
            for (op, log2) in [
                (ShiftImm::Shl, 1),
                (ShiftImm::Shl, 2),
                (ShiftImm::Shl, 3),
                (ShiftImm::Ushr, 1),
                (ShiftImm::Ushr, 2),
                (ShiftImm::Ushr, 3),
                (ShiftImm::Sshr, 1),
                (ShiftImm::Sshr, 2),
            ] {
                for count in [0u8, 1, 7, 15, 16, 31, 32, 63, 64] {
                    let (mut f, b, p, _) = start(&[Type::V128]);
                    let v = f
                        .push(
                            b,
                            Op::VecUnary(VecUnary::ShiftImm(op, log2, count, full), p[0]),
                        )
                        .unwrap();
                    finish(&mut f, b, v);
                    compare(&f, &[pairs[0].0], &mut h);
                }
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f
                    .push(b, Op::Vec(VecOp::Shift(op, log2, full), p[0], p[1]))
                    .unwrap();
                finish(&mut f, b, v);
                for count in [0u64, 1, 7, 15, 16, 31, 32, 63, 64, u64::MAX] {
                    compare(&f, &[pairs[0].0, u128::from(count)], &mut h);
                }
            }
            for op in [
                VecUnary::High64,
                VecUnary::Shuffle(if full { 2 } else { 1 }, 0x1b, full),
                VecUnary::Shuffle(if full { 2 } else { 1 }, 0xaa, full),
                VecUnary::ShuffleWords(false, 0xd8),
                VecUnary::ShuffleWords(true, 0x39),
                VecUnary::ShiftBytes(true, 0),
                VecUnary::ShiftBytes(true, 5),
                VecUnary::ShiftBytes(true, 16),
                VecUnary::ShiftBytes(false, 3),
                VecUnary::ShiftBytes(false, 20),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128]);
                let v = f.push(b, Op::VecUnary(op, p[0])).unwrap();
                finish(&mut f, b, v);
                for &(a, _) in &pairs {
                    compare(&f, &[a], &mut h);
                }
            }
            for op in [
                VecOp::Shufps(0x93),
                VecOp::Shufpd(2),
                VecOp::MulDwords(full),
                VecOp::SadBytes(full),
                VecOp::MoveLow32,
                VecOp::Insert64(false, false),
                VecOp::Insert64(true, false),
                VecOp::Insert64(false, true),
                VecOp::InsertWord(if full { 6 } else { 2 }, full),
            ] {
                let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
                let v = f.push(b, Op::Vec(op, p[0], p[1])).unwrap();
                finish(&mut f, b, v);
                for &(a, x) in &pairs {
                    compare(&f, &[a, x], &mut h);
                    compare(&f, &[a, a], &mut h);
                }
            }
        }
        for op in [
            VecToInt::ExtractWord(0),
            VecToInt::ExtractWord(3),
            VecToInt::ExtractWord(7),
            VecToInt::ByteMask(false),
            VecToInt::ByteMask(true),
            VecToInt::SignMask(false),
            VecToInt::SignMask(true),
        ] {
            let (mut f, b, p, _) = start(&[Type::V128]);
            let v = f.push(b, Op::VecToInt(op, p[0])).unwrap();
            finish(&mut f, b, v);
            for &(a, x) in &pairs {
                compare(&f, &[a], &mut h);
                compare(&f, &[x], &mut h);
            }
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_flags_and_conditions_match_reference() {
        use iced_x86::ConditionCode as CC;
        let mut h = harness();
        for width in [1, 2, 4] {
            for kind in [
                FlagKind::Add,
                FlagKind::AddCarry,
                FlagKind::Sub,
                FlagKind::SubBorrow,
                FlagKind::Logic,
                FlagKind::Increment,
                FlagKind::Decrement,
                FlagKind::Negate,
                FlagKind::Shl,
                FlagKind::Shr,
                FlagKind::Sar,
                FlagKind::MulSigned,
            ] {
                let (mut f, b, p, _) = start(&[Type::I32, Type::I32, Type::Flags]);
                let v = f
                    .push(
                        b,
                        Op::Flags {
                            kind,
                            width,
                            a: p[0],
                            b: p[1],
                            prior: Some(p[2]),
                        },
                    )
                    .unwrap();
                finish(&mut f, b, v);
                for (x, y) in [
                    (0, 0),
                    (0x7f, 1),
                    (0xff, 1),
                    (0x7fff_ffff, 1),
                    (0x8000_0000, 1),
                    (0x1234_abcd, 7),
                    (0xffff_ffff, 8),
                    (0xa5a5_4321, 16),
                    (0x8000_0001, 31),
                    (0xffff_ffff, 32),
                    (0x1234_5678, 33),
                ] {
                    for prior in [0x202, 0xed7] {
                        compare(&f, &[x, y, prior], &mut h);
                    }
                }
            }
        }
        for cc in [
            CC::o,
            CC::no,
            CC::b,
            CC::ae,
            CC::e,
            CC::ne,
            CC::be,
            CC::a,
            CC::s,
            CC::ns,
            CC::p,
            CC::np,
            CC::l,
            CC::ge,
            CC::le,
            CC::g,
        ] {
            let (mut f, b, p, _) = start(&[Type::Flags]);
            let v = f
                .push(
                    b,
                    Op::Test {
                        flags: p[0],
                        cond: cc,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for flags in 0..64 {
                let bits = [0, 2, 4, 6, 7, 11]
                    .iter()
                    .enumerate()
                    .fold(0x202, |v, (i, b)| v | (flags >> i & 1) << b);
                compare(&f, &[bits], &mut h);
            }
        }
        // A quiet NaN fails validation only in the ordered compare.
        for kind in [FlagKind::UnorderedCompare, FlagKind::OrderedCompare] {
            let (mut f, b, p, _) = start(&[Type::F64, Type::F64, Type::Flags]);
            let v = f
                .push(
                    b,
                    Op::Flags {
                        kind,
                        width: 8,
                        a: p[0],
                        b: p[1],
                        prior: Some(p[2]),
                    },
                )
                .unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for (x, y) in [
                (1.0f64, 2.0f64),
                (2.0, 1.0),
                (0.0, -0.0),
                (f64::NAN, 1.0),
                (f64::from_bits(0x7ff0_0000_0000_0001), 0.0),
            ] {
                compare(
                    &f,
                    &[u128::from(x.to_bits()), u128::from(y.to_bits()), 0xed7],
                    &mut h,
                );
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_failures_recover_the_region_under_canonical_fpcr() {
        let mut h = harness();
        let (mut f, b, p, entry) = start(&[Type::F64, Type::F64]);
        f.records[entry.0 as usize].x87.as_mut().unwrap().valid = 3;
        f.records[entry.0 as usize].x87.as_mut().unwrap().stack[0] = Some(Location::At(p[0]));
        f.records[entry.0 as usize].x87.as_mut().unwrap().stack[1] = Some(Location::At(p[1]));
        let v = f.push(b, Op::Fp(FpOp::Mul, p[0], p[1])).unwrap();
        f.push(b, Op::Validate);
        finish(&mut f, b, v);
        for (x, y) in [
            (1e300f64, 1e300f64),
            (1e-200, 1e-200),
            (f64::from_bits(1), 1.0),
            (f64::MIN_POSITIVE, 0.5),
        ] {
            assert_eq!(
                compare(
                    &f,
                    &[u128::from(x.to_bits()), u128::from(y.to_bits())],
                    &mut h
                )
                .record,
                entry
            );
        }
        for denormal in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I64]);
            f.records[entry.0 as usize]
                .frame
                .push((4, 8, Location::At(p[0])));
            let v = f
                .push(
                    b,
                    Op::X87Load {
                        bits: p[0],
                        denormal,
                        single: false,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for x in [
                f64::NAN,
                f64::from_bits(0x7ff0_0000_0000_0001),
                f64::from_bits(1),
                -0.0,
                f64::INFINITY,
                3.5,
            ] {
                compare(&f, &[u128::from(x.to_bits())], &mut h);
            }
        }
        {
            let (mut f, b, p, entry) = start(&[Type::I32]);
            f.records[entry.0 as usize].regs[0] = Some(Location::At(p[0]));
            let v = f
                .push(
                    b,
                    Op::X87Load {
                        bits: p[0],
                        denormal: false,
                        single: true,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for x in [
                f32::NAN,
                f32::from_bits(0x7f80_0001),
                f32::from_bits(1),
                -0.0,
                f32::INFINITY,
                f32::MIN_POSITIVE,
                -3.75,
            ] {
                compare(&f, &[u128::from(x.to_bits())], &mut h);
            }
        }
        for signed in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I64, Type::I32]);
            f.records[entry.0 as usize].regs[0] = Some(Location::At(p[1]));
            let v = f
                .push(
                    b,
                    Op::Div {
                        signed,
                        dividend: p[0],
                        divisor: p[1],
                        proved: false,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for (n, d) in [(123, 0), (u64::MAX, 1), (i64::MIN as u64, u32::MAX)] {
                compare(&f, &[u128::from(n), u128::from(d)], &mut h);
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-6, A64-OPT-7, A64-MEM-10
    #[test]
    fn opt_back_memory_and_fault_records_use_the_private_alias() {
        let mut h = harness();
        for (width, ty) in [
            (1, Type::I32),
            (1, Type::I64),
            (2, Type::I32),
            (2, Type::I64),
            (4, Type::I32),
            (4, Type::I64),
            (8, Type::I64),
            (16, Type::V128),
        ] {
            let (mut f, b, p, _) = start(&[Type::I32, ty]);
            let after = f.record(record(0x1010));
            f.push(
                b,
                Op::Store {
                    width,
                    addr: p[0],
                    value: p[1],
                    after,
                    checked: false,
                    pc: 0,
                },
            );
            let loaded = f
                .push(
                    b,
                    Op::Load {
                        ty,
                        width,
                        addr: p[0],
                        checked: false,
                        pc: 0,
                        clock: false,
                    },
                )
                .unwrap();
            finish(&mut f, b, loaded);
            let input = 0x1234_5678_9abc_def0_ffff_eeee_dddd_cccc;
            compare(&f, &[u128::from(DATA), input], &mut h);
            assert_eq!(
                h.emu.read(WINDOW_BASE + u64::from(DATA), width as usize),
                input.to_le_bytes()[..width as usize]
            );
        }
        for store in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I32, Type::I64]);
            f.records[entry.0 as usize].regs[7] = Some(Location::At(p[0]));
            f.records[entry.0 as usize]
                .frame
                .push((-8, 8, Location::At(p[1])));
            let value = if store {
                let after = f.record(record(0x1010));
                f.push(
                    b,
                    Op::Store {
                        width: 8,
                        addr: p[0],
                        value: p[1],
                        after,
                        checked: false,
                        pc: 0,
                    },
                );
                p[1]
            } else {
                f.push(
                    b,
                    Op::Load {
                        ty: Type::I64,
                        width: 8,
                        addr: p[0],
                        checked: false,
                        pc: 0,
                        clock: false,
                    },
                )
                .unwrap()
            };
            finish(&mut f, b, value);
            let got = compare(&f, &[0x80000, 0xdead_beef], &mut h);
            assert_eq!(got.record, entry);
            assert_eq!(got.lowered.sites[0].1.opt_record, Some(entry.0));
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_loop_parameters_countdown_and_head_exit() {
        let (mut f, head, p, entry) = start(&[Type::I32, Type::I32]);
        f.records[entry.0 as usize].regs[0] = Some(Location::At(p[0]));
        f.records[entry.0 as usize].regs[1] = Some(Location::At(p[1]));
        f.records[entry.0 as usize].refund = 3;
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, p[0], one)).unwrap();
        let total = f.push(head, Op::Int(IntOp::Add, p[1], p[0])).unwrap();
        let body = f.block(None);
        let done = f.block(None);
        f.end(
            head,
            End::Branch {
                cond: next,
                taken: Target {
                    block: body,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        f.end(
            body,
            End::Jump(Target {
                block: head,
                args: vec![next, total],
                charge: 3,
            }),
        );
        finish(&mut f, done, total);
        let mut h = harness();
        let full = compare(&f, &[5, 0], &mut h);
        assert_eq!(full.cpu.regs[0], 15);
        assert_eq!(full.countdown, 10000 - 15);
        for recall in [1, 2] {
            let cut = h.run_opt(&f, &[5, 0], 7, recall, 0);
            assert_eq!(cut.record, entry);
            assert_eq!(cut.cpu.regs[0..2], [3, 9]);
            assert_eq!(7u64.wrapping_sub(cut.countdown), 6);
        }
        f.head_exit = Some(head);
        let head_stop = h.run_opt(&f, &[5, 0], 100, 0, 2);
        assert_eq!(head_stop.record, entry);
        assert_eq!(head_stop.cpu.regs[0..2], [3, 9]);
        assert_eq!(head_stop.countdown, 94);
        f.head_exit = None;
        let refill = h.run_opt(&f, &[5, 0], 1, 0, 0);
        assert_eq!(refill.cpu.regs[0], 15);
        assert_ne!(refill.completed, 0);
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_spills_cold_values_and_c1_recipes() {
        let mut h = harness();
        let (mut f, b, p, entry) = start(&[Type::I64; 36]);
        for (i, &value) in p.iter().take(8).enumerate() {
            f.records[entry.0 as usize].regs[i] = Some(Location::At(value));
        }
        let mut result = p[0];
        for &v in &p[1..] {
            result = f.push(b, Op::Int(IntOp::Add, result, v)).unwrap();
        }
        finish(&mut f, b, result);
        let params: Vec<_> = (1..=36).collect();
        let got = compare(&f, &params, &mut h);
        assert_eq!(got.cpu.regs[0], 666);
        assert!(got.lowered.spill_cells > 0);
        assert!(got.spills.iter().any(|&v| v > 0));
        // Check a failure while all entry values must remain recoverable.
        let (mut f, b, p, entry) = start(&[Type::I64; 36]);
        f.records[entry.0 as usize].frame = p
            .iter()
            .enumerate()
            .map(|(i, &p)| (i as i32 * 8, 8, Location::At(p)))
            .collect();
        let zero = f.push(b, Op::Constant(Type::I32, 0)).unwrap();
        let v = f
            .push(
                b,
                Op::Div {
                    signed: false,
                    dividend: p[0],
                    divisor: zero,
                    proved: false,
                },
            )
            .unwrap();
        finish(&mut f, b, v);
        compare(&f, &params, &mut h);
        let (mut f, b, p, _) = start(&[Type::F64, Type::F64]);
        let v = f.push(b, Op::Fp(FpOp::Div, p[0], p[1])).unwrap();
        f.push(b, Op::Validate);
        let exit = finish(&mut f, b, v);
        f.records[exit.0 as usize].x87.as_mut().unwrap().c1 = Some(Location::Recipe(Recipe::C1 {
            op: FpOp::Div,
            a: p[0],
            b: p[1],
        }));
        f.records[exit.0 as usize].x87.as_mut().unwrap().top = 3;
        let got = compare(
            &f,
            &[u128::from(1f64.to_bits()), u128::from(10f64.to_bits())],
            &mut h,
        );
        assert_eq!(got.cpu.x87.status >> 9 & 1, 1);
        assert_eq!(got.registers.x[28], WINDOW_BASE);
        assert_eq!(got.registers.x[29], CONTEXT);
    }

    // spec: A64-OPT-7
    #[test]
    fn opt_back_loop_values_keep_registers_under_pressure() {
        let stubs = stubs::stubs();
        let mut h = harness();
        // Entry values live across the loop, more than there are registers.
        let (mut f, b, p, _) = start(&[Type::I64; 36]);
        let looped = f.record(record(0x1000));
        let head = f.block(Some(Head { record: looped }));
        let count = f.param(head, Type::I32);
        let total = f.param(head, Type::I64);
        let ten = f.push(b, Op::Constant(Type::I32, 10)).unwrap();
        f.end(
            b,
            End::Jump(Target {
                block: head,
                args: vec![ten, p[0]],
                charge: 0,
            }),
        );
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let wide = f.push(head, Op::Constant(Type::I64, 3)).unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, count, one)).unwrap();
        let sum = f.push(head, Op::Int(IntOp::Add, total, wide)).unwrap();
        let latch = f.block(None);
        let done = f.block(None);
        f.end(
            head,
            End::Branch {
                cond: next,
                taken: Target {
                    block: latch,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        f.end(
            latch,
            End::Jump(Target {
                block: head,
                args: vec![next, sum],
                charge: 1,
            }),
        );
        let mut result = sum;
        for &v in &p[1..] {
            result = f.push(done, Op::Int(IntOp::Add, result, v)).unwrap();
        }
        finish(&mut f, done, result);
        let params: Vec<_> = (1..=36).collect();
        let got = compare(&f, &params, &mut h);
        assert_eq!(got.cpu.regs[0], 666 + 30);
        let mut lowered = checked_lower(&f);
        lowered.place(0, stubs.opt_exit);
        let spill = |&w: &u32| w & 0x3b00_0000 == 0x3900_0000 && (w >> 5) & 31 == 31;
        for block in [head, latch] {
            assert!(
                !body(&lowered, block.0 as usize).iter().any(spill),
                "the loop touches no spill cell"
            );
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_edges_refund_and_copy_parameters_at_a_nonentry_head() {
        let (mut f, entry, p, _) = start(&[Type::I32, Type::I32, Type::I32]);
        let mut r = record(0x1100);
        r.refund = 3;
        let head_record = f.record(r);
        let head = f.block(Some(Head {
            record: head_record,
        }));
        let n = f.param(head, Type::I32);
        let x = f.param(head, Type::I32);
        let y = f.param(head, Type::I32);
        f.records[head_record.0 as usize].regs[0] = Some(Location::At(n));
        f.records[head_record.0 as usize].regs[1] = Some(Location::At(x));
        f.records[head_record.0 as usize].regs[2] = Some(Location::At(y));
        f.records[head_record.0 as usize].x87.as_mut().unwrap().ip = Some(Location::At(x));
        f.records[head_record.0 as usize]
            .x87
            .as_mut()
            .unwrap()
            .opcode = Some(Location::At(y));
        f.end(
            entry,
            End::Jump(Target {
                block: head,
                args: p.clone(),
                charge: 3,
            }),
        );
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, n, one)).unwrap();
        let flags = f
            .push(
                head,
                Op::Flags {
                    kind: FlagKind::Sub,
                    width: 4,
                    a: n,
                    b: one,
                    prior: None,
                },
            )
            .unwrap();
        let cond = f
            .push(
                head,
                Op::Test {
                    flags,
                    cond: iced_x86::ConditionCode::a,
                },
            )
            .unwrap();
        let done_record = f.record(record(0x1200));
        let done = f.block(Some(Head {
            record: done_record,
        }));
        f.end(
            head,
            End::Branch {
                cond,
                taken: Target {
                    block: head,
                    args: vec![next, y, x],
                    charge: 3,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: -2,
                },
            },
        );
        let result = f.push(done, Op::Int(IntOp::Add, x, y)).unwrap();
        finish(&mut f, done, result);
        let mut h = harness();
        compare(&f, &[3, 0x1234, 0x5678], &mut h);
        f.head_exit = Some(head);
        let cut = h.run_opt(&f, &[3, 0x1234, 0x5678], 100, 0, 2);
        assert_eq!(cut.record, head_record);
        assert_eq!(cut.cpu.regs[0..3], [2, 0x5678, 0x1234]);
        assert_eq!(cut.cpu.x87.ip, 0x5678);
        assert_eq!(cut.cpu.x87.opcode, 0x1234);
        assert_eq!(100 - cut.countdown, 3);
        let deferred = h.run_opt(&f, &[3, 0x1234, 0x5678], 1, 1, 0);
        assert_eq!(deferred.record, head_record);
        assert_eq!(deferred.cpu.regs[0..3], [2, 0x5678, 0x1234]);
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_failure_after_store_recovers_the_new_region() {
        let (mut f, b, p, _) = start(&[Type::I32, Type::I64, Type::F64, Type::F64]);
        let mut r = record(0x1010);
        r.regs[1] = Some(Location::At(p[1]));
        r.frame.push((8, 8, Location::At(p[1])));
        let after = f.record(r);
        f.push(
            b,
            Op::Store {
                width: 8,
                addr: p[0],
                value: p[1],
                after,
                checked: false,
                pc: 0,
            },
        );
        let v = f.push(b, Op::Fp(FpOp::Mul, p[2], p[3])).unwrap();
        f.push(b, Op::Validate);
        finish(&mut f, b, v);
        let mut h = harness();
        let got = compare(
            &f,
            &[
                u128::from(DATA),
                0x1234_abcd,
                u128::from(1e300f64.to_bits()),
                u128::from(1e300f64.to_bits()),
            ],
            &mut h,
        );
        assert_eq!(got.record, after);
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), 8),
            0x1234_abcdu64.to_le_bytes()
        );
    }
    // spec: A64-OPT-4, A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_lazy_flag_tests_and_records_cover_every_condition() {
        use iced_x86::ConditionCode as CC;
        let conditions = [
            CC::o,
            CC::no,
            CC::b,
            CC::ae,
            CC::e,
            CC::ne,
            CC::be,
            CC::a,
            CC::s,
            CC::ns,
            CC::p,
            CC::np,
            CC::l,
            CC::ge,
            CC::le,
            CC::g,
        ];
        let kinds = [
            FlagKind::Add,
            FlagKind::AddCarry,
            FlagKind::Sub,
            FlagKind::SubBorrow,
            FlagKind::Logic,
            FlagKind::Increment,
            FlagKind::Decrement,
            FlagKind::Negate,
            FlagKind::Shl,
            FlagKind::Shr,
            FlagKind::Sar,
            FlagKind::MulSigned,
            FlagKind::UnorderedCompare,
            FlagKind::OrderedCompare,
        ];
        let mut h = harness();
        for kind in kinds {
            for width in [1, 2, 4] {
                for cc in conditions {
                    let floating = kind.floating();
                    let ty = if floating { Type::F64 } else { Type::I32 };
                    let (mut f, b, p, _) = start(&[ty, ty, Type::Flags]);
                    let flags = f
                        .push(
                            b,
                            Op::Flags {
                                kind,
                                width,
                                a: p[0],
                                b: p[1],
                                prior: Some(p[2]),
                            },
                        )
                        .unwrap();
                    let test = f.push(b, Op::Test { flags, cond: cc }).unwrap();
                    if floating {
                        f.push(b, Op::Validate);
                    }
                    let exit = finish(&mut f, b, test);
                    f.records[exit.0 as usize].eflags =
                        Some(Location::Recipe(Recipe::Eflags(flags)));
                    let inputs: Vec<(u128, u128)> = if floating {
                        [
                            (1.0f64, 2.0f64),
                            (2.0, 1.0),
                            (0.0, -0.0),
                            (f64::NAN, 1.0),
                            (f64::from_bits(0x7ff0_0000_0000_0001), 0.0),
                            (f64::from_bits(1), 0.0),
                        ]
                        .map(|(a, b)| (u128::from(a.to_bits()), u128::from(b.to_bits())))
                        .to_vec()
                    } else {
                        vec![
                            (0, 0),
                            (0x7f, 0),
                            (2, 1),
                            (0xff, 0),
                            (0xff, 1),
                            (0x7fff, 1),
                            (0x7fff_ffff, 1),
                            (0x8000_0000, 1),
                            (0xffff_ffff, 1),
                            (0xa5a5_4321, 2),
                            (0x1234_abcd, 7),
                            (0xffff_ffff, 8),
                            (0x8001, 9),
                            (0xa5a5_4321, 16),
                            (0x8000_0001, 17),
                            (0x8000_0001, 31),
                            (0xffff_ffff, 32),
                            (0x1234_5678, 33),
                        ]
                    };
                    for (x, y) in inputs {
                        for prior in [0x202, 0xed7] {
                            let params = [x, y, prior];
                            let run = eval::run(&f, &params, &mut memory(&[], &[]), 10000);
                            let got = h.run_opt(&f, &params, 10000, 0, 0);
                            let expected = match run.outcome {
                                eval::Outcome::Exit(r) | eval::Outcome::Fail(r) => r,
                                _ => unreachable!(),
                            };
                            assert_eq!(
                                got.record, expected,
                                "{kind:?}/{width}/{cc:?}, {params:x?}"
                            );
                            if expected == exit {
                                assert_eq!(
                                    got.cpu.regs[0], run.values[test.0 as usize] as u32,
                                    "{kind:?}/{width}/{cc:?}, {params:x?}"
                                );
                            }
                            compare_record(&f, &run.values, &got);
                        }
                    }
                }
            }
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_lazy_flags_read_only_the_prior_bits_the_operation_keeps() {
        use iced_x86::ConditionCode as CC;
        let mut h = harness();
        for kind in [
            FlagKind::Increment,
            FlagKind::Decrement,
            FlagKind::AddCarry,
            FlagKind::SubBorrow,
            FlagKind::Shl,
            FlagKind::Shr,
            FlagKind::Sar,
            FlagKind::MulSigned,
        ] {
            for cc in [
                CC::o,
                CC::no,
                CC::b,
                CC::ae,
                CC::e,
                CC::ne,
                CC::be,
                CC::a,
                CC::s,
                CC::ns,
                CC::p,
                CC::np,
                CC::l,
                CC::ge,
                CC::le,
                CC::g,
            ] {
                let (mut f, b, p, _) = start(&[Type::I32, Type::I32, Type::Flags]);
                let prior = f
                    .push(
                        b,
                        Op::Flags {
                            kind: FlagKind::Sub,
                            width: 4,
                            a: p[1],
                            b: p[0],
                            prior: Some(p[2]),
                        },
                    )
                    .unwrap();
                let flags = f
                    .push(
                        b,
                        Op::Flags {
                            kind,
                            width: 1,
                            a: p[0],
                            b: p[1],
                            prior: Some(prior),
                        },
                    )
                    .unwrap();
                let test = f.push(b, Op::Test { flags, cond: cc }).unwrap();
                let exit = finish(&mut f, b, test);
                f.records[exit.0 as usize].eflags = Some(Location::Recipe(Recipe::Eflags(flags)));
                for params in [
                    [0xff, 0, 0x202],
                    [0x80, 1, 0xed7],
                    [0xff, 9, 0x202],
                    [0x7f, 31, 0xed7],
                ] {
                    compare(&f, &params, &mut h);
                }
            }
        }
    }

    fn body(lowered: &back::Lowered, block: usize) -> &[u32] {
        let start = lowered.marks[block + 1].offset as usize / 4;
        let end = lowered.marks[block + 2].offset as usize / 4;
        &lowered.words[start..end]
    }

    // spec: A64-OPT-4, A64-OPT-7
    #[test]
    fn opt_back_emitted_word_counts_keep_common_operations_lean() {
        let stubs = stubs::stubs();
        let (mut f, b, p, _) = start(&[Type::I32]);
        let k = f.push(b, Op::Constant(Type::I32, 7)).unwrap();
        let v = f.push(b, Op::Int(IntOp::Add, p[0], k)).unwrap();
        finish(&mut f, b, v);
        let mut lower = checked_lower(&f);
        lower.place(0, stubs.opt_exit);
        assert_eq!(body(&lower, 0).len(), 2, "one ADD plus exit branch");
        assert_eq!(
            body(&lower, 0)[0],
            add_imm(W, Reg::new(0), Reg::new(0), 7, false)
        );

        // A negative addend subtracts; a power-of-two factor shifts.
        let (mut f, b, p, _) = start(&[Type::I32]);
        let k = f.push(b, Op::Constant(Type::I32, 0xffff_fffc)).unwrap();
        let v = f.push(b, Op::Int(IntOp::Add, p[0], k)).unwrap();
        let k = f.push(b, Op::Constant(Type::I32, 8)).unwrap();
        let v = f.push(b, Op::Int(IntOp::Mul, v, k)).unwrap();
        finish(&mut f, b, v);
        let mut lower = checked_lower(&f);
        lower.place(0, stubs.opt_exit);
        assert_eq!(
            body(&lower, 0)[..2],
            [
                sub_imm(W, Reg::new(0), Reg::new(0), 4, false),
                lsl_imm(W, Reg::new(0), Reg::new(0), 3)
            ]
        );
        let mut h = harness();
        assert_eq!(compare(&f, &[7], &mut h).cpu.regs[0], 24);

        let (mut f, b, p, _) = start(&[Type::I32]);
        let k = f.push(b, Op::Constant(Type::I32, 5)).unwrap();
        let flags = f
            .push(
                b,
                Op::Flags {
                    kind: FlagKind::Sub,
                    width: 4,
                    a: p[0],
                    b: k,
                    prior: None,
                },
            )
            .unwrap();
        // The producer need not be adjacent to the Test.
        f.push(b, Op::Constant(Type::I32, 123));
        let test = f
            .push(
                b,
                Op::Test {
                    flags,
                    cond: iced_x86::ConditionCode::a,
                },
            )
            .unwrap();
        let fall = f.block(None);
        let taken = f.block(None);
        f.end(
            b,
            End::Branch {
                cond: test,
                taken: Target {
                    block: taken,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: fall,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        for block in [fall, taken] {
            let exit = f.record(record(0x2000));
            f.end(block, End::Exit(exit));
        }
        let mut lower = checked_lower(&f);
        lower.place(0, stubs.opt_exit);
        assert_eq!(body(&lower, 0).len(), 2, "CMP plus conditional branch");
        assert_eq!(body(&lower, 0)[0], cmp_imm(W, Reg::new(0), 5, false));
        assert_eq!(body(&lower, 0)[1] & 0xff00_001f, b_cond(Cond::Hi, 0));

        let (mut f, b, _, entry) = start(&[]);
        f.records[entry.0 as usize].refund = 3;
        f.push(b, Op::Validate);
        let exit = f.record(record(0x2000));
        f.end(b, End::Exit(exit));
        let mut lower = checked_lower(&f);
        lower.place(0, stubs.opt_exit);
        let start = lower.marks[1].offset as usize / 4;
        assert_eq!(lower.words[start - 2], sub_imm(X, X9, X9, 3, false));
        assert_eq!(lower.words[start - 1] & 0xfff8_001f, tbz(true, X9, 63, 0));
        assert_eq!(body(&lower, 0).len(), 1, "Validate emits no code");
    }

    // spec: A64-OPT-4, A64-OPT-7
    #[test]
    fn opt_back_counted_floating_loop_has_no_backedge_moves() {
        for split in [false, true] {
            let (mut f, entry, p, _) = start(&[Type::F64, Type::F64, Type::I32]);
            let head_record = f.record(record(0x1100));
            let head = f.block(Some(Head {
                record: head_record,
            }));
            let sum = f.param(head, Type::F64);
            let count = f.param(head, Type::I32);
            f.end(
                entry,
                End::Jump(Target {
                    block: head,
                    args: vec![p[0], p[2]],
                    charge: 0,
                }),
            );
            let one = f.push(entry, Op::Constant(Type::I32, 1)).unwrap();
            let total = f.push(head, Op::Fp(FpOp::Add, sum, p[1])).unwrap();
            f.push(head, Op::Validate);
            let next = f.push(head, Op::Int(IntOp::Sub, count, one)).unwrap();
            let done = f.block(None);
            // A charged arrival may reach its head through a head of its own
            // (A64-FLAG-3); that head is laid out inline on the edge.
            let taken = if split {
                let record = f.record(record(0x1100));
                let through = f.block(Some(Head { record }));
                f.end(
                    through,
                    End::Jump(Target {
                        block: head,
                        args: vec![total, next],
                        charge: 0,
                    }),
                );
                Target {
                    block: through,
                    args: Vec::new(),
                    charge: 3,
                }
            } else {
                Target {
                    block: head,
                    args: vec![total, next],
                    charge: 3,
                }
            };
            f.end(
                head,
                End::Branch {
                    cond: next,
                    taken,
                    fall: Target {
                        block: done,
                        args: Vec::new(),
                        charge: 0,
                    },
                },
            );
            finish(&mut f, done, total);
            let mut lower = checked_lower(&f);
            lower.place(0, stubs::stubs().opt_exit);
            let head_code = body(&lower, 1);
            assert_eq!(
                head_code[0],
                dfloat(Float::Fadd, VReg::new(31), VReg::new(0), VReg::new(1))
            );
            assert_eq!(
                head_code.len(),
                12,
                "FADD, result check, SUB, CBZ, then the inline backedge"
            );
            // The exit edge is bare, so the loop leaves with one CBZ and the
            // taken backedge holds only its two-word charge and branch; no
            // register or memory copies.
            assert_eq!(
                head_code[8] & 0xff00_0000,
                cbz(W, false, Reg::new(0), 0) & 0xff00_0000
            );
            assert_eq!(head_code[9], sub_imm(X, X9, X9, 3, false));
            assert_eq!(head_code[10] & 0xfff8_001f, tbz(true, X9, 63, 0));
            assert_eq!(head_code[11] & 0xfc00_0000, crate::encode::b(0));
            let mut h = harness();
            compare(
                &f,
                &[u128::from(1f64.to_bits()), u128::from(0.5f64.to_bits()), 4],
                &mut h,
            );
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_integer_immediates_and_allocated_constants_match_reference() {
        let mut h = harness();
        for ty in [Type::I32, Type::I64] {
            for op in [
                IntOp::Add,
                IntOp::Sub,
                IntOp::And,
                IntOp::Or,
                IntOp::Xor,
                IntOp::Shl,
                IntOp::Shr,
                IntOp::Sar,
                IntOp::Mul,
            ] {
                for bits in [0, 7, 0x1000, 0xffff0000, 0x12345678, 0x123456789abcdef0] {
                    let bits = if ty == Type::I32 {
                        bits & 0xffffffff
                    } else {
                        bits
                    };
                    let (mut f, b, p, _) = start(&[ty]);
                    let k = f.push(b, Op::Constant(ty, bits)).unwrap();
                    let v = f.push(b, Op::Int(op, p[0], k)).unwrap();
                    finish(&mut f, b, v);
                    compare(&f, &[0x98765432], &mut h);
                }
            }
        }
    }

    // spec: A64-CTL-7, A64-OPT-6
    #[test]
    fn opt_back_widens_checkpoint_bit_tests_across_large_forms() {
        let (mut f, b, p, entry) = start(&[Type::I32]);
        f.records[entry.0 as usize].refund = 3;
        // The body puts the entry checkpoint past 32 KiB.
        let one = f.push(b, Op::Constant(Type::I32, 1)).unwrap();
        let mut value = p[0];
        for _ in 0..9000 {
            value = f.push(b, Op::Int(IntOp::Add, value, one)).unwrap();
        }
        let exit = finish(&mut f, b, value);
        let mut h = harness();
        let ran = h.run_opt(&f, &[0x1234], 10, 0, 0);
        assert!(ran.lowered.words.len() > 8192);
        assert_eq!(ran.record, exit);
        assert_eq!(10 - ran.countdown, 3);
        let cut = h.run_opt(&f, &[0x1234], 2, 1, 0);
        assert_eq!(cut.record, entry);
        assert_eq!(2 - cut.countdown, 0);
    }
}
