//! lstr* and small arithmetic helpers.

use adv_abi::Registry;
use adv_cpu::backend::Guest;
use adv_machine::{Env, Outcome};
use std::borrow::Cow;

use super::K32;
use crate::stdcall;
use crate::util::{cstr, put_cstr, put_wstr, wstr};

fn lstrcpyn_a(env: &mut Env<'_>) -> Outcome {
    let (dst, src, n) = (env.arg(0), env.arg(1), env.arg(2));
    let s = cstr(env.cpu, src);
    put_cstr(env.cpu, dst, n, &s);
    Outcome::Return(if n == 0 { 0 } else { dst })
}

fn lstrcat_a(env: &mut Env<'_>) -> Outcome {
    let (dst, src) = (env.arg(0), env.arg(1));
    let d = cstr(env.cpu, dst);
    let mut s = cstr(env.cpu, src);
    s.push(0);
    let _ = env.cpu.write(dst + d.len() as u32, &s);
    Outcome::Return(dst)
}

fn cmp_ci(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}

fn name_bytes(cpu: &dyn Guest, pointer: u32) -> Cow<'_, [u8]> {
    if pointer == 0 {
        return Cow::Borrowed(&[]);
    }
    // Short resource names normally end on this page. Borrow only exclusively
    // owned storage; shared/lazy/protected pages and crossing strings retain
    // the existing copied read, including its fault and length-cap behavior.
    let page_remaining = 4096 - (pointer as usize & 4095);
    if let Some(bytes) = cpu.shared_bytes(pointer, page_remaining)
        && let Some(end) = bytes.iter().position(|&byte| byte == 0)
    {
        return Cow::Borrowed(&bytes[..end]);
    }
    Cow::Owned(cstr(cpu, pointer))
}

fn sign(o: std::cmp::Ordering) -> u32 {
    o as i32 as u32
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "lstrlenA", 1, |e| {
        Outcome::Return(cstr(e.cpu, e.arg(0)).len() as u32)
    });
    stdcall(reg, K32, "lstrlenW", 1, |e| {
        Outcome::Return(wstr(e.cpu, e.arg(0)).len() as u32)
    });
    stdcall(reg, K32, "lstrcpyA", 2, |e| {
        let (d, s) = (e.arg(0), e.arg(1));
        let v = cstr(e.cpu, s);
        put_cstr(e.cpu, d, u32::MAX, &v);
        Outcome::Return(d)
    });
    reg.register_alias(K32, "lstrcpy", "lstrcpyA")
        .expect("SDK-declared legacy ANSI alias");
    stdcall(reg, K32, "lstrcpyW", 2, |e| {
        let (d, s) = (e.arg(0), e.arg(1));
        let v = wstr(e.cpu, s);
        put_wstr(e.cpu, d, u32::MAX, &v);
        Outcome::Return(d)
    });
    stdcall(reg, K32, "lstrcpynA", 3, lstrcpyn_a);
    stdcall(reg, K32, "lstrcatA", 2, lstrcat_a);
    stdcall(reg, K32, "lstrcmpA", 2, |e| {
        Outcome::Return(sign(cstr(e.cpu, e.arg(0)).cmp(&cstr(e.cpu, e.arg(1)))))
    });
    stdcall(reg, K32, "lstrcmpiA", 2, |e| {
        Outcome::Return(sign(cmp_ci(
            &name_bytes(e.cpu, e.arg(0)),
            &name_bytes(e.cpu, e.arg(1)),
        )))
    });
    stdcall(reg, K32, "lstrcmpW", 2, |e| {
        Outcome::Return(sign(wstr(e.cpu, e.arg(0)).cmp(&wstr(e.cpu, e.arg(1)))))
    });
    stdcall(reg, K32, "MulDiv", 3, |e| {
        let (a, b, c) = (
            e.arg(0) as i32 as i64,
            e.arg(1) as i32 as i64,
            e.arg(2) as i32 as i64,
        );
        Outcome::Return(if c == 0 {
            u32::MAX
        } else {
            ((a * b + c / 2) / c) as i32 as u32
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Prot};
    use adv_machine::layout::PARK_ADDR;

    #[test]
    fn production_guest_view_borrows_only_terminated_names_on_one_page() {
        use adv_cpu::backend::asm::Asm;
        use adv_machine::{Clock, Conv, Machine, RunResult};
        let mut registry = Registry::default();
        registry.add("fixture", "names", Conv::Stdcall { argc: 0 }, |e| {
            assert!(matches!(name_bytes(e.cpu, 0x410000), Cow::Borrowed(_)));
            assert_eq!(name_bytes(e.cpu, 0x410000).as_ref(), b"A\x83a.ogg");
            assert!(matches!(name_bytes(e.cpu, 0x410ffc), Cow::Owned(_)));
            assert_eq!(name_bytes(e.cpu, 0x410ffc).as_ref(), b"CrossPage");
            assert!(name_bytes(e.cpu, 0x411ffc).is_empty());
            Outcome::Return(0)
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        machine.cpu.map(0x410000, 8192, Prot::RW).unwrap();
        machine.cpu.write(0x410000, b"A\x83a.ogg\0").unwrap();
        machine.cpu.write(0x410ffc, b"CrossPage\0").unwrap();
        machine.cpu.write(0x411ffc, b"NoNL").unwrap();
        let target = machine.resolve_import("fixture", "names").unwrap();
        let mut code = Asm::new(0x400000);
        code.call(target).ret();
        machine.cpu.write(0x400000, &code.bytes).unwrap();
        machine
            .spawn("name memory boundaries", 0x400000, &[], 65536)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    #[test]
    fn names_at_page_edges_keep_termination_crossing_and_fault_behavior() {
        let mut cpu = JitCpu::new(PARK_ADDR, false).unwrap();
        cpu.map(0x400000, 8192, Prot::RW).unwrap();
        cpu.write(0x400ffb, b"AbCd\0").unwrap();
        assert_eq!(name_bytes(&cpu, 0x400ffb).as_ref(), b"AbCd");
        cpu.write(0x400ffc, b"AbCdEf\0").unwrap();
        assert_eq!(name_bytes(&cpu, 0x400ffc).as_ref(), b"AbCdEf");
        cpu.write(0x401ffc, b"AbCd").unwrap();
        assert!(name_bytes(&cpu, 0x401ffc).is_empty());
        assert!(name_bytes(&cpu, 0).is_empty());
        assert!(name_bytes(&cpu, 0x500000).is_empty());
    }

    #[test]
    fn borrowed_names_preserve_dbcs_bytes_and_comparison_signs() {
        let mut cpu = JitCpu::new(PARK_ADDR, false).unwrap();
        cpu.map(0x400000, 4096, Prot::RW).unwrap();
        for bytes in [b"AbCd".as_slice(), b"\x83A\x83a.ogg", b"\xd6\xd0A\xce\xc4"] {
            cpu.write(0x400000, &[bytes, &[0]].concat()).unwrap();
            assert_eq!(name_bytes(&cpu, 0x400000).as_ref(), bytes);
        }
        assert_eq!(sign(cmp_ci(b"A.ogg", b"a.OGG")), 0);
        assert_eq!(sign(cmp_ci(b"a", b"b")) as i32, -1);
        assert_eq!(sign(cmp_ci(b"b", b"a")) as i32, 1);
    }
}
