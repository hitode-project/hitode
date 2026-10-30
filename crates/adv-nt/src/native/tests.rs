use super::*;
use adv_cpu::backend::{GuestExt, MemFault, Reg};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct Allocator;
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
fn allocated() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
    }
}
// SAFETY: allocation and deallocation use System unchanged; counting is thread-local.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

const PAGE: usize = 4096;
struct TestGuest {
    bytes: Box<[u8; 8 * PAGE]>,
    regs: [u32; 16],
    lend: bool,
    access: Cell<HostAccess>,
}
impl TestGuest {
    fn range(&self, addr: u32, len: usize, prot: Prot) -> Option<std::ops::Range<usize>> {
        let first = addr as usize;
        let end = first.checked_add(len)?;
        if end > self.bytes.len() {
            return None;
        }
        let permissions = [0, 1, 3, 3, 1, 3, 0, 3];
        if len != 0
            && (first / PAGE..=(end - 1) / PAGE)
                .any(|i| permissions[i] == 0 || permissions[i] & prot.0 != prot.0)
        {
            return None;
        }
        Some(first..end)
    }
    fn writes(&self, len: usize) {
        let mut access = self.access.get();
        access.writes += 1;
        access.written_bytes += len as u64;
        self.access.set(access);
    }
}
impl Guest for TestGuest {
    fn reg(&self, reg: Reg) -> u32 {
        self.regs[reg as usize]
    }
    fn set_reg(&mut self, reg: Reg, value: u32) {
        self.regs[reg as usize] = value;
    }
    fn read(&self, addr: u32, out: &mut [u8]) -> Result<(), MemFault> {
        let range = self.range(addr, out.len(), Prot::READ).ok_or(MemFault {
            addr,
            len: out.len() as u32,
        })?;
        let mut access = self.access.get();
        access.reads += 1;
        access.read_bytes += out.len() as u64;
        self.access.set(access);
        out.copy_from_slice(&self.bytes[range]);
        Ok(())
    }
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
        let range = self.range(addr, bytes.len(), Prot::WRITE).ok_or(MemFault {
            addr,
            len: bytes.len() as u32,
        })?;
        self.writes(bytes.len());
        self.bytes[range].copy_from_slice(bytes);
        Ok(())
    }
    fn accessible(&self, addr: u32, len: u32, prot: Prot) -> bool {
        self.range(addr, len as usize, prot).is_some()
    }
    fn shared_bytes(&self, addr: u32, len: usize) -> Option<&[u8]> {
        self.lend
            .then_some(&self.bytes[self.range(addr, len, Prot::READ)?])
    }
    fn writable_bytes(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        if !self.lend {
            return None;
        }
        let range = self.range(addr, len, Prot::WRITE)?;
        self.writes(len);
        Some(&mut self.bytes[range])
    }
    fn host_access(&self) -> HostAccess {
        self.access.get()
    }
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        let fault = MemFault {
            addr: dst,
            len: len as u32,
        };
        let from = self.range(src, len, Prot::READ).ok_or(fault)?;
        let to = self.range(dst, len, Prot::WRITE).ok_or(fault)?;
        self.writes(len);
        self.bytes.copy_within(from, to.start);
        Ok(())
    }
    fn fill_memory(&mut self, addr: u32, byte: u8, len: usize) -> Result<(), MemFault> {
        let range = self.range(addr, len, Prot::WRITE).ok_or(MemFault {
            addr,
            len: len as u32,
        })?;
        self.writes(len);
        self.bytes[range].fill(byte);
        Ok(())
    }
    fn map(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
        Err(MemFault { addr, len })
    }
    fn protect(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
        Err(MemFault { addr, len })
    }
    fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        Err(MemFault { addr, len })
    }
}
fn guest() -> TestGuest {
    let mut cpu = TestGuest {
        bytes: Box::new([0; 8 * PAGE]),
        regs: [0; 16],
        lend: true,
        access: Cell::new(HostAccess::default()),
    };
    cpu.regs[Reg::Esp as usize] = 7 * PAGE as u32;
    cpu
}
fn machine() -> Machine<adv_cpu::Cpu> {
    Machine::new(
        adv_cpu::Cpu::new(PARK_ADDR, false).unwrap(),
        crate::dispatch::Platform,
        Clock::virtual_time(),
    )
    .unwrap()
}

