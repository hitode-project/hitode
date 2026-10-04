#![allow(dead_code)]
// Authored graphics fixtures use the production machine/ABI path for every backend.
#[path = "../../tests/support/cpu.rs"]
pub mod cpu;
use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_cpu::{Region, RunOutcome};
use adv_machine::{Clock, Conv, Env, Machine, Object, Outcome, RunResult, Wait};
use adv_runtime::{Graphics, IID_DDRAW7, Snapshot, api, install_graphics, register_graphics};
use iced_x86::code_asm::*;
use std::collections::BTreeMap;
use std::time::Duration;

pub const CODE: u32 = 0x10000;
pub const LAUNCH: u32 = 0x17000;
pub const DATA: u32 = 0x20000;
pub const INPUT_A: u32 = 0x100000;
pub const INPUT_B: u32 = 0x1000000;
pub const SURFACE_OUT: u32 = DATA + 0x208;
pub const PRIMARY_OUT: u32 = DATA + 0x20c;
pub const LOCK_DESC: u32 = DATA + 0x300;
pub const BITMAP_OUT: u32 = DATA + 0x210;
pub const DC_OUT: u32 = DATA + 0x214;
pub const OLD_BITMAP: u32 = DATA + 0x218;
const REQUEST: u32 = DATA + 0x800;
const ARGC: u32 = REQUEST + 4;
const RESULT: u32 = REQUEST + 8;
const ARGS: u32 = REQUEST + 16;

struct Gate(u32);
fn completed(env: &mut Env<'_>) -> Outcome {
    Outcome::block_then_return(
        Wait::Object {
            handle: env.ext.get::<Gate>().0,
            deadline: None,
        },
        0,
    )
}

