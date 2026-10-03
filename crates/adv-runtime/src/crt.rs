//! Small C-runtime services used by guest library DLLs. Library algorithms remain guest code.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::{Conv, Env, Outcome};
use adv_os::Heaps;
use std::collections::HashMap;

#[derive(Default)]
struct CrtState {
    errno: HashMap<u32, u32>,
    locks: HashMap<(String, u32), u32>,
    globals: HashMap<(String, String), u32>,
}

pub fn install<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), String> {
    let data = machine
        .resolve_import("msvcrt.dll", "_adjust_fdiv")
        .map_err(|e| e.to_string())?;
    machine.cpu.write_u32(data, 0).map_err(|e| e.to_string())?;
    machine.ext.insert(CrtState::default());
    Ok(())
}

pub fn register(r: &mut Registry) {
    for (name, argc) in [
        ("malloc", 1),
        ("calloc", 2),
        ("realloc", 2),
        ("free", 1),
        ("memcpy", 3),
        ("memmove", 3),
        ("memset", 3),
        ("memcmp", 3),
        ("strlen", 1),
        ("strcmp", 2),
        ("_initterm", 2),
        ("_initterm_e", 2),
        ("_lock", 1),
        ("_unlock", 1),
        ("__dllonexit", 3),
        ("__set_app_type", 1),
        ("__p__fmode", 0),
        ("__p__commode", 0),
        ("_adjust_fdiv", 0),
        ("_errno", 0),
        ("toupper", 1),
        ("qsort", 4),
        ("floor", 2),
        ("ceil", 2),
        ("ldexp", 3),
        ("frexp", 3),
        ("_CIpow", 0),
        ("_ftol", 0),
    ] {
        r.add("msvcrt.dll", name, Conv::Cdecl { argc }, call);
    }
    // Whitelist implemented x86 cdecl constructor and heap helpers. Unknown
    // functions keep the loader's explicit unsupported handler.
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        for name in ["_initterm", "_initterm_e"] {
            r.add(module, name, Conv::Cdecl { argc: 2 }, call);
        }
        for name in ["_lock", "_unlock"] {
            r.add(module, name, Conv::Cdecl { argc: 1 }, call);
        }
        r.add(module, "__dllonexit", Conv::Cdecl { argc: 3 }, call);
        for (name, argc) in [
            ("__set_app_type", 1),
            ("__p__fmode", 0),
            ("__p__commode", 0),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
        for (name, argc) in [
            ("_malloc_crt", 1),
            ("malloc", 1),
            ("calloc", 2),
            ("realloc", 2),
            ("free", 1),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
        for (name, argc) in [
            ("_encode_pointer", 1),
            ("_decode_pointer", 1),
            ("_encoded_null", 0),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
    }
}
fn double(e: &Env<'_>) -> Result<f64, String> {
    let mut b = [0; 8];
    e.cpu
        .read(e.cpu.reg(Reg::Esp).wrapping_add(4), &mut b)
        .map_err(|e| e.to_string())?;
    Ok(f64::from_le_bytes(b))
}
fn split_fraction(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        let (v, e) = split_fraction(x * 18014398509481984.0);
        return (v, e - 54);
    }
    (
        f64::from_bits((bits & 0x800fffffffffffff) | (1022u64 << 52)),
        exp - 1022,
    )
}
fn scale(mut x: f64, mut exp: i32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    exp = exp.clamp(-4096, 4096);
    while exp > 512 {
        x *= 2f64.powi(512);
        exp -= 512;
    }
    while exp < -512 {
        x *= 2f64.powi(-512);
        exp += 512;
    }
    x * 2f64.powi(exp)
}
fn sort_step(
    _e: &mut Env<'_>,
    base: u32,
    count: u32,
    size: u32,
    cmp: u32,
    i: u32,
    j: u32,
) -> Outcome {
    if i >= count {
        return Outcome::Return(0);
    }
    let (i, j) = if j == 0 { (i + 1, i + 1) } else { (i, j) };
    if i >= count {
        return Outcome::Return(0);
    }
    let left = base + (j - 1) * size;
    let right = base + j * size;
    Outcome::call(cmp, [left, right], move |e, result| {
        if result.value() as i32 > 0 {
            let mut temporary = vec![0; size as usize];
            if let Err(err) = e
                .cpu
                .read(left, &mut temporary)
                .and_then(|_| e.cpu.copy_memory(right, left, size as usize))
                .and_then(|_| e.cpu.write(right, &temporary))
            {
                return Outcome::Fail(err.to_string());
            }
            sort_step(e, base, count, size, cmp, i, j - 1)
        } else {
            sort_step(e, base, count, size, cmp, i + 1, i + 1)
        }
    })
}
fn allocate(e: &mut Env<'_>, size: u32, zero: bool) -> u32 {
    if size > 0x40000000 {
        return 0;
    }
    let heaps = e.ext.get_mut::<Heaps>();
    let h = heaps.process_heap;
    heaps
        .alloc(e.cpu, &mut e.sys.vm, h, size, zero)
        .unwrap_or(0)
}
fn initializers(e: &mut Env<'_>, mut p: u32, end: u32, stop_on_error: bool) -> Outcome {
    while p < end {
        let Ok(target) = e.cpu.read_u32(p) else {
            return Outcome::Fail("CRT initializer table unmapped".into());
        };
        p += 4;
        if target != 0 {
            return Outcome::call(target, vec![], move |e, result| {
                if stop_on_error && result.value() != 0 {
                    Outcome::Return(result.value())
                } else {
                    initializers(e, p, end, stop_on_error)
                }
            });
        }
    }
    Outcome::Return(0)
}
fn call(e: &mut Env<'_>) -> Outcome {
    let name = e.sys.thunks.get(e.thunk).name.clone();
    let arg = |i| e.arg(i);
    let (a, b, c) = (arg(0), arg(1), arg(2));
    let value = match name.as_str() {
        "__set_app_type" | "__p__fmode" | "__p__commode" => {
            if e.ext.try_get::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let key = (
                e.sys.thunks.get(e.thunk).module.clone(),
                if name == "__set_app_type" {
                    "app_type"
                } else {
                    name.trim_start_matches("__p__")
                }
                .to_owned(),
            );
            let existing = e.ext.get::<CrtState>().globals.get(&key).copied();
            let pointer = if let Some(pointer) = existing {
                pointer
            } else {
                let pointer = allocate(e, 4, true);
                if pointer == 0 {
                    return Outcome::Fail("CRT global allocation failed".into());
                }
                if name == "__p__fmode"
                    && let Err(error) = e.cpu.write_u32(pointer, 0x4000)
                {
                    return Outcome::Fail(error.to_string());
                }
                e.ext.get_mut::<CrtState>().globals.insert(key, pointer);
                pointer
            };
            if name == "__set_app_type" {
                if let Err(error) = e.cpu.write_u32(pointer, a) {
                    return Outcome::Fail(error.to_string());
                }
                0
            } else {
                pointer
            }
        }
        "__dllonexit" => {
            if !e.cpu.accessible(b, 4, adv_cpu::backend::Prot::RW)
                || !e.cpu.accessible(c, 4, adv_cpu::backend::Prot::RW)
            {
                return Outcome::Fail("CRT exit table pointer is inaccessible".into());
            }
            let (Ok(begin), Ok(end)) = (e.cpu.read_u32(b), e.cpu.read_u32(c)) else {
                return Outcome::Fail("CRT exit table pointer is unmapped".into());
            };
            let Some(used) = end
                .checked_sub(begin)
                .filter(|used| used.is_multiple_of(4) && *used < 1024 * 1024)
            else {
                return Outcome::Fail("CRT exit table range is invalid".into());
            };
            let needed = used + 4;
            let pointer = if begin == 0 {
                allocate(e, needed, false)
            } else {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps
                    .realloc(
                        e.cpu,
                        &mut e.sys.vm,
                        heaps.process_heap,
                        begin,
                        needed,
                        false,
                        false,
                    )
                    .unwrap_or(0)
            };
            if pointer == 0 {
                return Outcome::Return(0);
            }
            if let Err(error) = e
                .cpu
                .write_u32(pointer + used, a)
                .and_then(|_| e.cpu.write_u32(b, pointer))
                .and_then(|_| e.cpu.write_u32(c, pointer + needed))
            {
                return Outcome::Fail(error.to_string());
            }
            a
        }
        "_lock" | "_unlock" => {
            if a >= 64 {
                return Outcome::Fail("CRT lock index outside the supported table".into());
            }
            if e.ext.try_get::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let module = e.sys.thunks.get(e.thunk).module.clone();
            let state = e.ext.get_mut::<CrtState>();
            let handle = *state.locks.entry((module, a)).or_insert_with(|| {
                e.sys.objects.insert(adv_machine::Object::Mutex {
                    owner: None,
                    recursion: 0,
                })
            });
            if name == "_lock" {
                if e.sys.try_acquire(handle) {
                    return Outcome::Return(0);
                }
                return Outcome::block(
                    adv_machine::Wait::Object {
                        handle,
                        deadline: None,
                    },
                    |_, _| Outcome::Return(0),
                );
            }
            match e.sys.objects.get_mut(handle) {
                Some(adv_machine::Object::Mutex { owner, recursion })
                    if *owner == Some(e.sys.current) =>
                {
                    *recursion -= 1;
                    if *recursion == 0 {
                        *owner = None;
                    }
                }
                _ => return Outcome::Fail("CRT unlock by a non-owner".into()),
            }
            e.sys.signal_waiters(handle);
            0
        }
        "malloc" | "_malloc_crt" => allocate(e, a, false),
        "_encode_pointer" | "_decode_pointer" | "_encoded_null" => {
            let kernel = if name == "_decode_pointer" {
                "DecodePointer"
            } else {
                "EncodePointer"
            };
            let target = match e.sys.thunks.resolve(e.cpu, "kernel32.dll", kernel) {
                Ok(target) => target,
                Err(error) => return Outcome::Fail(error.to_string()),
            };
            return Outcome::call(
                target,
                [if name == "_encoded_null" { 0 } else { a }],
                |_, value| Outcome::Return(value.value()),
            );
        }
        "calloc" => a.checked_mul(b).map_or(0, |n| allocate(e, n, true)),
        "free" => {
            if a != 0 {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, a);
            }
            0
        }
        "realloc" => {
            if a == 0 {
                allocate(e, b, false)
            } else if b == 0 {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, a);
                0
            } else {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps
                    .realloc(e.cpu, &mut e.sys.vm, heaps.process_heap, a, b, false, false)
                    .unwrap_or(0)
            }
        }
        "memcpy" | "memmove" => {
            if let Err(err) = e.cpu.copy_memory(b, a, c as usize) {
                return Outcome::Fail(err.to_string());
            }
            a
        }
        "memset" => {
            let chunk = [b as u8; 4096];
            let mut p = 0;
            while p < c {
                let n = (c - p).min(4096);
                if let Err(err) = e.cpu.write(a.wrapping_add(p), &chunk[..n as usize]) {
                    return Outcome::Fail(err.to_string());
                }
                p += n;
            }
            a
        }
        "memcmp" => {
            let mut left = [0; 4096];
            let mut right = [0; 4096];
            let mut offset = 0;
            let mut diff = 0;
            while offset < c {
                let n = (c - offset).min(4096) as usize;
                if let Err(err) = e
                    .cpu
                    .read(a.wrapping_add(offset), &mut left[..n])
                    .and_then(|_| e.cpu.read(b.wrapping_add(offset), &mut right[..n]))
                {
                    return Outcome::Fail(err.to_string());
                }
                if let Some((l, r)) = left[..n].iter().zip(&right[..n]).find(|(l, r)| l != r) {
                    diff = i32::from(*l) - i32::from(*r);
                    break;
                }
                offset += n as u32;
            }
            diff as u32
        }
        "strlen" => match e.cpu.read_cstr(a, 16 * 1024 * 1024) {
            Ok(s) => s.len() as u32,
            Err(err) => return Outcome::Fail(err.to_string()),
        },
        "strcmp" => {
            let (Ok(l), Ok(r)) = (
                e.cpu.read_cstr(a, 16 * 1024 * 1024),
                e.cpu.read_cstr(b, 16 * 1024 * 1024),
            ) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            match l.cmp(&r) {
                std::cmp::Ordering::Less => u32::MAX,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        }
        "_initterm" | "_initterm_e" => {
            if b < a || b - a > 1024 * 1024 || !(b - a).is_multiple_of(4) {
                return Outcome::Fail("invalid initializer range".into());
            }
            return initializers(e, a, b, name == "_initterm_e");
        }
        "_adjust_fdiv" => 0,
        "_errno" => {
            if e.ext.try_get_mut::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let ctx = e.sys.current;
            if let Some(&p) = e.ext.get::<CrtState>().errno.get(&ctx) {
                p
            } else {
                let p = allocate(e, 4, true);
                e.ext.get_mut::<CrtState>().errno.insert(ctx, p);
                p
            }
        }
        "toupper" => {
            if a <= 255 {
                u32::from((a as u8).to_ascii_uppercase())
            } else {
                a
            }
        }
        "qsort" => {
            if b < 2 || c == 0 {
                return Outcome::Return(0);
            }
            if c > 1024 * 1024 || b.checked_mul(c).and_then(|n| a.checked_add(n)).is_none() {
                return Outcome::Fail("invalid qsort range".into());
            }
            let cmp = e.arg(3);
            return sort_step(e, a, b, c, cmp, 1, 1);
        }
        "floor" | "ceil" | "ldexp" | "frexp" => {
            let x = match double(e) {
                Ok(v) => v,
                Err(err) => return Outcome::Fail(err),
            };
            let result = match name.as_str() {
                "floor" => x.floor(),
                "ceil" => x.ceil(),
                "ldexp" => scale(x, c as i32),
                _ => {
                    let (v, exp) = split_fraction(x);
                    if let Err(err) = e.cpu.write_u32(c, exp as u32) {
                        return Outcome::Fail(err.to_string());
                    }
                    v
                }
            };
            e.cpu.push_f64(result);
            0
        }
        "_CIpow" => {
            let (Some(y), Some(x)) = (e.cpu.pop_f64(), e.cpu.pop_f64()) else {
                return Outcome::Fail("_CIpow: empty x87 stack".into());
            };
            e.cpu.push_f64(x.powf(y));
            0
        }
        "_ftol" => {
            let Some(x) = e.cpu.pop_f64() else {
                return Outcome::Fail("_ftol: empty x87 stack".into());
            };
            let value = if !x.is_finite()
                || !(-9223372036854775808.0..9223372036854775808.0).contains(&x)
            {
                i64::MIN
            } else {
                x.trunc() as i64
            };
            return Outcome::Return64(value as u64);
        }
        _ => {
            return Outcome::Fail(format!(
                "unsupported CRT {name} at {:#x}",
                e.cpu.reg(Reg::Eip)
            ));
        }
    };
    Outcome::Return(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, Prot};
    use adv_machine::{Clock, Machine, RunResult};
    use iced_x86::code_asm::*;
    fn machine(jit: bool) -> Machine<JitCpu> {
        let mut reg = Registry::default();
        adv_win32::register(&mut reg);
        register(&mut reg);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        adv_win32::Win32::install(
            &mut m,
            adv_win32::Win32Config::new(".", "crt-test.exe", "target/crt-test"),
        )
        .unwrap();
        install(&mut m).unwrap();
        m
    }
    #[test]
    fn qsort_calls_guest_comparator_and_initializer_callbacks_in_order() {
        for jit in [false, true] {
            let mut m = machine(jit);
            let code = 0x400000;
            let data = 0x500000;
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            m.cpu.map(data, 4096, Prot::RW).unwrap();
            let sort = m.resolve_import("msvcrt.dll", "qsort").unwrap();
            let init = m.resolve_import("msvcrt.dll", "_initterm").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.push(data + 8).unwrap();
            a.push(data).unwrap();
            a.call(u64::from(init)).unwrap();
            a.add(esp, 8).unwrap();
            a.push(code + 0x100).unwrap();
            a.push(4).unwrap();
            a.push(5).unwrap();
            a.push(data + 32).unwrap();
            a.call(u64::from(sort)).unwrap();
            a.add(esp, 16).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.mov(eax, dword_ptr(esp + 4)).unwrap();
            a.mov(eax, dword_ptr(eax)).unwrap();
            a.mov(edx, dword_ptr(esp + 8)).unwrap();
            a.sub(eax, dword_ptr(edx)).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code + 0x100, &a.assemble(u64::from(code + 0x100)).unwrap())
                .unwrap();
            for (pc, mult, add) in [(code + 0x200, 1, 3), (code + 0x220, 2, 1)] {
                let mut a = CodeAssembler::new(32).unwrap();
                a.mov(eax, dword_ptr((data + 16) as i32)).unwrap();
                if mult == 2 {
                    a.add(eax, eax).unwrap();
                }
                a.add(eax, add).unwrap();
                a.mov(dword_ptr((data + 16) as i32), eax).unwrap();
                a.ret().unwrap();
                m.cpu
                    .write(pc, &a.assemble(u64::from(pc)).unwrap())
                    .unwrap();
            }
            m.cpu
                .write(
                    data,
                    &[code + 0x200, code + 0x220]
                        .into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            m.cpu
                .write(
                    data + 32,
                    &[7i32, -3, 0, 7, -9]
                        .into_iter()
                        .flat_map(i32::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            m.spawn("crt", code, &[], 65536).unwrap();
            assert!(matches!(m.run(), RunResult::Exited(0)));
            assert_eq!(m.cpu.read_u32(data + 16).unwrap(), 7);
            let mut sorted = [0; 20];
            m.cpu.read(data + 32, &mut sorted).unwrap();
            assert_eq!(
                sorted.to_vec(),
                [-9i32, -3, 0, 7, 7]
                    .into_iter()
                    .flat_map(i32::to_le_bytes)
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn crt_globals_store_app_type_and_invalid_ranges_or_lock_owners_fail_loudly() {
        for jit in [false, true] {
            let code = 0x400000;
            let data = 0x500000;
            let cases = [
                ("_initterm_e", vec![0xdead0000, 0xdead0004]),
                ("_initterm_e", vec![data, data + 3]),
                ("_lock", vec![64]),
                ("_unlock", vec![8]),
                ("__dllonexit", vec![code, data + 16, data + 20]),
            ];
            for (name, args) in cases {
                let mut m = machine(jit);
                m.cpu.map(code, 4096, Prot::RX).unwrap();
                m.cpu.map(data, 4096, Prot::RW).unwrap();
                m.cpu.write_u32(data + 16, data + 64).unwrap();
                m.cpu.write_u32(data + 20, data + 63).unwrap();
                let target = m.resolve_import("msvcr90.dll", name).unwrap();
                let mut a = CodeAssembler::new(32).unwrap();
                for value in args.iter().rev() {
                    a.push(*value).unwrap();
                }
                a.call(u64::from(target)).unwrap();
                a.add(esp, (args.len() * 4) as i32).unwrap();
                a.xor(eax, eax).unwrap();
                a.ret().unwrap();
                m.cpu
                    .write(code, &a.assemble(u64::from(code)).unwrap())
                    .unwrap();
                m.spawn("invalid-crt", code, &[], 65536).unwrap();
                assert!(matches!(m.run(), RunResult::Failed(_)), "{name} jit={jit}");
            }
            let mut m = machine(jit);
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            let target = m.resolve_import("msvcr90.dll", "__set_app_type").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.push(2).unwrap();
            a.call(u64::from(target)).unwrap();
            a.add(esp, 4).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            m.spawn("crt-global", code, &[], 65536).unwrap();
            assert!(matches!(m.run(), RunResult::Exited(0)));
            let pointer =
                m.ext.get::<CrtState>().globals[&("msvcr90.dll".into(), "app_type".into())];
            assert_eq!(m.cpu.read_u32(pointer).unwrap(), 2);
        }
    }
    #[test]
    fn frexp_ldexp_helpers_preserve_subnormal_sign_and_full_double_precision() {
        for v in [
            0.0,
            -0.0,
            1.0000000000000002,
            -12345.6789,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::MAX,
        ] {
            let (fraction, exponent) = split_fraction(v);
            assert_eq!(scale(fraction, exponent).to_bits(), v.to_bits());
        }
    }
}