// spec: NAT-BODY-1, NAT-BODY-15, NAT-BODY-16
#[test]
fn native_body_decline_allocation_gate() {
    let mut machine = machine();
    let context = machine
        .spawn("native-gate", PARK_ADDR, &[], 0x10000)
        .unwrap();
    let mut cpu = guest();
    let mut seed = 0x67a5_1392_d3c8_4e01u64;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as u32
    };
    for byte in cpu.bytes.iter_mut() {
        *byte = random() as u8;
    }
    let mut calls = 0;
    for (index, body) in BODIES.iter().chain(dib::BODIES).enumerate() {
        let function = body.body;
        machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, &body.name)
            .unwrap();
        let thunk = machine
            .sys
            .thunks
            .published_index(MODULE, &body.name)
            .unwrap();
        // Where a fault on the full path is raised.
        if let Some(direct) = body.direct {
            machine
                .ext
                .get_or_default::<Entries>()
                .insert(thunk, 0x1000, direct.direct);
        }
        for round in 0..64 {
            for i in 0..8 {
                let value = if round == 0 {
                    0
                } else if round == 1 {
                    [0x3100, 0x4100, 16, 0x5100, 8, 0, 0, 0][i]
                } else if random() & 1 == 0 {
                    random() % 65
                } else {
                    (random() % 8) * PAGE as u32 + random() % 64
                };
                cpu.write_u32(7 * PAGE as u32 + 4 + i as u32 * 4, value)
                    .unwrap();
            }
            let before = cpu.bytes.clone();
            let registers = cpu.regs;
            for repeat in 0..2 {
                cpu.bytes.copy_from_slice(before.as_slice());
                cpu.regs = registers;
                let mut env = Env {
                    cpu: &mut cpu,
                    sys: &mut machine.sys,
                    ext: &mut machine.ext,
                    context,
                    thunk,
                };
                ALLOCATIONS.set(0);
                COUNTING.set(repeat == 1 && index < BODIES.len());
                // Direct bodies never decline; their full path raises
                // faults before it writes (NAT-BODY-16, A64-CALL-4).
                let unchanged = match (function, body.direct) {
                    (_, Some(direct)) => match run_full(&mut env, direct, body.handler) {
                        Outcome::Raise(_) => true,
                        Outcome::Return(_) | Outcome::Return64(_) => false,
                        _ => panic!("{} failed on its full path", body.name),
                    },
                    (Some(function), None) => function(&mut env).is_none(),
                    (None, None) => panic!("{} has no body", body.name),
                };
                COUNTING.set(false);
                let allocations = ALLOCATIONS.get();
                assert!(
                    !unchanged || cpu.bytes == before,
                    "{} declined or faulted after mutation, round {round}",
                    body.name
                );
                assert_eq!(
                    allocations, 0,
                    "{} allocated on its repeated call, round {round}",
                    body.name
                );
                calls += 1;
            }
        }
        println!("gate {}", body.name);
    }
    println!(
        "gate coverage: {} ordinary bodies, {} DIB bodies; {calls} calls; DIB allocation checks excluded by contract",
        BODIES.len(),
        dib::BODIES.len()
    );
}

// spec: NAT-BODY-1, NAT-BODY-15
#[test]
#[should_panic(expected = "test!write_then_decline declined after guest writes")]
fn native_boundary_rejects_write_then_decline() {
    fn bad(env: &mut Env<'_>) -> Option<u64> {
        env.cpu.write(0x3000, &[1]).unwrap();
        None
    }
    static BAD: Body = Body {
        name: std::borrow::Cow::Borrowed("test!write_then_decline"),
        conv: Conv::Cdecl { argc: 0 },
        body: Some(bad),
        handler: resume_original,
        direct: None,
    };
    let mut machine = machine();
    let mut cpu = guest();
    run_body(
        &mut Env {
            cpu: &mut cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            context: 0,
            thunk: 0,
        },
        0,
        &BAD,
    );
}

