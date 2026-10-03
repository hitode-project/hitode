#![allow(dead_code)]
#[path = "../../../adv-cpu/tests/support/mod.rs"]
mod oracle_support;
use adv_cpu::{CpuState, EXEC, READ, Region, WRITE};
use adv_runtime::{Graphics, IID_DDRAW7, Snapshot, api, install};
use iced_x86::code_asm::*;
use oracle_support::Machine;
use std::collections::BTreeMap;

pub const CODE: u32 = 0x10000;
pub const API_BASE: u32 = 0x12000;
pub const STOP: u32 = 0x18000;
pub const DATA: u32 = 0x20000;
pub const STACK: u32 = 0xf0000;
pub const INPUT_A: u32 = 0x100000;
pub const INPUT_B: u32 = 0x1000000;
pub const HEAP: u32 = 0x2000000;
pub const SURFACE_OUT: u32 = DATA + 0x208;
pub const PRIMARY_OUT: u32 = DATA + 0x20c;
pub const LOCK_DESC: u32 = DATA + 0x300;
pub const BITMAP_OUT: u32 = DATA + 0x210;
pub const DC_OUT: u32 = DATA + 0x214;
pub const OLD_BITMAP: u32 = DATA + 0x218;

pub struct Fixture {
    pub machine: Machine,
    pub graphics: Graphics<Snapshot>,
    pub expected: Vec<u8>,
    pub entries: BTreeMap<u32, u32>,
    pub frame_pc: u32,
}

fn blend(a: &mut CodeAssembler, w: u32, h: u32) {
    let mut top = a.create_label();
    a.mov(esi, INPUT_A).unwrap();
    a.mov(edx, INPUT_B).unwrap();
    a.mov(ecx, w * h / 4).unwrap();
    a.set_label(&mut top).unwrap();
    a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
    a.movdqu(xmm1, xmmword_ptr(edx)).unwrap();
    a.pavgb(xmm0, xmm1).unwrap();
    a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
    a.add(esi, 16).unwrap();
    a.add(edx, 16).unwrap();
    a.add(edi, 16).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
}
fn com(a: &mut CodeAssembler, slot: i32) {
    a.mov(edx, dword_ptr(ebx)).unwrap();
    a.call(dword_ptr(edx + slot * 4)).unwrap();
}