pub struct Fixture {
    pub machine: Machine<cpu::Cpu>,
    pub expected: Vec<u8>,
    pub entries: BTreeMap<u32, u32>,
    pub frame_pc: u32,
    started: bool,
}
impl Fixture {
    pub fn graphics(&self) -> &Graphics<Snapshot> {
        self.machine.ext.get()
    }
    pub fn graphics_mut(&mut self) -> &mut Graphics<Snapshot> {
        self.machine.ext.get_mut()
    }
    pub fn read(&self, address: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        self.machine.cpu.read(address, &mut out).unwrap();
        out
    }
    pub fn run(&mut self) -> Result<RunOutcome, String> {
        self.run_entry(CODE, &[])
    }
    pub fn run_frame(&mut self) -> Result<RunOutcome, String> {
        self.run_entry(self.frame_pc, &[])
    }
    fn run_entry(&mut self, entry: u32, args: &[u32]) -> Result<RunOutcome, String> {
        assert!(args.len() <= 16);
        self.machine.cpu.write_u32(REQUEST, entry).unwrap();
        self.machine.cpu.write_u32(ARGC, args.len() as u32).unwrap();
        for (i, value) in args.iter().enumerate() {
            self.machine
                .cpu
                .write_u32(ARGS + i as u32 * 4, *value)
                .unwrap();
        }
        if self.started {
            self.machine.sys.set_event(self.machine.ext.get::<Gate>().0);
        } else {
            self.started = true;
        }
        match self.machine.run() {
            RunResult::Idle {
                next_deadline: None,
            } => Ok(RunOutcome::ReachedStop),
            other => Err(format!("graphics fixture failed: {other:?}")),
        }
    }
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
    fixture_backend(ddraw, w, h, bottom_up, "jit")
}
pub fn fixture_backend(ddraw: bool, w: u32, h: u32, bottom_up: bool, backend: &str) -> Fixture {
    assert!((w * h).is_multiple_of(4));
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    register_graphics(&mut registry);
    registry.add(
        "fixture.dll",
        "Completed",
        Conv::Cdecl { argc: 0 },
        completed,
    );
    let mut m = Machine::new(
        cpu::Cpu::new(backend).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.set_execution_budget(Some(Duration::from_secs(5)));
    m.cpu.map(CODE, 0x4000000, Prot::RW).unwrap();
    let mut config =
        adv_win32::Win32Config::new(".", "pipeline-fixture.exe", "target/pipeline-fixture");
    config.screen = (w as i32, h as i32);
    adv_win32::Win32::install(&mut m, config).unwrap();
    let entries = install_graphics(&mut m, w, h).unwrap();
    let done = m.resolve_import("fixture.dll", "Completed").unwrap();
    let gate = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    m.ext.insert(Gate(gate));
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
    m.cpu.write(INPUT_A, &input_a).unwrap();
    m.cpu.write(INPUT_B, &input_b).unwrap();
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
        m.cpu.write(DATA, &bmi).unwrap();
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
        m.cpu.write(DATA, &IID_DDRAW7).unwrap();
        let mut desc = [0u8; 124];
        for (p, v) in [(0, 124), (4, 7), (8, h), (12, w), (104, 0x40u32)] {
            desc[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        m.cpu.write(DATA + 0x40, &desc).unwrap();
        desc.fill(0);
        for (p, v) in [(0, 124u32), (4, 1), (104, 0x200)] {
            desc[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        m.cpu.write(DATA + 0xc0, &desc).unwrap();
        m.cpu.write(LOCK_DESC, &124u32.to_le_bytes()).unwrap();
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
    m.cpu.write(CODE, &code.inner.code_buffer).unwrap();
    m.cpu.protect(CODE, 4096, Prot::RX).unwrap();
    // A reusable real guest context calls the selected function, records EAX,
    // then blocks on a kernel event. Every resume uses the machine continuation.
    let mut launcher = CodeAssembler::new(32).unwrap();
    let mut again = launcher.create_label();
    let mut push_arg = launcher.create_label();
    let mut call_target = launcher.create_label();
    launcher.mov(ebp, esp).unwrap();
    launcher.set_label(&mut again).unwrap();
    launcher.mov(ecx, dword_ptr(ARGC)).unwrap();
    launcher.mov(esi, ARGS).unwrap();
    launcher.test(ecx, ecx).unwrap();
    launcher.jz(call_target).unwrap();
    launcher.set_label(&mut push_arg).unwrap();
    launcher.dec(ecx).unwrap();
    launcher.push(dword_ptr(esi + ecx * 4)).unwrap();
    launcher.test(ecx, ecx).unwrap();
    launcher.jnz(push_arg).unwrap();
    launcher.set_label(&mut call_target).unwrap();
    launcher.call(dword_ptr(REQUEST)).unwrap();
    launcher.mov(dword_ptr(RESULT), eax).unwrap();
    launcher.mov(esp, ebp).unwrap();
    launcher.call(u64::from(done)).unwrap();
    launcher.jmp(again).unwrap();
    let launch = launcher.assemble(u64::from(LAUNCH)).unwrap();
    m.cpu.write(LAUNCH, &launch).unwrap();
    m.cpu.protect(LAUNCH, 4096, Prot::RX).unwrap();
    if let Some(jit) = m.cpu.jit_mut() {
        jit.set_async_compilation(false);
        let mut region = Region::new(CODE, code.inner.code_buffer.len());
        region.entries.push(frame_pc);
        jit.executor_mut().add_region(region).unwrap();
        jit.executor_mut()
            .add_region(Region::new(LAUNCH, launch.len()))
            .unwrap();
    }
    m.spawn("graphics fixture", LAUNCH, &[], 65536).unwrap();
    Fixture {
        machine: m,
        expected,
        entries,
        frame_pc,
        started: false,
    }
}

pub fn invoke(f: &mut Fixture, id: u32, args: &[u32]) -> u32 {
    assert_eq!(
        f.run_entry(f.entries[&id], args).unwrap(),
        RunOutcome::ReachedStop
    );
    f.machine.cpu.read_u32(RESULT).unwrap()
}
pub fn u32_at(m: &Machine<cpu::Cpu>, p: u32) -> u32 {
    m.cpu.read_u32(p).unwrap()
}