// spec: NAT-BODY-15
fn waits<M: adv_cpu::body::Mem>(
    _: &mut M,
    _: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    Err(adv_cpu::body::Stop::Guest)
}
adv_cpu::body!(WAITS = waits, inputs: &[], outputs: adv_cpu::body::EAX, pop: 0);

fn guest_code(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(7)
}

// spec: A64-CALL-6
#[test]
fn a_body_that_would_wait_runs_its_guest_code_once() {
    let mut machine = machine();
    let mut cpu = guest();
    let mut env = Env {
        cpu: &mut cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        context: 0,
        thunk: 0,
    };
    assert!(matches!(
        run_full(&mut env, WAITS, guest_code),
        Outcome::Return(7)
    ));
    assert_eq!(machine.ext.get::<Declines>().0, 1);
}

#[test]
fn native_boundary_counts_untimed_calls_and_keeps_tail_ring() {
    let mut machine = machine();
    let mut cpu = guest();
    let at = 7 * PAGE as u32 + 4;
    for (i, value) in [0x3000, 0x4000].iter().enumerate() {
        cpu.write_u32(at + i as u32 * 4, *value).unwrap();
    }
    cpu.bytes[0x4000..0x4010].copy_from_slice(b"fifteen letters\0");
    fn copy(env: &mut Env<'_>) -> Option<u64> {
        env.cpu.copy_memory(env.arg(1), env.arg(0), 16).ok()?;
        Some(u64::from(env.arg(0)))
    }
    static ORDINARY: Body = Body {
        name: std::borrow::Cow::Borrowed("test!ordinary_copy"),
        conv: Conv::Cdecl { argc: 2 },
        body: Some(copy),
        handler: resume_original,
        direct: None,
    };
    run_body(
        &mut Env {
            cpu: &mut cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            context: 0,
            thunk: 0,
        },
        0,
        &ORDINARY,
    );
    debugger_attached(true);
    run_body(
        &mut Env {
            cpu: &mut cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            context: 0,
            thunk: 0,
        },
        1,
        &ORDINARY,
    );
    debugger_attached(false);
    assert_eq!(machine.ext.get::<Boundary>().bodies[1].timed_calls, 1);
    let boundary = machine.ext.get_mut::<Boundary>();
    let row = boundary.bodies[0];
    assert_eq!(
        (row.calls, row.declines, row.nanos, row.timed_calls),
        (1, 0, 0, 0)
    );
    assert_eq!(row.access.written_bytes, 16);
    for _ in 0..1000 {
        boundary.record(
            0,
            false,
            HostAccess::default(),
            Some(std::time::Duration::from_nanos(1)),
            [0; 6],
        );
    }
    boundary.record(
        0,
        false,
        HostAccess::default(),
        Some(std::time::Duration::from_micros(100)),
        [0; 6],
    );
    assert_eq!(boundary.tails().count(), 0);
    for i in 0..20 {
        boundary.record(
            0,
            false,
            HostAccess::default(),
            Some(std::time::Duration::from_micros(200)),
            [i; 6],
        );
    }
    let tails: Vec<_> = boundary.tails().collect();
    assert_eq!(tails.len(), 16);
    assert_eq!(tails[0].arguments, [4; 6]);
    assert_eq!(tails[15].arguments, [19; 6]);
    let delta = boundary.bodies[0].since(row);
    assert_eq!(delta.calls, 1021);
}

// spec: NAT-BODY-15
#[test]
fn native_scratch_reuses_storage() {
    let mut ext = Ext::default();
    let bytes = scratch(&mut ext, 4096);
    bytes.fill(1);
    let pointer = bytes.as_ptr();
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    let bytes = scratch(&mut ext, 1024);
    COUNTING.set(false);
    assert_eq!(bytes.as_ptr(), pointer);
    assert_eq!(ALLOCATIONS.get(), 0);
}

