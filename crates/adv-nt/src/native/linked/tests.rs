//! The linked helpers' bodies against their own code (NAT-BODY-1).

use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};

/// One caller per helper, 0x100 bytes apart, then the helpers back to back.
const CODE: u32 = 0x0040_0000;
const HELPERS: u32 = 0x0040_1000;
/// EAX and EDX, the stacked arguments, then what the call left in EAX,
/// EDX, ECX, EBX, ESI, EDI, EBP and ESP, then ESP before the call.
const DATA: u32 = 0x0041_0000;
const OUT: u32 = DATA + 64;
const SAVED: u32 = OUT + 32;
/// Pixels.
const BUF: u32 = 0x0042_0000;
const BUF_LEN: usize = 0x4000;

fn helpers() -> Vec<&'static super::Helper> {
    super::helpers().collect()
}

fn index(name: &str) -> usize {
    helpers().iter().position(|h| h.0 == name).unwrap()
}

fn divisions() -> impl Iterator<Item = usize> {
    ["__lldiv", "__lludiv", "__llmod", "__llumod"]
        .map(|name| index(&format!("borland!{name}")))
        .into_iter()
}

fn machine(native: bool) -> Machine<Cpu> {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, true).unwrap(),
        crate::dispatch::Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 0x2000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    m.cpu.map(BUF, BUF_LEN as u32, Prot::RW).unwrap();
    let mut at = HELPERS;
    for (index, &&(_, conv, bytes, _)) in helpers().iter().enumerate() {
        let (Conv::Cdecl { argc } | Conv::Stdcall { argc }) = conv else {
            unreachable!()
        };
        let mut code = vec![0x53, 0x55, 0x56, 0x57, 0x89, 0x25];
        code.extend_from_slice(&SAVED.to_le_bytes());
        for arg in (0..u32::from(argc)).rev() {
            code.extend_from_slice(&[0xff, 0x35]);
            code.extend_from_slice(&(DATA + 8 + 4 * arg).to_le_bytes());
        }
        code.push(0xa1);
        code.extend_from_slice(&DATA.to_le_bytes());
        code.extend_from_slice(&[0x8b, 0x15]);
        code.extend_from_slice(&(DATA + 4).to_le_bytes());
        for op in [0xb9, 0xbb, 0xbe, 0xbf, 0xbd] {
            code.push(op);
            code.extend_from_slice(&(u32::from(op) * 0x0101_0101).to_le_bytes());
        }
        let caller = CODE + 0x100 * index as u32;
        code.push(0xe8);
        let next = caller + code.len() as u32 + 4;
        code.extend_from_slice(&at.wrapping_sub(next).to_le_bytes());
        code.push(0xa3);
        code.extend_from_slice(&OUT.to_le_bytes());
        for (modrm, offset) in [0x15, 0x0d, 0x1d, 0x35, 0x3d, 0x2d, 0x25]
            .into_iter()
            .zip(1..)
        {
            code.extend_from_slice(&[0x89, modrm]);
            code.extend_from_slice(&(OUT + 4 * offset).to_le_bytes());
        }
        code.extend_from_slice(&[0x8b, 0x25]);
        code.extend_from_slice(&SAVED.to_le_bytes());
        code.extend_from_slice(&[0x5f, 0x5e, 0x5d, 0x5b, 0x31, 0xc0, 0xc3]);
        assert!(code.len() <= 0x100);
        m.cpu.write(caller, &code).unwrap();
        m.cpu.write(at, bytes).unwrap();
        at += bytes.len() as u32;
    }
    if native {
        super::bind(&mut m.cpu, &mut m.sys, &mut m.ext, HELPERS, at - HELPERS);
    }
    m
}

