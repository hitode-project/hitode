//! lstr* and small arithmetic helpers.

use adv_abi::{GuestPtr, Registry, WideStr, win32};
use adv_cpu::backend::{Guest, Prot};
use adv_machine::{Env, Outcome};
use std::borrow::Cow;

use super::K32;
use crate::stdcall;
use crate::util::{cstr, put_cstr, put_wstr, wstr};

fn lstrcpyn_a(env: &mut Env<'_>) -> Outcome {
    let (dst, src, n) = (env.arg(0), env.arg(1), env.arg(2));
    Outcome::Return(if copy_counted(env.cpu, dst, src, n, false).is_ok() {
        dst
    } else {
        0
    })
}

fn copy_counted(
    cpu: &mut dyn Guest,
    dst: u32,
    src: u32,
    count: u32,
    wide: bool,
) -> Result<(), adv_cpu::backend::MemFault> {
    // Native returns dst for a zero count without touching either pointer.
    // Copy units incrementally: SEH catches access faults after a partial copy.
    if count == 0 {
        return Ok(());
    }
    let width = if wide { 2 } else { 1 };
    let mut unit = [0; 2];
    for index in 0..count {
        let offset = index.checked_mul(width).ok_or(adv_cpu::backend::MemFault {
            addr: dst,
            len: width,
        })?;
        if index == count - 1 {
            unit.fill(0);
        } else {
            cpu.read(src.wrapping_add(offset), &mut unit[..width as usize])?;
        }
        cpu.write(dst.wrapping_add(offset), &unit[..width as usize])?;
        if unit[..width as usize].iter().all(|&byte| byte == 0) {
            break;
        }
    }
    Ok(())
}

#[win32(kernel32, name = "lstrcpynW", level = implement)]
fn lstrcpyn_w(env: &mut Env<'_>, dst: WideStr, src: WideStr, count: i32) -> Outcome {
    Outcome::Return(
        if copy_counted(env.cpu, dst.0.addr(), src.0.addr(), count as u32, true).is_ok() {
            dst.0.addr()
        } else {
            0
        },
    )
}

fn lstrcat_a(env: &mut Env<'_>) -> Outcome {
    let (dst, src) = (env.arg(0), env.arg(1));
    let d = cstr(env.cpu, dst);
    let mut s = cstr(env.cpu, src);
    s.push(0);
    let _ = env.cpu.write(dst + d.len() as u32, &s);
    Outcome::Return(dst)
}

fn append_wide(
    cpu: &mut dyn Guest,
    mut dst: GuestPtr<u16>,
    mut src: GuestPtr<u16>,
) -> Result<(), adv_cpu::backend::MemFault> {
    while dst.read(cpu)? != 0 {
        dst = dst.offset(1)?;
    }
    loop {
        let unit = src.read(cpu)?;
        dst.write(cpu, unit)?;
        if unit == 0 {
            return Ok(());
        }
        dst = dst.offset(1)?;
        src = src.offset(1)?;
    }
}

#[win32(kernel32, name = "lstrcatW", level = implement)]
fn lstrcat_w(env: &mut Env<'_>, dst: WideStr, src: WideStr) -> Outcome {
    // Copy code units, including unpaired surrogates, without text conversion.
    // Windows catches access errors and returns NULL; an already copied prefix
    // is retained and LastError is not part of this API's result contract.
    Outcome::Return(if append_wide(env.cpu, dst.0, src.0).is_ok() {
        dst.0.addr()
    } else {
        0
    })
}

#[cfg(test)]
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

#[cfg(test)]
fn sign(o: std::cmp::Ordering) -> u32 {
    o as i32 as u32
}

fn linguistic(env: &mut Env<'_>, wide: bool, ignore_case: bool) -> Outcome {
    let (a, b) = if wide {
        (wstr(env.cpu, env.arg(0)), wstr(env.cpu, env.arg(1)))
    } else {
        match (
            super::nlsapi::decode_comparison(env, &name_bytes(env.cpu, env.arg(0))),
            super::nlsapi::decode_comparison(env, &name_bytes(env.cpu, env.arg(1))),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(error), _) | (_, Err(error)) => {
                crate::util::set_last_error(env, error);
                return Outcome::Return((-2i32) as u32);
            }
        }
    };
    let locale = super::nlsapi::lcid(env);
    Outcome::Return(
        super::nlsapi::compare(env, locale, u32::from(ignore_case), &a, &b).wrapping_sub(2),
    )
}

fn string_length(env: &mut Env<'_>, wide: bool) -> Outcome {
    let pointer = env.arg(0);
    let width = if wide { 2 } else { 1 };
    let mut length = 0;
    let mut buffer = [0; 256];
    while length < crate::util::MAX_STR as u32 {
        let address = pointer.wrapping_add(length * width);
        // Read only this page, except for an unaligned UTF-16 boundary unit.
        let available = (4096 - (address & 4095)).max(width) / width * width;
        let size = available
            .min(buffer.len() as u32)
            .min((crate::util::MAX_STR as u32 - length) * width);
        if !env.cpu.accessible(address, size, Prot::READ)
            || env.cpu.read(address, &mut buffer[..size as usize]).is_err()
        {
            return Outcome::Return(0);
        }
        if let Some(end) = buffer[..size as usize]
            .chunks_exact(width as usize)
            .position(|unit| unit.iter().all(|&byte| byte == 0))
        {
            return Outcome::Return(length + end as u32);
        }
        length += size / width;
    }
    Outcome::Return(length)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "lstrlenA", 1, |e| string_length(e, false));
    stdcall(reg, K32, "lstrlenW", 1, |e| string_length(e, true));
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
    reg.register(lstrcpyn_w_ABI);
    stdcall(reg, K32, "lstrcatA", 2, lstrcat_a);
    reg.register(lstrcat_w_ABI);
    stdcall(reg, K32, "lstrcmpA", 2, |e| linguistic(e, false, false));
    stdcall(reg, K32, "lstrcmpiA", 2, |e| linguistic(e, false, true));
    stdcall(reg, K32, "lstrcmpW", 2, |e| linguistic(e, true, false));
    stdcall(reg, K32, "lstrcmpiW", 2, |e| linguistic(e, true, true));
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
    fn wide_append_preserves_units_and_reports_partial_access_failure() {
        let mut cpu = JitCpu::new(PARK_ADDR, false).unwrap();
        cpu.map(0x400000, 4096, Prot::RW).unwrap();
        cpu.write(0x400000, &[0x2d, 0x4e, 0, 0]).unwrap();
        cpu.write(0x400100, &[0, 0xd8, 0x87, 0x65, 0, 0]).unwrap();
        append_wide(&mut cpu, GuestPtr::new(0x400000), GuestPtr::new(0x400100)).unwrap();
        let mut result = [0; 8];
        cpu.read(0x400000, &mut result).unwrap();
        assert_eq!(result, [0x2d, 0x4e, 0, 0xd8, 0x87, 0x65, 0, 0]);
        cpu.write(0x400ffe, &[0, 0]).unwrap();
        assert!(append_wide(&mut cpu, GuestPtr::new(0x400ffe), GuestPtr::new(0x400100)).is_err());
        let mut prefix = [0; 2];
        cpu.read(0x400ffe, &mut prefix).unwrap();
        assert_eq!(prefix, [0, 0xd8]);
        assert!(append_wide(&mut cpu, GuestPtr::new(0), GuestPtr::new(0x400100)).is_err());
    }

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