// spec: NAT-BODY-15
#[test]
fn native_memo_tracks_pinned_storage_and_type() {
    use std::{io::Write, os::fd::AsFd};
    let mut machine = machine();
    let path = std::env::temp_dir().join(format!("adv-native-memo-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file.write_all(&[0x31; PAGE]).unwrap();
    let address = 0x400000;
    machine
        .cpu
        .map_shared_file(address, PAGE as u32, file.as_fd(), 0, Prot::READ)
        .unwrap();
    let derived = Cell::new(0);
    let mut env = Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        context: 0,
        thunk: 0,
    };
    assert_eq!(
        *memo(&mut env, address, 16, |bytes| {
            derived.set(derived.get() + 1);
            u32::from(bytes[0])
        })
        .unwrap(),
        0x31
    );
    let counts = env.cpu.host_access();
    assert_eq!(*memo(&mut env, address, 16, |_| 0u32).unwrap(), 0x31);
    assert_eq!(env.cpu.host_access(), counts);
    assert_eq!(*memo(&mut env, address, 16, |_| 7u64).unwrap(), 7);
    env.cpu.map(0x500000, PAGE as u32, Prot::RW).unwrap();
    assert_eq!(*memo(&mut env, address, 16, |_| 0u32).unwrap(), 0x31);
    env.cpu.protect(address, PAGE as u32, Prot::RW).unwrap();
    assert!(memo(&mut env, address, 16, |_| 0u32).is_none());
    env.cpu.unmap(address, PAGE as u32).unwrap();
    let mut other = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    other.write_all(&[0x72; PAGE]).unwrap();
    env.cpu
        .map_shared_file(address, PAGE as u32, other.as_fd(), 0, Prot::READ)
        .unwrap();
    assert_eq!(
        *memo(&mut env, address, 16, |bytes| {
            derived.set(derived.get() + 1);
            u32::from(bytes[0])
        })
        .unwrap(),
        0x72
    );
    assert_eq!(derived.get(), 2);
}

// spec: NAT-BODY-5, NAT-BODY-15
#[test]
fn native_nls_memo_and_output_scratch_allocate_only_when_cold() {
    use std::{io::Write, os::fd::AsFd};
    let mut machine = machine();
    let context = machine
        .spawn("nls-scratch", PARK_ADDR, &[], 0x10000)
        .unwrap();
    let base = 0x1000000;
    crate::identity::note_image(
        &mut machine.ext,
        base,
        0x100000,
        Some(std::path::Path::new(
            "fixture/lib/wine/i386-windows/kernelbase.dll",
        )),
        &[],
    );
    machine
        .cpu
        .map(base + 0xc0000, PAGE as u32, Prot::RW)
        .unwrap();
    let mut info = [0; 44];
    info[..2].copy_from_slice(&932u16.to_le_bytes());
    info[12..14].copy_from_slice(&1u16.to_le_bytes());
    let tables = 0x3000000u32;
    info[28..32].copy_from_slice(&tables.to_le_bytes());
    info[40..44].copy_from_slice(&(tables + 512).to_le_bytes());
    machine.cpu.write(base + 0xc0b00, &info).unwrap();
    let path = std::env::temp_dir().join(format!("adv-native-nls-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let mut bytes = [0; PAGE];
    for i in 0..256 {
        bytes[2 * i..2 * i + 2].copy_from_slice(&(i as u16).to_le_bytes());
    }
    file.write_all(&bytes).unwrap();
    machine
        .cpu
        .map_shared_file(tables, PAGE as u32, file.as_fd(), 0, Prot::READ)
        .unwrap();
    let src = 0x4000000;
    let dst = src + PAGE as u32;
    let stack = dst + PAGE as u32;
    machine.cpu.map(src, 3 * PAGE as u32, Prot::RW).unwrap();
    machine.cpu.write(src, &[b'A'; 512]).unwrap();
    machine.cpu.set_reg(Reg::Esp, stack);
    for (i, value) in [0, 0, src, 512, dst, 512].iter().enumerate() {
        machine
            .cpu
            .write_u32(stack + 4 + i as u32 * 4, *value)
            .unwrap();
    }
    let mut env = Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        context,
        thunk: 0,
    };
    assert_eq!(nls::multi_byte_to_wide_char(&mut env), Some(512));
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    let result = nls::multi_byte_to_wide_char(&mut env);
    COUNTING.set(false);
    assert_eq!(result, Some(512));
    assert_eq!(ALLOCATIONS.get(), 0);
    let mut output = [0; 1024];
    env.cpu.read(dst, &mut output).unwrap();
    assert!(output.as_chunks::<2>().0.iter().all(|ch| *ch == [b'A', 0]));
}

// spec: NAT-BODY-6, NAT-BODY-10, NAT-BODY-19
#[test]
fn crt_and_adler_neon_match_scalar() {
    use adv_cpu::body::{GuestMem, Scratch};
    let mut cpu = guest();
    let mut scratch = Scratch::default();
    let mut seed = 0x73a1_62d9u32;
    for len in 0..=512u32 {
        for byte in &mut cpu.bytes[2 * PAGE..4 * PAGE] {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *byte = seed as u8 | 1;
        }
        for width in [1, 2] {
            let a = 2 * PAGE as u32 + len % 16;
            let b = 3 * PAGE as u32 + (len * 7) % 16;
            for i in 0..len * width {
                cpu.bytes[(b + i) as usize] = cpu.bytes[(a + i) as usize];
            }
            cpu.bytes[(a + len * width) as usize..(a + len * width + width) as usize].fill(0);
            cpu.bytes[(b + len * width) as usize..(b + len * width + width) as usize].fill(0);
            for different in [false, true] {
                if different && len != 0 {
                    cpu.bytes[(b + len / 2 * width) as usize] ^= 0x42;
                }
                let args = [a, b, len + 1, 0, 0, 0, 0];
                let mut mem = GuestMem::new(&mut cpu, &mut scratch);
                let (scalar, neon) = if width == 1 {
                    (
                        crt::compare_strings::<_, 1, true, true, false>(&mut mem, args),
                        crt::compare_strings::<_, 1, true, true, true>(&mut mem, args),
                    )
                } else {
                    (
                        crt::compare_strings::<_, 2, true, true, false>(&mut mem, args),
                        crt::compare_strings::<_, 2, true, true, true>(&mut mem, args),
                    )
                };
                assert_eq!(neon, scalar, "length {len}, width {width}");
            }
        }
        let args = [seed, 2 * PAGE as u32, len, 0, 0, 0, 0];
        let mut mem = GuestMem::new(&mut cpu, &mut scratch);
        assert_eq!(
            zlib::adler32::<_, true>(&mut mem, args),
            zlib::adler32::<_, false>(&mut mem, args)
        );
    }
}

// spec: NAT-BODY-6, NAT-BODY-11, NAT-BODY-13, NAT-BODY-17
#[test]
fn direct_bodies_preserve_aliases_without_borrowed_memory() {
    use adv_cpu::body::{GuestMem, Scratch};
    for (name, args) in [
        ("borland!memcpy", [0x5101, 0x5100, 63, 0, 0, 0, 0]),
        ("borland!memmove", [0x5101, 0x5100, 63, 0, 0, 0, 0]),
        ("msvcrt.dll!strcpy", [0x5100, 0x5101, 0, 0, 0, 0, 0]),
        (
            "windowscodecs.dll!inflate_fast",
            [0x2100, 512, 0, 0, 0, 0, 0],
        ),
    ] {
        let body = BODIES
            .iter()
            .find(|body| body.name == name)
            .unwrap()
            .direct
            .unwrap();
        let mut expected = None;
        for lend in [true, false] {
            let mut cpu = guest();
            cpu.lend = lend;
            for (i, byte) in cpu.bytes[5 * PAGE..6 * PAGE].iter_mut().enumerate() {
                *byte = i as u8;
            }
            cpu.write(0x5101, b"overlapping string\0").unwrap();
            if name.ends_with("inflate_fast") {
                for (at, value) in [
                    (0x2100, 0x5100u32),
                    (0x2104, 32),
                    (0x210c, 0x5101),
                    (0x2110, 512),
                    (0x211c, 0x3000),
                    (0x3050, 0x1100),
                    (0x3054, 0x4100),
                    (0x3058, 1),
                    (0x305c, 1),
                    (0x4bc4, 1),
                ] {
                    cpu.bytes[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
                }
                // A root table of one-bit literals keeps every overwritten
                // compressed byte decodable, exposing later live input reads.
                cpu.bytes[0x1100..0x1108].copy_from_slice(&[0, 1, b'A', 0, 0, 1, b'B', 0]);
            }
            let mut scratch = Scratch::default();
            let result = (body.full)(&mut GuestMem::new(&mut cpu, &mut scratch), args);
            assert!(result.is_ok(), "{name}: {result:?}");
            if let Some((ret, bytes)) = &expected {
                assert_eq!(&result, ret, "{name}");
                assert_eq!(&cpu.bytes, bytes, "{name}");
            } else {
                expected = Some((result, cpu.bytes));
            }
        }
    }
}

// spec: NAT-BODY-6
#[test]
fn wide_substrings_search_overlapping_unit_patterns() {
    for hay in 0..256u32 {
        let units: [u16; 8] = std::array::from_fn(|i| 0x4141 + ((hay >> i) & 1) as u16);
        let bytes: [u8; 16] = std::array::from_fn(|i| units[i / 2].to_le_bytes()[i % 2]);
        for len in 1..=8 {
            for needle in 0..1u32 << len {
                let target: [u16; 8] = std::array::from_fn(|i| 0x4141 + ((needle >> i) & 1) as u16);
                let pattern: [u8; 16] = std::array::from_fn(|i| target[i / 2].to_le_bytes()[i % 2]);
                let expected = units
                    .windows(len)
                    .position(|window| window == &target[..len])
                    .map(|i| i * 2);
                assert_eq!(
                    crt::wide_substring(&bytes, &pattern[..len * 2]),
                    expected,
                    "haystack {hay:#x}, needle {needle:#x}, length {len}"
                );
            }
        }
    }
}

// spec: NAT-BODY-6
#[test]
fn crt_right_overlap_matches_windows() {
    use adv_cpu::body::{GuestMem, Scratch};
    // Windows 11, i686: 87 lengths (0..65 plus both sides of 80/96/112/
    // 128/256/512/1024), 16 source alignments, displacements 1..32.
    // FNV64 folds length/alignment/displacement, relative EAX, LastError,
    // and FNV32 of the whole 4 KiB data page for every returning call.
    // Faults are at the entry; A64-CALL-4 permits leaving no partial writes.
    let golden = [
        ("strcpy", 10480, 0x2669460801a819c1u64),
        ("wcscpy", 44544, 0xcc44497392c08fb6),
        ("strcat", 24, 0xa4a8d98bd156332e),
        ("wcscat", 19328, 0x4a30339f44241035),
    ];
    let mut lengths: Vec<u32> = (0..=65).collect();
    lengths.extend([
        79, 80, 81, 95, 96, 97, 111, 112, 113, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023,
        1024, 1025,
    ]);
    for module in ["ntdll.dll", "msvcrt.dll", "ucrtbase.dll"] {
        for (name, expected_count, expected_hash) in golden {
            let body = bodies()
                .find(|body| body.name == format!("{module}!{name}"))
                .unwrap()
                .direct
                .unwrap();
            let width = if name.starts_with('w') { 2 } else { 1 };
            let mut cpu = guest();
            let mut scratch = Scratch::default();
            let mut count = 0;
            let mut hash = 0xcbf29ce484222325u64;
            for &len in &lengths {
                for align in 0..16 {
                    for delta in 1..=32 {
                        for (i, byte) in cpu.bytes[5 * PAGE..6 * PAGE].iter_mut().enumerate() {
                            *byte = (1 + (i * 37 + 11) % 251) as u8;
                        }
                        let src = 0x5100 + align;
                        cpu.bytes[(src + len * width) as usize..(src + (len + 1) * width) as usize]
                            .fill(0);
                        let result = (body.full)(
                            &mut GuestMem::new(&mut cpu, &mut scratch),
                            [src + delta, src, 0, 0, 0, 0, 0],
                        );
                        let Ok(result) = result else { continue };
                        count += 1;
                        let page = cpu.bytes[5 * PAGE..6 * PAGE]
                            .iter()
                            .fold(2166136261u32, |h, b| {
                                (h ^ u32::from(*b)).wrapping_mul(16777619)
                            });
                        for value in [
                            len,
                            align,
                            delta,
                            (result.value as u32).wrapping_sub(src + delta),
                            0x1234abcd,
                            page,
                        ] {
                            for byte in value.to_le_bytes() {
                                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                            }
                        }
                    }
                }
            }
            assert_eq!(
                (count, hash),
                (expected_count, expected_hash),
                "{module}!{name}"
            );
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-6
#[test]
fn crt_comparison_lengths_match_windows() {
    use adv_cpu::body::{GuestMem, Scratch};
    // Same i686 Windows sweep as crt_right_overlap_matches_windows, with
    // both operand orders and a mismatch at (displacement - 1) % length.
    // No threshold through 1025 changes sign/difference conventions.
    let mut lengths: Vec<u32> = (0..=65).collect();
    lengths.extend([
        79, 80, 81, 95, 96, 97, 111, 112, 113, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023,
        1024, 1025,
    ]);
    for module in [
        "ntdll.dll",
        "msvcrt.dll",
        "ucrtbase.dll",
        "msvcr70.dll",
        "msvcr71.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "msvcr100.dll",
        "msvcr110.dll",
        "msvcr120.dll",
        "msvcrtd.dll",
    ] {
        for (name, width) in [
            ("strcmp", 1),
            ("strncmp", 1),
            ("wcscmp", 2),
            ("wcsncmp", 2),
            ("memcmp", 1),
        ] {
            let body = bodies()
                .find(|body| body.name == format!("{module}!{name}"))
                .unwrap()
                .direct
                .unwrap();
            let difference = if name == "wcsncmp" {
                24536
            } else if name == "strncmp"
                && matches!(
                    module,
                    "ntdll.dll"
                        | "msvcrt.dll"
                        | "msvcr80.dll"
                        | "msvcr90.dll"
                        | "msvcr100.dll"
                        | "msvcrtd.dll"
                )
            {
                216
            } else {
                1
            };
            let mut cpu = guest();
            let mut scratch = Scratch::default();
            for &len in &lengths {
                for align in 0..16 {
                    for delta in 1..=32 {
                        let a = 0x2100 + align;
                        let b = 0x3100 + delta % 16;
                        for at in [a, b] {
                            cpu.bytes[at as usize..(at + len * width) as usize].fill(0x41);
                            cpu.bytes
                                [(at + len * width) as usize..(at + (len + 1) * width) as usize]
                                .fill(0);
                        }
                        if len != 0 {
                            let offset = (delta - 1) % len * width;
                            cpu.bytes[(a + offset) as usize] = 0x11;
                            cpu.bytes[(b + offset) as usize] = 0xe9;
                            if width == 2 {
                                cpu.bytes[(a + offset + 1) as usize] = 0x14;
                                cpu.bytes[(b + offset + 1) as usize] = 0x73;
                            }
                        }
                        let (a, b, sign) = if delta & 1 == 0 {
                            (a, b, -1i32)
                        } else {
                            (b, a, 1)
                        };
                        let actual = (body.full)(
                            &mut GuestMem::new(&mut cpu, &mut scratch),
                            [a, b, len, 0, 0, 0, 0],
                        )
                        .unwrap();
                        assert_eq!(
                            actual.value as u32,
                            if len == 0 {
                                0
                            } else {
                                (difference * sign) as u32
                            },
                            "{module}!{name}: length {len}, alignment {align}, displacement {delta}"
                        );
                    }
                }
            }
        }
    }
}
