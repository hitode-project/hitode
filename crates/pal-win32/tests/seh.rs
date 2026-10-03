//! RtlUnwind through real x86 SEH registrations on the guest stack.

use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::layout::PARK_ADDR;
use pal_machine::{Clock, Machine, Registry, RunResult};
use pal_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const FS_BEFORE: u32 = DATA;
const OUTER_FRAME: u32 = DATA + 4;
const INNER_FRAME: u32 = DATA + 8;
const SEQ: u32 = DATA + 0xc;
const OUT: u32 = DATA + 0x10; // eax, ebx, esi, edi, ebp, fs:[0], esp
const LOG: u32 = DATA + 0x40; // per handler call: id, frame, code, flags

fn imm(a: &mut Asm, op: &[u8], v: u32) {
    a.bytes.extend_from_slice(op);
    a.bytes.extend_from_slice(&v.to_le_bytes());
}

/// A cdecl handler that logs (id, frame, code, flags), clobbers EBX and continues the search.
fn handler(a: &mut Asm, id: u32) -> u32 {
    let at = a.here();
    imm(a, &[0x8b, 0x0d], SEQ); // mov ecx, [SEQ]
    a.bytes.extend_from_slice(&[0xc1, 0xe1, 0x04]); // shl ecx, 4
    imm(a, &[0xc7, 0x81], LOG); // mov dword [ecx+LOG], id
    a.bytes.extend_from_slice(&id.to_le_bytes());
    a.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 0x08]); // mov eax, [esp+8]
    imm(a, &[0x89, 0x81], LOG + 4); // mov [ecx+LOG+4], eax
    a.bytes.extend_from_slice(&[0x8b, 0x54, 0x24, 0x04]); // mov edx, [esp+4]
    a.bytes.extend_from_slice(&[0x8b, 0x02]); // mov eax, [edx]
    imm(a, &[0x89, 0x81], LOG + 8);
    a.bytes.extend_from_slice(&[0x8b, 0x42, 0x04]); // mov eax, [edx+4]
    imm(a, &[0x89, 0x81], LOG + 12);
    imm(a, &[0xff, 0x05], SEQ); // inc dword [SEQ]
    imm(a, &[0xbb], 0xdead); // mov ebx, 0xdead
    imm(a, &[0xb8], 1); // mov eax, ExceptionContinueSearch
    a.ret();
    at
}

fn register_frame(a: &mut Asm, handler: u32, save: u32) {
    a.push_imm(handler);
    a.bytes.extend_from_slice(&[0x64, 0xff, 0x35, 0, 0, 0, 0]); // push fs:[0]
    a.bytes.extend_from_slice(&[0x64, 0x89, 0x25, 0, 0, 0, 0]); // mov fs:[0], esp
    imm(a, &[0x89, 0x25], save); // mov [save], esp
}

struct Run {
    m: Machine<UnicornCpu>,
}

impl Run {
    fn rd(&self, p: u32) -> u32 {
        self.m.cpu.read_u32(p).unwrap()
    }
}

/// Two registrations, then RtlUnwind to `target` (a frame address slot, or 0) with the
/// label after the call as target IP.
fn unwind(target: Option<u32>) -> Run {
    let mut r = Registry::default();
    pal_win32::register(&mut r);
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), r, Clock::virtual_time()).unwrap();
    let tmp = std::env::temp_dir().join("pal-win32-test-seh");
    Win32::install(&mut m, Win32Config::new(&tmp, "test.exe", &tmp)).unwrap();
    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    let rtl_unwind = m.resolve_import("kernel32.dll", "RtlUnwind").unwrap();

    let mut a = Asm::new(CODE + 0x200);
    let inner = handler(&mut a, 1);
    let outer = handler(&mut a, 2);
    let handlers = a.bytes.clone();

    let mut a = Asm::new(CODE);
    a.mov_eax_fs(0).mov_mem_eax(FS_BEFORE);
    imm(&mut a, &[0xbb], 0x1111_1111); // mov ebx
    imm(&mut a, &[0xbe], 0x2222_2222); // mov esi
    imm(&mut a, &[0xbf], 0x3333_3333); // mov edi
    a.bytes.push(0x55); // push ebp
    imm(&mut a, &[0xbd], 0x4444_4444); // mov ebp
    register_frame(&mut a, outer, OUTER_FRAME);
    register_frame(&mut a, inner, INNER_FRAME);
    a.push_imm(0x1234).push_imm(0);
    let label_slot = a.bytes.len() + 1;
    a.push_imm(0);
    match target {
        Some(slot) => imm(&mut a, &[0xff, 0x35], slot), // push [slot]
        None => {
            a.push_imm(0);
        }
    }
    a.call(rtl_unwind);
    let label = a.here();
    a.bytes[label_slot..label_slot + 4].copy_from_slice(&label.to_le_bytes());
    a.mov_mem_eax(OUT);
    imm(&mut a, &[0x89, 0x1d], OUT + 4); // mov [], ebx
    imm(&mut a, &[0x89, 0x35], OUT + 8); // esi
    imm(&mut a, &[0x89, 0x3d], OUT + 12); // edi
    imm(&mut a, &[0x89, 0x2d], OUT + 16); // ebp
    a.mov_eax_fs(0).mov_mem_eax(OUT + 20);
    imm(&mut a, &[0x89, 0x25], OUT + 24); // esp
    a.add_esp(16);
    a.bytes.push(0x5d); // pop ebp
    a.ret();
    assert!(a.bytes.len() < 0x200);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.cpu.write(CODE + 0x200, &handlers).unwrap();
    m.spawn("seh", CODE, &[], 0x10000).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(_) | RunResult::Idle { next_deadline: None }), "{result:?}");
    Run { m }
}

#[test]
fn rtl_unwind_calls_frames_above_the_target_and_resumes_after_the_call() {
    let r = unwind(Some(OUTER_FRAME));
    let (outer, inner) = (r.rd(OUTER_FRAME), r.rd(INNER_FRAME));
    assert_eq!(r.rd(SEQ), 1, "only the inner handler runs");
    assert_eq!([r.rd(LOG), r.rd(LOG + 4), r.rd(LOG + 8), r.rd(LOG + 12)], [1, inner, 0xc000_0027, 2]);
    let out: Vec<u32> = (0..7).map(|i| r.rd(OUT + 4 * i)).collect();
    // EAX = return value, callee-saved registers of the entry, chain at the target, and the
    // stack as the caller left it after the stdcall.
    assert_eq!(out, [0x1234, 0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444, outer, inner]);
}

#[test]
fn rtl_unwind_without_target_is_an_exit_unwind_of_the_whole_chain() {
    let r = unwind(None);
    let (outer, inner) = (r.rd(OUTER_FRAME), r.rd(INNER_FRAME));
    assert_eq!(r.rd(SEQ), 2);
    assert_eq!([r.rd(LOG), r.rd(LOG + 4), r.rd(LOG + 12)], [1, inner, 6]);
    assert_eq!([r.rd(LOG + 16), r.rd(LOG + 20), r.rd(LOG + 28)], [2, outer, 6]);
    assert_eq!(r.rd(OUT), 0x1234);
    assert_eq!(r.rd(OUT + 4), 0x1111_1111);
    assert_eq!(r.rd(OUT + 20), r.rd(FS_BEFORE), "chain unwound to its end");
}