pub fn fixture(ddraw: bool, w: u32, h: u32, bottom_up: bool) -> Fixture {
    assert!((w * h).is_multiple_of(4));
    let mut m = Machine::new(CODE, 0x4000000).unwrap();
    m.protect(CODE, 0x4000000, READ | WRITE).unwrap();
    let entries = install(&mut m, API_BASE).unwrap();
    let mut input_a = Vec::new();
    let mut input_b = Vec::new();
    let mut expected = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = [(x * 255 / w) as u8, (y * 255 / h) as u8, 40, 255];
            let b = [
                30,
                if (x / 16 + y / 16) % 2 == 0 { 220 } else { 70 },
                (255 - x * 200 / w) as u8,
                255,
            ];
            input_a.extend(a);
            input_b.extend(b);
            expected.extend(
                a.into_iter()
                    .zip(b)
                    .map(|(a, b)| ((u16::from(a) + u16::from(b) + 1) >> 1) as u8),
            );
        }
    }
    m.write(INPUT_A, &input_a).unwrap();
    m.write(INPUT_B, &input_b).unwrap();
    if bottom_up {
        let original = expected.clone();
        for y in 0..h as usize {
            let n = w as usize * 4;
            expected[y * n..(y + 1) * n]
                .copy_from_slice(&original[(h as usize - 1 - y) * n..(h as usize - y) * n]);
        }
    }
    let mut a = CodeAssembler::new(32).unwrap();
    let mut frame = a.create_label();
    if !ddraw {
        let mut bmi = [0u8; 40];
        bmi[..4].copy_from_slice(&40u32.to_le_bytes());
        bmi[4..8].copy_from_slice(&w.to_le_bytes());
        bmi[8..12].copy_from_slice(&(if bottom_up { h as i32 } else { -(h as i32) }).to_le_bytes());
        bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
        bmi[14..16].copy_from_slice(&32u16.to_le_bytes());
        m.write(DATA, &bmi).unwrap();
        a.push(0).unwrap();
        a.call(u64::from(entries[&api::GET_DC])).unwrap();
        a.mov(dword_ptr(DATA as i32 + 0x220), eax).unwrap();
        a.push(0).unwrap();
        a.call(u64::from(entries[&api::CREATE_DC])).unwrap();
        a.mov(ebx, eax).unwrap();
        a.mov(dword_ptr(DC_OUT as i32), eax).unwrap();
        for v in [0, 0, DATA + 0x200, 0, DATA, 0] {
            a.push(v).unwrap();
        }
        a.call(u64::from(entries[&api::CREATE_DIB])).unwrap();
        a.mov(dword_ptr(BITMAP_OUT as i32), eax).unwrap();
        a.push(eax).unwrap();
        a.push(ebx).unwrap();
        a.call(u64::from(entries[&api::SELECT])).unwrap();
        a.mov(dword_ptr(OLD_BITMAP as i32), eax).unwrap();
        a.set_label(&mut frame).unwrap();
        a.mov(edi, dword_ptr(DATA as i32 + 0x200)).unwrap();
        blend(&mut a, w, h);
        a.push(0x00cc0020).unwrap();
        a.push(0).unwrap();
        a.push(0).unwrap();
        a.push(dword_ptr(DC_OUT as i32)).unwrap();
        for v in [h, w, 0, 0] {
            a.push(v).unwrap();
        }
        a.push(dword_ptr(DATA as i32 + 0x220)).unwrap();
        a.call(u64::from(entries[&api::BITBLT])).unwrap();
        a.ret().unwrap();
    } else {
        m.write(DATA, &IID_DDRAW7).unwrap();
        let mut desc = [0u8; 124];
        for (p, v) in [(0, 124), (4, 7), (8, h), (12, w), (104, 0x40u32)] {
            desc[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        m.write(DATA + 0x40, &desc).unwrap();
        desc.fill(0);
        for (p, v) in [(0, 124u32), (4, 1), (104, 0x200)] {
            desc[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        m.write(DATA + 0xc0, &desc).unwrap();
        m.write(LOCK_DESC, &124u32.to_le_bytes()).unwrap();
        for v in [0, DATA, DATA + 0x200, 0] {
            a.push(v).unwrap();
        }
        a.call(u64::from(entries[&api::DD_CREATE])).unwrap();
        a.mov(ebx, dword_ptr(DATA as i32 + 0x200)).unwrap();
        for (desc, out) in [(DATA + 0x40, SURFACE_OUT), (DATA + 0xc0, PRIMARY_OUT)] {
            for v in [0, out, desc] {
                a.push(v).unwrap();
            }
            a.push(ebx).unwrap();
            com(&mut a, 6);
        }
        a.set_label(&mut frame).unwrap();
        a.mov(ebx, dword_ptr(SURFACE_OUT as i32)).unwrap();
        for v in [0, 0, LOCK_DESC, 0] {
            a.push(v).unwrap();
        }
        a.push(ebx).unwrap();
        com(&mut a, 25);
        a.mov(edi, dword_ptr(LOCK_DESC as i32 + 36)).unwrap();
        blend(&mut a, w, h);
        a.push(0).unwrap();
        a.push(ebx).unwrap();
        com(&mut a, 32);
        a.mov(ebx, dword_ptr(PRIMARY_OUT as i32)).unwrap();
        for v in [0, 0x01000000, 0] {
            a.push(v).unwrap();
        }
        a.push(dword_ptr(SURFACE_OUT as i32)).unwrap();
        a.push(0).unwrap();
        a.push(ebx).unwrap();
        com(&mut a, 5);
        a.ret().unwrap();
    }
    let code = a
        .assemble_options(
            u64::from(CODE),
            iced_x86::BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let frame_pc = code.label_ip(&frame).unwrap() as u32;
    m.write(CODE, &code.inner.code_buffer).unwrap();
    m.protect(CODE, 0x3000, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    let region_end = entries.values().copied().max().unwrap() + 16;
    let mut region = Region::new(CODE, (region_end - CODE) as usize);
    region.entries = entries.values().copied().chain([frame_pc]).collect();
    m.set_region(region).unwrap();
    m.state = CpuState {
        eip: CODE,
        ..CpuState::default()
    };
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let graphics =
        Graphics::new(w, h, HEAP, 0x1800000, entries.clone(), Snapshot::default()).unwrap();
    Fixture {
        machine: m,
        graphics,
        expected,
        entries,
        frame_pc,
    }
}

pub fn invoke(f: &mut Fixture, id: u32, args: &[u32]) -> u32 {
    // Enter the real stdcall thunk with an ordinary guest call frame.
    let stack_pointer = STACK - args.len() as u32 * 4;
    f.machine.write(stack_pointer, &STOP.to_le_bytes()).unwrap();
    for (i, &v) in args.iter().enumerate() {
        f.machine
            .write(stack_pointer + 4 + i as u32 * 4, &v.to_le_bytes())
            .unwrap();
    }
    f.machine.state.eip = f.entries[&id];
    f.machine.state.regs[4] = stack_pointer;
    assert_eq!(
        f.machine
            .run_with_services(STOP, 10000, &mut f.graphics)
            .unwrap(),
        adv_cpu::RunOutcome::ReachedStop
    );
    assert_eq!(f.machine.state.regs[4], STACK + 4);
    f.machine.state.regs[0]
}

pub fn u32_at(m: &Machine, p: u32) -> u32 {
    u32::from_le_bytes(m.memory().read(p, 4).unwrap().try_into().unwrap())
}
