//! lstr* and small arithmetic helpers.

use adv_machine::{Env, Outcome, Registry};

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
        Outcome::Return(sign(cmp_ci(&cstr(e.cpu, e.arg(0)), &cstr(e.cpu, e.arg(1)))))
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