/// Calls a helper with EDX:EAX and stacked `args`: the registers it left,
/// with ESP relative to its value before the call.
fn call(m: &mut Machine<Cpu>, helper: usize, edx_eax: u64, args: &[u32]) -> Result<Vec<u32>, u32> {
    m.cpu.write(DATA, &edx_eax.to_le_bytes()).unwrap();
    for (offset, arg) in (0..).zip(args) {
        m.cpu
            .write(DATA + 8 + 4 * offset, &arg.to_le_bytes())
            .unwrap();
    }
    m.spawn("helper", CODE + 0x100 * helper as u32, &[], 0x10000)
        .unwrap();
    match m.run() {
        RunResult::Exited(0) => {}
        RunResult::Failed(error) => return Err(error.pc),
        other => panic!("{other:?}"),
    }
    let mut words: Vec<_> = (0..9)
        .map(|i| m.cpu.read_u32(OUT + 4 * i).unwrap())
        .collect();
    // Machines place stacks apart; the call must keep ESP where it was.
    words[7] = words[7].wrapping_sub(words.pop().unwrap());
    Ok(words)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

// spec: NAT-BODY-1, NAT-BODY-13
#[test]
fn borland_division_matches_its_code() {
    let (mut native, mut reference) = (machine(true), machine(false));
    let mut values = vec![
        0,
        1,
        2,
        7,
        0xffff_ffff,
        1 << 32,
        0x1_2345_6789,
        i64::MAX as u64,
        i64::MIN as u64,
    ];
    values.extend(values.clone().iter().map(|v| v.wrapping_neg()));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..16 {
        let seed = rng.next();
        values.push(seed >> (seed % 48));
    }
    for helper in divisions() {
        let (before, declines) = (native.sys.host_calls, super::super::declines(&native.ext));
        for &a in &values {
            for &b in values.iter().filter(|&&b| b != 0) {
                let args = [b as u32, (b >> 32) as u32];
                let expected = call(&mut reference, helper, a, &args);
                // The translated caller warms before it can redirect.
                for _ in 0..2 {
                    let actual = call(&mut native, helper, a, &args);
                    assert_eq!(actual, expected, "helper {helper}: {a:#x} by {b:#x}");
                }
            }
        }
        assert!(native.sys.host_calls > before, "helper {helper} never ran");
        assert_eq!(super::super::declines(&native.ext), declines);
    }
}

// spec: NAT-BODY-13
#[test]
fn borland_division_by_zero_faults_in_its_code() {
    for helper in divisions() {
        // A fault ends the machine's run, so each call gets fresh machines.
        let expected = call(&mut machine(false), helper, 1 << 32, &[0, 0]);
        assert!(expected.is_err());
        let mut native = machine(true);
        let mut warm = || call(&mut native, helper, 7, &[2, 0]).unwrap();
        warm();
        warm();
        assert_eq!(call(&mut native, helper, 1 << 32, &[0, 0]), expected);
        assert_eq!(super::super::declines(&native.ext), 1, "helper {helper}");
    }
}

/// Random pixels whose alpha is clear, opaque or between, in runs.
fn pixels(rng: &mut Rng) -> Vec<u8> {
    let mut bytes = vec![0; BUF_LEN];
    let mut run = 0;
    for pixel in bytes.as_chunks_mut().0 {
        *pixel = (rng.next() as u32).to_le_bytes();
        if rng.below(4) == 0 {
            run = rng.below(3);
        }
        pixel[3] = match run {
            0 => 0,
            1 => 0xff,
            _ => pixel[3],
        };
    }
    bytes
}

/// Runs a helper on both machines from the same buffer, twice on the
/// native one: the registers it keeps and the buffer must match, and EAX
/// too when it holds a result.
fn check_buffer(
    machines: &mut [Machine<Cpu>; 2],
    helper: usize,
    buffer: &[u8],
    args: &[u32],
    result: bool,
) {
    let run = |m: &mut Machine<Cpu>| {
        m.cpu.write(BUF, buffer).unwrap();
        let mut words = call(m, helper, 0, args).unwrap();
        if !result {
            words.remove(0);
        }
        let mut bytes = vec![0; BUF_LEN];
        m.cpu.read(BUF, &mut bytes).unwrap();
        (words, bytes)
    };
    let [native, reference] = machines;
    let expected = run(reference);
    for _ in 0..2 {
        let actual = run(native);
        assert_eq!(actual.0, expected.0, "{args:x?}");
        if let Some(at) = (0..BUF_LEN).find(|&i| actual.1[i] != expected.1[i]) {
            panic!(
                "{args:x?}: byte {at:#x} is {:#x}, not {:#x}",
                actual.1[at], expected.1[at]
            );
        }
    }
}

// spec: NAT-BODY-14
#[test]
fn tvpgl_alpha_blend_matches_its_code() {
    let mut machines = [machine(true), machine(false)];
    let helper = index("kirikiri2!TVPAlphaBlend");
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for case in 0..240u32 {
        let len = match case % 6 {
            0 => case / 6 % 8,
            1 => 300,
            2 => (-(case as i32 % 3)) as u32,
            _ => rng.below(64) as u32,
        };
        let dest = BUF + 4 * rng.below(16) as u32 + u32::from(case % 7 == 0);
        let src = match case % 5 {
            0 => dest,
            _ => BUF + 0x2000 + 4 * rng.below(16) as u32,
        };
        let pixels = pixels(&mut rng);
        check_buffer(&mut machines, helper, &pixels, &[dest, src, len], false);
    }
    let [native, _] = &machines;
    assert!(native.sys.host_calls > 0);
    assert_eq!(super::super::declines(&native.ext), 0);
}

// spec: NAT-BODY-14
#[test]
fn tvpgl_interp_stretch_copy_matches_its_code() {
    let mut machines = [machine(true), machine(false)];
    let helper = index("kirikiri2!TVPInterpStretchCopy");
    let (src1, src2, width) = (BUF + 0x2000, BUF + 0x3000, 0x100u32);
    let mut rng = Rng(0x0bad_5eed_1234_5678);
    for case in 0..240u32 {
        let len = match case % 8 {
            0 => (-(case as i32 % 3)) as u32,
            _ => 1 + rng.below(64) as u32,
        };
        let step = match case % 4 {
            0 => 0x10000,
            1 => rng.below(0x8000) as u32,
            2 => 0x8000 + rng.below(0x18000) as u32,
            _ => (rng.below(0x18000) as u32).wrapping_neg(),
        };
        let range = len.saturating_sub(1) as i64 * i64::from(step as i32);
        let room = (width - 2) << 16;
        if range.unsigned_abs() >= u64::from(room) {
            continue;
        }
        let start =
            (-range).max(0) as u32 + rng.below(u64::from(room) - range.unsigned_abs()) as u32;
        let blend_y = match case % 5 {
            0 => 0,
            1 => 0xff,
            2 => 0x80,
            3 => rng.next() as u32,
            _ => rng.below(0x100) as u32,
        };
        let dest = BUF + 4 * rng.below(64) as u32;
        let pixels = pixels(&mut rng);
        let args = [dest, len, src1, src2, blend_y, start, step];
        check_buffer(&mut machines, helper, &pixels, &args, false);
    }
    let [native, _] = &machines;
    assert!(native.sys.host_calls > 0);
    assert_eq!(super::super::declines(&native.ext), 0);
}

// spec: NAT-BODY-13
#[test]
fn borland_copies_match_their_code() {
    let mut machines = [machine(true), machine(false)];
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    let mut overlapping_memcpy = 0;
    for name in ["borland!memcpy", "borland!memmove"] {
        let helper = index(name);
        for case in 0..200u32 {
            let n = match case % 4 {
                0 => case / 4 % 9,
                _ => rng.below(200) as u32,
            };
            let src = BUF + rng.below(0x400) as u32;
            let dest = match case % 5 {
                0 => src,
                1 => src.wrapping_add(rng.below(16) as u32).wrapping_sub(8),
                2 => src + rng.below(u64::from(n) + 1) as u32,
                _ => BUF + 0x800 + rng.below(0x400) as u32,
            };
            overlapping_memcpy +=
                u64::from(name == "borland!memcpy" && dest > src && dest - src < n);
            let bytes: Vec<u8> = (0..BUF_LEN).map(|_| rng.next() as u8).collect();
            check_buffer(&mut machines, helper, &bytes, &[dest, src, n], true);
        }
    }
    let [native, _] = &machines;
    assert!(native.sys.host_calls > 0);
    // Each overlapping memcpy declines on both native calls.
    assert_eq!(super::super::declines(&native.ext), 2 * overlapping_memcpy);
}

// spec: NAT-BODY-14
#[test]
fn tvpgl_ps_alpha_blend_matches_its_code() {
    let mut machines = [machine(true), machine(false)];
    let helper = index("kirikiri2!TVPPsAlphaBlend");
    let mut rng = Rng(0x1405_7b7e_f767_814f);
    for case in 0..240u32 {
        let len = match case % 6 {
            0 => case / 6 % 8,
            1 => 300,
            2 => (-(case as i32 % 3)) as u32,
            _ => rng.below(64) as u32,
        };
        let src = BUF + 0x2000 + 4 * rng.below(16) as u32 + u32::from(case % 7 == 0);
        let dest = match case % 5 {
            0 => src,
            _ => BUF + 4 * rng.below(16) as u32,
        };
        let pixels = pixels(&mut rng);
        check_buffer(&mut machines, helper, &pixels, &[dest, src, len], false);
    }
    let [native, _] = &machines;
    assert!(native.sys.host_calls > 0);
    assert_eq!(super::super::declines(&native.ext), 0);
}

// spec: NAT-BODY-14
#[test]
fn tvpgl_fill_argb_matches_its_code() {
    let mut machines = [machine(true), machine(false)];
    let helper = index("kirikiri2!TVPFillARGB");
    let mut rng = Rng(0x3c6e_f372_fe94_f82b);
    for case in 0..120u32 {
        let len = match case % 4 {
            0 => (-(case as i32 % 3)) as u32,
            1 => 300 + rng.below(64) as u32,
            _ => rng.below(64) as u32,
        };
        let dest = BUF + 4 * rng.below(64) as u32 + u32::from(case % 7 == 0);
        let pixels = pixels(&mut rng);
        let value = rng.next() as u32;
        check_buffer(
            &mut machines,
            helper,
            &pixels,
            &[dest, len, value],
            len as i32 > 0,
        );
    }
    let [native, _] = &machines;
    assert!(native.sys.host_calls > 0);
    assert_eq!(super::super::declines(&native.ext), 0);
}
