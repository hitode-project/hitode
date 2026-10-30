//! Differential calls into the pinned dsound.dll, without a device or window.
use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::Pe;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};
#[path = "../src/native/mixer_layout.rs"]
mod layout;
use layout::{buffer as b, device as d, volume as v, *};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x500000;
const LEN: u32 = 0x100000;
const MIX: u32 = DATA + 0xb000;
const ALL: u32 = DATA + 0xc000;
const FRAMES: u32 = 37;

fn dist() -> Option<PathBuf> {
    std::env::var_os("ADV_WINE_DIST").map(PathBuf::from)
}
fn set(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put(data: &mut [u8], base: u32, at: usize, value: u32) {
    set(data, (base - DATA) as usize + at, value);
}
fn qword(data: &mut [u8], base: u32, at: usize, value: i64) {
    let start = (base - DATA) as usize + at;
    data[start..start + 8].copy_from_slice(&value.to_le_bytes());
}
fn addr(table: &str, pe: &Pe, name: &str) -> u32 {
    let row = table
        .lines()
        .skip(1)
        .find(|l| l.split('\t').nth(3) == Some(name))
        .unwrap();
    pe.image_base + u32::from_str_radix(row.split('\t').next().unwrap(), 16).unwrap()
}
fn map(machine: &mut Machine<Cpu>, pe: &Pe) {
    adv_nt::image::map(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        pe,
        pe.image_base,
    )
    .unwrap();
}
struct Futex(u32);
struct Services;
impl adv_machine::Resolver for Services {
    fn provides(&self, module: &str) -> bool {
        module == "ntdll.dll" || adv_machine::Resolver::provides(&Platform, module)
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<adv_machine::HostFn> {
        if module == "ntdll.dll" && name == "NtSetEvent" {
            return Some(adv_machine::HostFn {
                conv: adv_machine::Conv::Stdcall { argc: 2 },
                handler: |env| {
                    let args = [env.arg(0), env.arg(1)];
                    adv_nt::sync::set_event(env, &args)
                },
                leaf: None,
                data: 0,
                trace: false,
                guest: None,
            });
        }
        if module == "ntdll.dll" && name == "NtAlertThreadByThreadId" {
            return Some(adv_machine::HostFn {
                conv: adv_machine::Conv::Stdcall { argc: 1 },
                handler: |env| {
                    let tid = env.arg(0);
                    adv_nt::sync::alert_thread(env, &[tid])
                },
                leaf: None,
                data: 0,
                trace: false,
                guest: None,
            });
        }
        adv_machine::Resolver::host_fn(&Platform, module, name)
    }
}
fn machine(dist: &Path, pe: &Pe, image: &[u8], table: &str) -> Machine<Cpu> {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, true).unwrap(),
        Services,
        Clock::virtual_time(),
    )
    .unwrap();
    map(&mut m, pe);
    for (offset, bytes) in pe.image.windows(8).enumerate() {
        if bytes == b"\xffdsound\0" {
            m.cpu.write(pe.image_base + offset as u32, &[0]).unwrap();
        }
    }
    let ntdll =
        Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/ntdll.dll")).unwrap()).unwrap();
    map(&mut m, &ntdll);
    // futex_queues address and 12-byte queue stride from the pinned code.
    m.ext.insert(Futex(ntdll.image_base + 0xa5960));
    let ntdll_image = std::fs::read(dist.join("lib/wine/i386-windows/ntdll.dll")).unwrap();
    adv_nt::identity::note_ntdll(
        &mut m,
        &dist.join("lib/wine/i386-windows/ntdll.dll"),
        &ntdll_image,
    );
    adv_nt::identity::note_ntdll(
        &mut m,
        &dist.join("lib/wine/i386-windows/dsound.dll"),
        image,
    );
    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(DATA, LEN, Prot::RW).unwrap();
    m.cpu.map(0x7ffe0000, 0x1000, Prot::READ).unwrap();
    // Imported service boundaries stay deterministic. The SRW paths are Wine's
    // real ntdll code; event delivery is recorded in guest memory.
    let mut event = vec![0xff, 0x05];
    event.extend_from_slice(&(ALL + 4).to_le_bytes());
    event.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc2, 4, 0]);
    m.cpu.write(CODE + 0x120, &event).unwrap();
    let mut effect = vec![0xff, 0x05];
    effect.extend_from_slice(&(ALL + 8).to_le_bytes());
    effect.extend_from_slice(&[0x31, 0xc0, 0xc2, 24, 0]);
    m.cpu.write(CODE + 0x140, &effect).unwrap();
    // Deterministic guest allocators: realloc keeps an already reserved block;
    // malloc supplies the cache block. Both expose their calls in guest bytes.
    let mut realloc = vec![0xff, 0x05];
    realloc.extend_from_slice(&(ALL + 12).to_le_bytes());
    realloc.extend_from_slice(&[0x8b, 0x44, 0x24, 4, 0x85, 0xc0, 0x75, 5, 0xb8]);
    realloc.extend_from_slice(&(DATA + 0x10000).to_le_bytes());
    realloc.push(0xc3);
    m.cpu.write(CODE + 0x200, &realloc).unwrap();
    let mut malloc = vec![0xff, 0x05];
    malloc.extend_from_slice(&(ALL + 12).to_le_bytes());
    malloc.push(0xb8);
    malloc.extend_from_slice(&(DATA + 0x20000).to_le_bytes());
    malloc.push(0xc3);
    m.cpu.write(CODE + 0x240, &malloc).unwrap();
    for module in pe.imports().unwrap() {
        for import in module.imports {
            let name = import.name.to_string();
            let target = match name.as_str() {
                "AcquireSRWLockShared" | "ReleaseSRWLockShared" | "memset" => {
                    let function = if name == "memset" {
                        name.clone()
                    } else {
                        format!("Rtl{name}")
                    };
                    ntdll
                        .exports()
                        .unwrap()
                        .into_iter()
                        .find_map(|e| match (e.name, e.target) {
                            (Some(n), adv_pe::ExportTarget::Rva(r)) if n == function => {
                                let target = ntdll.image_base + r;
                                if name == "AcquireSRWLockShared" {
                                    // The fixture's exclusive owner releases at this boundary.
                                    let mut release = vec![
                                        0x8b, 0x44, 0x24, 4, 0x66, 0x83, 0x38, 0, 0x74, 6, 0xc7, 0,
                                        0, 0, 0, 0, 0xe9,
                                    ];
                                    release.extend_from_slice(
                                        &target.wrapping_sub(CODE + 0x160 + 21).to_le_bytes(),
                                    );
                                    m.cpu.write(CODE + 0x160, &release).unwrap();
                                    Some(CODE + 0x160)
                                } else {
                                    Some(target)
                                }
                            }
                            _ => None,
                        })
                        .unwrap()
                }
                "SetEvent" => CODE + 0x120,
                "realloc" => CODE + 0x200,
                "malloc" => CODE + 0x240,
                _ => CODE + 0x180,
            };
            m.cpu
                .write_u32(pe.image_base + import.iat_rva, target)
                .unwrap();
        }
    }
    // An unexpected import faults, instead of silently succeeding.
    m.cpu.write(CODE + 0x180, &[0x0f, 0x0b]).unwrap();
    let entry = addr(table, pe, "DSOUND_MixToPrimary");
    // Capture both control domains in the actual guest call, as Wine does.
    let mut code = vec![0xd9, 0x2d];
    code.extend_from_slice(&(CODE + 0x300).to_le_bytes());
    code.extend_from_slice(&[0x0f, 0xae, 0x15]);
    code.extend_from_slice(&(CODE + 0x304).to_le_bytes());
    code.push(0x68);
    code.extend_from_slice(&ALL.to_le_bytes());
    for (opcode, value) in [(0xb8, DATA), (0xba, MIX), (0xb9, FRAMES)] {
        code.push(opcode);
        code.extend_from_slice(&value.to_le_bytes());
    }
    code.push(0xe8);
    let next = CODE + code.len() as u32 + 4;
    code.extend_from_slice(&entry.wrapping_sub(next).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, 4, 0xc3]);
    m.cpu.write(CODE, &code).unwrap();
    m
}
fn fixture(pe: &Pe, table: &str, seed: u32) -> Vec<u8> {
    let mut data = vec![0; LEN as usize];
    let channels = 1 + seed % 2;
    put(&mut data, DATA, d::pwfx, DATA + 0x200);
    put(&mut data, DATA, d::nrofbuffers, 3);
    put(&mut data, DATA, d::buffers, DATA + 0x300);
    put(&mut data, DATA, d::tmp_buffer, DATA + 0x4000);
    put(&mut data, DATA, d::tmp_buffer_len, 0x1000);
    put(&mut data, DATA, d::cp_buffer, DATA + 0x5000);
    put(&mut data, DATA, d::cp_buffer_len, 0x5000);
    data[0x202..0x204].copy_from_slice(&(channels as u16).to_le_bytes());
    put(&mut data, DATA + 0x200, 0, 3 | (channels << 16));
    put(&mut data, DATA + 0x200, 4, 48000);
    put(&mut data, DATA + 0x200, 8, 48000 * channels * 4);
    put(&mut data, DATA + 0x200, 12, (channels * 4) | (32 << 16));
    let mut rng = seed.wrapping_add(1);
    for i in 0..3 {
        let base = DATA + 0x1000 + i * 0x1000;
        put(&mut data, DATA + 0x300, i as usize * 4, base);
        let inputs = 1 + (seed / 2 + i) % 2;
        let kind = [1, 2, 4][((seed / 4 + i) % 3) as usize];
        let freq = if (seed + i).is_multiple_of(3) {
            48000
        } else {
            4000 + (seed * 7919 + i * 33331) % 188001
        };
        let step = if freq > 48000 {
            120 * 48000 / freq
        } else {
            120
        };
        let field = |data: &mut [u8], offset, value| put(data, base, offset, value);
        field(&mut data, b::device, DATA);
        field(&mut data, b::lock, (seed % 3) << 16);
        field(&mut data, b::pwfx, base + 0x400);
        field(&mut data, b::buffer, base + 0x500);
        field(
            &mut data,
            b::state,
            if seed.is_multiple_of(17) {
                0
            } else {
                1 + (seed + i) % 3
            },
        );
        field(&mut data, b::playflags, (seed + i) % 2);
        field(&mut data, b::buflen, 128 * inputs * kind);
        field(
            &mut data,
            b::sec_mixpos,
            ((seed + i * 29) % 128) * inputs * kind,
        );
        field(&mut data, 0x4c, freq); // freq
        field(&mut data, b::firstep, step);
        field(&mut data, b::firgain, (step as f32 / 120.0).to_bits());
        field(&mut data, b::dsbd + desc::dwFlags, (seed % 4) * 0x40);
        field(
            &mut data,
            b::volpan + v::lVolume,
            if seed.is_multiple_of(3) {
                0
            } else {
                (-1000i32) as u32
            },
        );
        field(&mut data, b::volpan + v::lPan, seed % 5);
        for c in 0..channels {
            field(
                &mut data,
                b::volpan + v::dwTotalAmpFactor + c as usize * 4,
                if seed.is_multiple_of(13) {
                    0
                } else {
                    10000 + (seed * 7919 + c * 333) % 55536
                },
            );
        }
        qword(&mut data, base, b::freqAdjustNum, i64::from(freq));
        qword(&mut data, base, b::freqAdjustDen, 48000);
        qword(
            &mut data,
            base,
            b::freqAccNum,
            i64::from(seed * 1777 % 48000),
        );
        let getter = addr(
            table,
            pe,
            match kind {
                1 => "get8",
                2 => "get16",
                _ => "getieee32",
            },
        );
        field(
            &mut data,
            b::get,
            if inputs > channels {
                addr(table, pe, "get_mono")
            } else {
                getter
            },
        );
        field(&mut data, b::get_aux, getter);
        field(
            &mut data,
            b::put,
            addr(
                table,
                pe,
                if inputs < channels {
                    "put_mono2stereo"
                } else {
                    "putieee32"
                },
            ),
        );
        field(&mut data, b::put_aux, addr(table, pe, "putieee32"));
        field(&mut data, b::mix_channels, inputs.min(channels));
        field(&mut data, b::writelead, 16 * inputs * kind);
        field(&mut data, b::use_committed, seed % 2);
        field(&mut data, b::committedbuff, base + 0xc00);
        field(&mut data, b::committed_mixpos, 3 * inputs * kind);
        put(&mut data, base + 0x500, memory::memory, base + 0x600);
        let start = (base + 0x400 - DATA) as usize;
        data[start..start + 2].copy_from_slice(&(if kind == 4 { 3u16 } else { 1 }).to_le_bytes());
        data[start + wave::nChannels..start + wave::nChannels + 2]
            .copy_from_slice(&(inputs as u16).to_le_bytes());
        data[start + wave::nBlockAlign..start + wave::nBlockAlign + 2]
            .copy_from_slice(&((inputs * kind) as u16).to_le_bytes());
        put(&mut data, base + 0x400, 4, freq);
        put(&mut data, base + 0x400, 8, freq * inputs * kind);
        data[start + 14..start + 16].copy_from_slice(&((kind * 8) as u16).to_le_bytes());
        for offset in (0x600..0x1000).step_by(4) {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let value = if kind == 4 {
                ((rng as i32) as f32 / 2147483648.0).to_bits()
            } else {
                rng
            };
            put(&mut data, base, offset, value);
        }
    }
    for offset in (0x4000..0xc000).step_by(4) {
        set(&mut data, offset, 0.125f32.to_bits());
    }
    data
}
fn call(m: &mut Machine<Cpu>, data: &[u8]) -> (Result<(), String>, Vec<u8>) {
    call_controls(m, data, 0x027f, 0x1f80)
}
fn call_controls(
    m: &mut Machine<Cpu>,
    data: &[u8],
    control: u16,
    mxcsr: u32,
) -> (Result<(), String>, Vec<u8>) {
    m.cpu.write(CODE + 0x300, &control.to_le_bytes()).unwrap();
    m.cpu.write_u32(CODE + 0x304, mxcsr).unwrap();
    m.cpu.write(DATA, data).unwrap();
    m.spawn("mixer", CODE, &[], 0x10000).unwrap();
    let result = match m.run_slice(std::time::Duration::from_secs(10)) {
        adv_machine::SliceResult::Complete(result) => match result {
            RunResult::Exited(_) => Ok(()),
            RunResult::Failed(error) => Err(format!("{:#x} {}", error.pc, error.message)),
            other => Err(format!("{other:?}")),
        },
        adv_machine::SliceResult::Yielded => Err("mixer call timed out".into()),
    };
    let mut output = vec![0; LEN as usize];
    m.cpu.read(DATA, &mut output).unwrap();
    (result, output)
}

// spec: NAT-BODY-3
#[test]
fn mixer_matches_wine() {
    let Some(dist) = dist() else {
        return;
    };
    let image = std::fs::read(dist.join("lib/wine/i386-windows/dsound.dll")).unwrap();
    let pe = Pe::parse(&image).unwrap();
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/dsound.dll.tsv")).unwrap();
    let mut native = machine(&dist, &pe, &image, &table);
    let mut reference = machine(&dist, &pe, &image, &table);
    assert!(!native.sys.redirects.is_empty());
    reference.sys.redirects.clear();
    for seed in 0..84 {
        let data = fixture(&pe, &table, seed);
        let actual = call(&mut native, &data);
        let expected = call(&mut reference, &data);
        assert_eq!(actual.0, Ok(()), "seed {seed}: {:?}", actual.0);
        assert_eq!(expected.0, Ok(()), "seed {seed}: {:?}", expected.0);
        if let Some(at) = actual.1.iter().zip(&expected.1).position(|(a, b)| a != b) {
            panic!(
                "seed {seed}, offset {at:#x}: native {:?}, Wine {:?}",
                &actual.1[at / 4 * 4..at / 4 * 4 + 4],
                &expected.1[at / 4 * 4..at / 4 * 4 + 4]
            );
        }
    }
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
    assert!(native.sys.host_calls > 60);
    assert_eq!(reference.sys.host_calls, 0);
    // Every get/put mapping, equal/unequal counts (including >32), all
    // sample encodings, up/down/no resampling, committed and 3D paths.
    for inputs in [0, 1, 2, 3, 4, 5, 6, 7, 8, 16, 32, 33] {
        for outputs in [0, 1, 2, 3, 4, 5, 6, 7, 8, 16, 32, 33] {
            for kind in 0..5 {
                for ratio in 0..4 {
                    let data = classes(&pe, &table, inputs, outputs, kind, ratio);
                    compare(
                        &mut native,
                        &mut reference,
                        &data,
                        0x027f,
                        0x1f80,
                        &format!("in {inputs} out {outputs} format {kind} ratio {ratio}"),
                    );
                }
            }
        }
    }
    // Guest rounding, precision, DAZ/FTZ, zeros, infinities, NaN payloads,
    // and denormals. Controls remain owned by the guest across the body.
    for sse_rc in 0..4 {
        for x87_rc in 0..4 {
            for precision in [0, 2, 3] {
                for denormals in [0, 0x40, 0x8000, 0x8040] {
                    for ratio in [0, 1] {
                        let mut data = classes(&pe, &table, 6, 2, 4, ratio);
                        for (i, bits) in [
                            0, 0x80000000, 1, 0x807fffff, 0x7f800000, 0xff800000, 0x7fc12345,
                            0xffc54321, 0x7f812345, 0xff854321, 0x3eaaaaab,
                        ]
                        .iter()
                        .cycle()
                        .take(73 * 6)
                        .enumerate()
                        {
                            set(&mut data, 0xa0000 + i * 4, *bits);
                        }
                        let cw = 0x7f | (x87_rc << 10) | (precision << 8);
                        let mxcsr = 0x1f80 | (sse_rc << 13) | denormals;
                        compare(
                            &mut native,
                            &mut reference,
                            &data,
                            cw,
                            mxcsr,
                            &format!("FP {cw:x} {mxcsr:x}, ratio {ratio}"),
                        );
                    }
                }
            }
        }
    }
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
    // Each waiting buffer is last: a decline must leave earlier buffers untouched.
    for case in [0, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13] {
        let mut native = machine(&dist, &pe, &image, &table);
        let mut reference = machine(&dist, &pe, &image, &table);
        reference.sys.redirects.clear();
        for _ in 0..3 {
            let warm = fixture(&pe, &table, 5);
            assert!(call(&mut native, &warm).0.is_ok());
            assert!(call(&mut reference, &warm).0.is_ok());
        }
        let mut data = fixture(&pe, &table, 5);
        let last = DATA + 0x3000;
        match case {
            0 => put(&mut data, last, b::lock, 1),
            1 | 8 => {
                put(&mut data, last, b::dsbd + desc::dwFlags, 0x100);
                put(&mut data, last, b::notifies, last + 0x300);
                put(&mut data, last, b::nrofnotifies, 1);
                put(
                    &mut data,
                    last + 0x300,
                    notify::dwOffset,
                    if case == 1 || case == 10 { 1 } else { 100 },
                );
                put(&mut data, last, b::sec_mixpos, 0);
                put(&mut data, last, b::playflags, 1);
                qword(&mut data, last, b::freqAdjustNum, 48000);
            }
            2 => put(&mut data, last, b::dsbd + desc::dwFlags, 0x10),
            3 => {
                put(&mut data, last, b::num_filters, 1);
                put(&mut data, last, b::filters, last + 0x300); // filters
                // DSFilter.inplace, then the object's vtable and Process slot.
                put(&mut data, last + 0x300, filter::inplace, last + 0x340);
                put(&mut data, last + 0x340, 0, last + 0x360);
                put(&mut data, last + 0x360, 12, CODE + 0x140);
            }
            4 => put(&mut data, last, b::get, addr(&table, &pe, "get24")),
            5 => put(&mut data, last, b::put, addr(&table, &pe, "putieee32_sum")),
            6 => put(&mut data, last + 0x500, memory::memory, DATA + LEN - 8),
            7 => {
                native.cpu.protect(MIX, 0x1000, Prot::READ).unwrap();
                reference.cpu.protect(MIX, 0x1000, Prot::READ).unwrap();
            }
            9 => put(&mut data, DATA, d::tmp_buffer_len, 4),
            10 => {
                put(&mut data, last, b::dsbd + desc::dwFlags, 0x100);
                put(&mut data, last, b::notifies, last + 0x300);
                put(&mut data, last, b::nrofnotifies, 1);
                put(&mut data, last + 0x300, notify::dwOffset, u32::MAX);
                put(&mut data, last, b::sec_mixpos, 120);
                put(&mut data, last, b::playflags, 0);
                qword(&mut data, last, b::freqAdjustNum, 48000);
            }
            11 => put(&mut data, last, b::lock, 0x10002),
            13 => put(&mut data, last, b::lock, 0xffff0000),
            12 => {
                put(&mut data, DATA, d::tmp_buffer, MIX);
                put(&mut data, DATA, d::tmp_buffer_len, FRAMES * 2 * 4);
            }
            _ => unreachable!(),
        }
        let before = adv_nt::native::declines(&native.ext);
        let actual = call(&mut native, &data);
        let expected = call(&mut reference, &data);
        assert_eq!(
            actual.0.is_err(),
            expected.0.is_err(),
            "case {case}: {:?}, {:?}",
            actual.0,
            expected.0
        );
        assert_eq!(actual.0.is_err(), matches!(case, 6 | 7), "decline {case}");
        if let Some(at) = actual.1.iter().zip(&expected.1).position(|(a, b)| a != b) {
            panic!(
                "case {case} changed memory at {at:#x}: {:?}/{:?}",
                &actual.1[at / 4 * 4..at / 4 * 4 + 4],
                &expected.1[at / 4 * 4..at / 4 * 4 + 4]
            );
        }
        assert_eq!(
            adv_nt::native::declines(&native.ext) - before,
            u64::from(matches!(case, 0 | 3 | 11)),
            "decline {case}"
        );
        if case == 1 || case == 10 {
            assert_eq!(native.cpu.read_u32(ALL + 4).unwrap(), 1);
        }
        if case == 3 {
            assert_eq!(native.cpu.read_u32(ALL + 8).unwrap(), 1);
        }
        if case == 7 {
            native.cpu.protect(MIX, 0x1000, Prot::RW).unwrap();
            reference.cpu.protect(MIX, 0x1000, Prot::RW).unwrap();
        }
    }
}

// spec: NAT-BODY-3
#[test]
fn mixer_layout_matches_dwarf() {
    let Some(dist) = dist() else {
        return;
    };
    let dll = dist
        .with_file_name("wine11-sse-dist")
        .join("usr/local/lib/wine/i386-windows/dsound.dll");
    let dump = std::process::Command::new("dwarfdump")
        .arg("--debug-info")
        .arg(&dll)
        .output()
        .unwrap();
    assert!(dump.status.success());
    let text = String::from_utf8(dump.stdout).unwrap();
    let source = include_str!("../src/native/mixer_layout.rs");
    for (module, name) in [
        ("device", "DirectSoundDevice"),
        ("buffer", "IDirectSoundBufferImpl"),
        ("filter", "DSFilter"),
        ("wave", "tWAVEFORMATEX"),
        ("volume", "_DSVOLUMEPAN"),
        ("desc", "_DSBUFFERDESC"),
        ("memory", "BufferMemory"),
        ("notify", "_DSBPOSITIONNOTIFY"),
    ] {
        let entries: Vec<_> = text.split("\n0x").collect();
        let start = entries
            .iter()
            .position(|e| {
                e.contains(":   DW_TAG_structure_type")
                    && e.contains(&format!("DW_AT_name\t(\"{name}\")"))
            })
            .unwrap();
        if module == "filter" {
            assert!(entries[start].contains("DW_AT_byte_size\t(0x18)"));
        }
        let members: Vec<_> = entries[start + 1..]
            .iter()
            .take_while(|e| !e.contains(":     NULL"))
            .copied()
            .collect();
        let constants = source
            .split(&format!("pub mod {module} {{"))
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        for line in constants.lines().filter(|l| l.contains("pub const")) {
            let field = line
                .split("pub const ")
                .nth(1)
                .unwrap()
                .split(':')
                .next()
                .unwrap();
            let value = line.split("= ").nth(1).unwrap().trim_end_matches(';');
            let offset = if let Some(hex) = value.strip_prefix("0x") {
                usize::from_str_radix(hex, 16).unwrap()
            } else {
                value.parse().unwrap()
            };
            let entry = members
                .iter()
                .find(|e| e.contains(&format!("DW_AT_name\t(\"{field}\")")))
                .unwrap();
            let actual = entry
                .split("DW_AT_data_member_location\t(0x")
                .nth(1)
                .unwrap()
                .split(')')
                .next()
                .unwrap();
            assert_eq!(
                offset,
                usize::from_str_radix(actual, 16).unwrap(),
                "{name}.{field}"
            );
        }
    }
    let fir = text
        .split("DW_AT_name\t(\"fir\")")
        .nth(1)
        .unwrap()
        .split("\n0x")
        .next()
        .unwrap();
    let address = fir
        .split("DW_OP_addr 0x")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    let pe = Pe::parse(&std::fs::read(dll).unwrap()).unwrap();
    assert_eq!(
        FIR_RVA,
        u32::from_str_radix(address, 16).unwrap() - pe.image_base
    );
    assert!(fir.contains(&format!("float[{FIR_LEN}]")));
    let function = text
        .split("DW_AT_name\t(\"DSOUND_MixToPrimary\")")
        .nth(1)
        .unwrap();
    for (parameter, register) in function
        .split("DW_TAG_formal_parameter")
        .skip(1)
        .take(4)
        .zip([
            "DW_OP_reg0 EAX",
            "DW_OP_reg2 EDX",
            "DW_OP_reg1 ECX",
            "DW_OP_fbreg +0",
        ])
    {
        assert!(parameter.contains(register));
    }
}

fn compare(
    native: &mut Machine<Cpu>,
    reference: &mut Machine<Cpu>,
    data: &[u8],
    cw: u16,
    mxcsr: u32,
    label: &str,
) {
    let actual = call_controls(native, data, cw, mxcsr);
    let expected = call_controls(reference, data, cw, mxcsr);
    assert_eq!(actual.0, Ok(()), "{label}: {:?}", actual.0);
    assert_eq!(expected.0, Ok(()), "{label}: {:?}", expected.0);
    if let Some(at) = actual.1.iter().zip(&expected.1).position(|(a, b)| a != b) {
        panic!(
            "{label} offset {at:#x}: native {:02x?}, Wine {:02x?}",
            &actual.1[at / 4 * 4..at / 4 * 4 + 4],
            &expected.1[at / 4 * 4..at / 4 * 4 + 4]
        );
    }
    let mut queues = [[0u8; 256 * 12]; 2];
    for (m, bytes) in [&mut *native, &mut *reference].into_iter().zip(&mut queues) {
        m.cpu.read(m.ext.get::<Futex>().0, bytes).unwrap();
    }
    assert_eq!(queues[0], queues[1], "{label}: shared-lock wake queues");
    let fp = native.cpu.capture_architecture().unwrap();
    assert_eq!(fp.mxcsr & !0x3f, mxcsr & !0x3f, "{label}");
    assert_eq!(fp.x87.control, cw, "{label}");
}
fn classes(pe: &Pe, table: &str, inputs: u32, outputs: u32, kind: usize, ratio: u32) -> Vec<u8> {
    let mut data = fixture(pe, table, 1);
    let buffer = DATA + 0x1000;
    let bytes = [1, 2, 3, 4, 4][kind];
    let (mix_channels, putter) = if inputs == outputs {
        (inputs.min(32), "putieee32")
    } else if outputs == 1 {
        (1, "putieee32")
    } else {
        match (inputs, outputs) {
            (1, 2) => (1, "put_mono2stereo"),
            (1, 4) => (1, "put_mono2quad"),
            (1, 6) => (1, "put_mono2surround51"),
            (2, 4) => (2, "put_stereo2quad"),
            (2, 6) => (2, "put_stereo2surround51"),
            (4, 2) => (4, "put_quad2stereo"),
            (6, 2) => (6, "put_surround512stereo"),
            (8, 2) => (8, "put_surround712stereo"),
            (1, _) => (1, "putieee32"),
            _ => (2, "putieee32"),
        }
    };
    put(&mut data, DATA, d::nrofbuffers, 1);
    put(&mut data, DATA, d::tmp_buffer, DATA + 0x10000);
    put(&mut data, DATA, d::tmp_buffer_len, 0x10000);
    put(&mut data, DATA, d::cp_buffer, DATA + 0x20000);
    put(&mut data, DATA, d::cp_buffer_len, 0x70000);
    data[0x202..0x204].copy_from_slice(&(outputs as u16).to_le_bytes());
    let fmt = (buffer + 0x400 - DATA) as usize;
    data[fmt..fmt + 2].copy_from_slice(&(if kind == 4 { 3u16 } else { 1 }).to_le_bytes());
    data[fmt + 2..fmt + 4].copy_from_slice(&(inputs as u16).to_le_bytes());
    data[fmt + 12..fmt + 14].copy_from_slice(&((inputs * bytes) as u16).to_le_bytes());
    data[fmt + 14..fmt + 16].copy_from_slice(&((bytes * 8) as u16).to_le_bytes());
    let getter = addr(
        table,
        pe,
        ["get8", "get16", "get24", "get32", "getieee32"][kind],
    );
    put(
        &mut data,
        buffer,
        b::get,
        if outputs == 1 && inputs != 1 {
            addr(table, pe, "get_mono")
        } else {
            getter
        },
    );
    put(&mut data, buffer, b::get_aux, getter);
    put(&mut data, buffer, b::put, addr(table, pe, putter));
    put(
        &mut data,
        buffer,
        b::put_aux,
        addr(
            table,
            pe,
            if matches!((inputs, outputs), (4 | 6 | 8, 2)) {
                "putieee32_sum"
            } else {
                "putieee32"
            },
        ),
    );
    put(&mut data, buffer, b::mix_channels, mix_channels);
    put(&mut data, buffer, b::buflen, 73 * inputs * bytes);
    put(&mut data, buffer, b::sec_mixpos, 5 * inputs * bytes);
    put(&mut data, buffer, b::committed_mixpos, 3 * inputs * bytes);
    put(&mut data, buffer, b::committedbuff, DATA + 0xa0000);
    put(&mut data, buffer, b::writelead, 16 * inputs * bytes);
    put(&mut data, buffer, b::dsbd + desc::dwFlags, 0x10);
    put(&mut data, buffer, b::playflags, ratio & 1);
    put(&mut data, buffer + 0x500, memory::memory, DATA + 0xa0000);
    for c in 0..6 {
        put(
            &mut data,
            buffer,
            b::volpan + c * 4,
            60000 - c as u32 * 7654,
        );
    }
    let num = [48000, 4000, 192000, 1536000][ratio as usize];
    let step = if num > 48000 { 120 * 48000 / num } else { 120 };
    qword(&mut data, buffer, b::freqAdjustNum, num as i64);
    qword(&mut data, buffer, b::freqAdjustDen, 48000);
    qword(&mut data, buffer, b::freqAccNum, 23999);
    put(&mut data, buffer, b::firstep, step);
    put(
        &mut data,
        buffer,
        b::firgain,
        (step as f32 / 120.0).to_bits(),
    );
    for (i, at) in (0xa0000..0xb0000).step_by(4).enumerate() {
        let v = (i as u32).wrapping_mul(0xabcdef13).wrapping_add(0x12345678);
        set(
            &mut data,
            at,
            if kind == 4 {
                (v as i32 as f32 / 2147483648.0).to_bits()
            } else {
                v
            },
        );
    }
    data
}

#[cfg(any(target_vendor = "apple", target_os = "android"))]
fn notifications(dist: &Path, pe: &Pe, image: &[u8], table: &str) {
    use adv_machine::{Env, Outcome};
    use adv_nt::server::{Connection, Link, Progress, Time};
    use adv_wineserver::{Config, Server, State};
    use std::{cell::RefCell, rc::Rc};
    struct Clock;
    impl adv_wineserver::Clock for Clock {
        fn monotonic(&self) -> u64 {
            0
        }
        fn unix(&self) -> u64 {
            17_672_256_000_000_000
        }
    }
    #[derive(Clone)]
    struct Runtime(Rc<RefCell<Server>>);
    impl Progress for Runtime {
        fn run_server(&mut self, _: Time) {
            assert!(matches!(
                self.0.borrow_mut().step(&Clock),
                State::Waiting(_)
            ));
        }
    }
    let root = std::env::temp_dir().join(format!("adv-mixer-notifications-{}", std::process::id()));
    std::fs::create_dir_all(root.join("prefix")).unwrap();
    let server = Server::start(
        &Config {
            prefix: root.join("prefix"),
            server_dir: root.join("server"),
            install: dist.to_owned(),
            debug: 0,
        },
        &Clock,
    )
    .unwrap();
    let socket = server.socket_dir().unwrap();
    let runtime = Runtime(Rc::new(RefCell::new(server)));
    let mut machines = [
        machine(dist, pe, image, table),
        machine(dist, pe, image, table),
    ];
    machines[1].sys.redirects.clear();
    let nt_image =
        Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/ntdll.dll")).unwrap()).unwrap();
    let export = |name: &str| {
        nt_image
            .exports()
            .unwrap()
            .into_iter()
            .find_map(|e| match (e.name, e.target) {
                (Some(n), adv_pe::ExportTarget::Rva(r)) if n == name => {
                    Some(nt_image.image_base + r)
                }
                _ => None,
            })
            .unwrap()
    };
    let mut outputs = Vec::new();
    for m in &mut machines {
        let mut data = classes(pe, table, 2, 2, 4, 0);
        put(&mut data, DATA, d::nrofbuffers, 3);
        let context = m.spawn("mixer notifications", CODE, &[], 0x10000).unwrap();
        let mut progress = runtime.clone();
        let (connection, first) =
            Connection::connect(&socket, &mut progress, Time::default()).unwrap();
        let link = Link::new(
            connection,
            Box::new(progress),
            Time::default(),
            context,
            first.tid,
            &mut m.sys,
        );
        m.ext.insert(link);
        m.sys.contexts[context as usize].tid = first.tid;
        let teb = m.sys.contexts[context as usize].teb;
        m.cpu.write_u32(teb + 0x34, 0x1234abcd).unwrap();
        m.cpu.write_u32(teb + 0xbf4, 0x1234abcd).unwrap();
        let alert = m
            .sys
            .thunks
            .publish(&mut m.cpu, "ntdll.dll", "NtAlertThreadByThreadId")
            .unwrap();
        let target = export("NtAlertThreadByThreadId");
        let mut jump = vec![0xe9];
        jump.extend_from_slice(&alert.wrapping_sub(target + 5).to_le_bytes());
        m.cpu.write(target, &jump).unwrap();
        let address = DATA + 0x1000 + b::lock as u32 + 2;
        let queue = m.ext.get::<Futex>().0 + ((address >> 4) & 255) * 12;
        for at in [queue, queue + 4] {
            m.cpu.write_u32(at, DATA + 0xe100).unwrap();
        }
        for (offset, value) in [(0, queue), (4, queue), (8, address), (12, first.tid)] {
            put(&mut data, DATA + 0xe100, offset, value);
        }
        m.cpu.write_u32(CODE + 0x300, 0x027f).unwrap();
        m.cpu.write_u32(CODE + 0x304, 0x1f80).unwrap();
        let thunk = m
            .sys
            .thunks
            .publish(&mut m.cpu, "ntdll.dll", "NtSetEvent")
            .unwrap();
        let mut event = vec![0x8b, 0x44, 0x24, 4, 0x6a, 0, 0x50, 0xb8];
        event.extend_from_slice(&thunk.to_le_bytes());
        event.extend_from_slice(&[
            0xff, 0xd0, 0x85, 0xc0, 0x78, 8, 0xb8, 1, 0, 0, 0, 0xc2, 4, 0, 0x50, 0xb8,
        ]);
        event.extend_from_slice(
            &export("RtlSetLastWin32ErrorAndNtStatusFromNtStatus").to_le_bytes(),
        );
        event.extend_from_slice(&[0xff, 0xd0, 0x31, 0xc0, 0xc2, 4, 0]);
        m.cpu.write(CODE + 0x120, &event).unwrap();
        let mut handles = Vec::new();
        for index in 0..3 {
            let buffer = DATA + 0x1000 + index * 0x1000;
            let template: Vec<_> = data[0x1000..0x1140].to_vec();
            data[(buffer - DATA) as usize..(buffer - DATA) as usize + 0x140]
                .copy_from_slice(&template);
            put(&mut data, DATA + 0x300, index as usize * 4, buffer);
            put(&mut data, buffer, b::dsbd + desc::dwFlags, 0x110);
            put(&mut data, buffer, b::lock, 0);
            put(
                &mut data,
                buffer,
                b::sec_mixpos,
                if index < 2 { 70 * 8 } else { 0 },
            );
            put(&mut data, buffer, b::playflags, u32::from(index != 0));
            put(&mut data, buffer, b::notifies, buffer + 0x200);
            put(&mut data, buffer, b::nrofnotifies, 10);
            for (i, offset) in [u32::MAX, u32::MAX, 0, 8, 8, 16, 32, 64, 280, 288]
                .into_iter()
                .enumerate()
            {
                let mut env = Env {
                    cpu: &mut m.cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    context,
                    thunk: 0,
                };
                assert!(matches!(
                    adv_nt::sync::create_event(&mut env, &[ALL + 16, 0x1f0003, 0, 0, 0]),
                    Outcome::Return(0)
                ));
                let handle = env.cpu.read_u32(ALL + 16).unwrap();
                put(&mut data, buffer + 0x200, i * 8, offset);
                put(
                    &mut data,
                    buffer + 0x200,
                    i * 8 + 4,
                    if index == 2 && i == 4 { 0 } else { handle },
                );
                handles.push(handle);
            }
        }
        m.cpu.write(DATA, &data).unwrap();
        assert!(matches!(
            m.run_slice(std::time::Duration::from_secs(10)),
            adv_machine::SliceResult::Complete(RunResult::Exited(_))
        ));
        let mut memory = vec![0; LEN as usize];
        m.cpu.read(DATA, &mut memory).unwrap();
        assert_eq!(m.cpu.read_u32(teb + 0x34).unwrap(), 6);
        assert_eq!(m.cpu.read_u32(teb + 0xbf4).unwrap(), 0xc0000008);
        assert_eq!(m.cpu.read_u32(queue).unwrap(), queue);
        assert_eq!(m.cpu.read_u32(DATA + 0xe108).unwrap(), 0);
        // The server assigns a different TID to each reference process.
        memory[0xe10c..0xe110].fill(0);
        let mut states = Vec::new();
        for handle in handles {
            let mut env = Env {
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                context,
                thunk: 0,
            };
            assert!(matches!(
                adv_nt::sync::query_event(&mut env, &[handle, 0, ALL + 16, 8, 0]),
                Outcome::Return(0)
            ));
            states.push(env.cpu.read_u32(ALL + 20).unwrap());
        }
        outputs.push((memory, states));
    }
    assert!(
        outputs[0].0 == outputs[1].0,
        "notification calls changed mixer memory"
    );
    assert_eq!(outputs[0].1, outputs[1].1);
    assert!(outputs[0].1.iter().any(|&state| state != 0));
    assert_eq!(adv_nt::native::declines(&machines[0].ext), 0);
    drop(machines);
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

// spec: NAT-BODY-3, A64-CALL-6
#[cfg(any(target_vendor = "apple", target_os = "android"))]
#[test]
fn mixer_notifications_use_machine_services() {
    let Some(dist) = dist() else { return };
    let image = std::fs::read(dist.join("lib/wine/i386-windows/dsound.dll")).unwrap();
    let pe = Pe::parse(&image).unwrap();
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/dsound.dll.tsv")).unwrap();
    notifications(&dist, &pe, &image, &table);
}

// spec: NAT-BODY-3, A64-CALL-4
#[test]
fn mixer_unmasked_floating_faults_raise_at_entry() {
    let Some(dist) = dist() else { return };
    let image = std::fs::read(dist.join("lib/wine/i386-windows/dsound.dll")).unwrap();
    let pe = Pe::parse(&image).unwrap();
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/dsound.dll.tsv")).unwrap();
    for (sample, destination, factor, cw, mxcsr, vector) in [
        (0x7f812345, 0, 60000, 0x027e, 0x1f80, 16),
        (1, 0, 60000, 0x027d, 0x1f80, 16),
        (0x7f800000, 0xff800000, 60000, 0x027f, 0x1f00, 19),
        (0x7f7fffff, 0, u32::MAX, 0x027f, 0x1b80, 19),
        (0x3eaaaaab, 0, 60000, 0x027f, 0x0f80, 19),
    ] {
        let mut native = machine(&dist, &pe, &image, &table);
        let mut reference = machine(&dist, &pe, &image, &table);
        reference.sys.redirects.clear();
        let mut data = classes(&pe, &table, 1, 1, 4, 0);
        for at in (0xa0000..0xb0000).step_by(4) {
            set(&mut data, at, sample);
        }
        for at in (0xb000..0xc000).step_by(4) {
            set(&mut data, at, destination);
        }
        put(&mut data, DATA + 0x1000, b::volpan, factor);
        let actual = call_controls(&mut native, &data, cw, mxcsr);
        let expected = call_controls(&mut reference, &data, cw, mxcsr);
        assert!(
            actual.0.is_err() && expected.0.is_err(),
            "CW {cw:x}, MXCSR {mxcsr:x}: {:?}, {:?}",
            actual.0,
            expected.0
        );
        let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
        assert_eq!(fault.kind, adv_cpu::backend::FaultKind::Trap(vector));
        assert_eq!(fault.pc, addr(&table, &pe, "DSOUND_MixToPrimary"));
        assert!(
            actual.1 == expected.1,
            "floating fault changed mixer memory: CW {cw:x}, MXCSR {mxcsr:x}"
        );
        assert_eq!(adv_nt::native::declines(&native.ext), 0);
    }
}

// spec: NAT-BODY-3, NAT-BODY-17
#[test]
fn mixer_preserves_live_aliases_and_helper_hooks() {
    let Some(dist) = dist() else { return };
    let image = std::fs::read(dist.join("lib/wine/i386-windows/dsound.dll")).unwrap();
    let pe = Pe::parse(&image).unwrap();
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/dsound.dll.tsv")).unwrap();
    let mut native = machine(&dist, &pe, &image, &table);
    let mut reference = machine(&dist, &pe, &image, &table);
    reference.sys.redirects.clear();
    for ratio in [0, 1] {
        for case in 0..10 {
            let mut data = classes(&pe, &table, 2, 2, 4, ratio);
            let buffer = DATA + 0x1000;
            match case {
                0 => put(&mut data, DATA, d::tmp_buffer, MIX),
                1 => put(&mut data, DATA, d::tmp_buffer, MIX - 4),
                2 => put(&mut data, DATA, d::tmp_buffer, MIX + 4),
                3 => put(&mut data, buffer + 0x500, memory::memory, MIX),
                4 => {
                    put(&mut data, buffer + 0x500, memory::memory, DATA + 0x10004);
                    put(&mut data, buffer, b::use_committed, 0);
                }
                5 => put(&mut data, DATA, d::cp_buffer, MIX),
                6 => put(&mut data, DATA, d::cp_buffer, DATA + 0x10000),
                7 => {
                    for c in 0..2 {
                        put(&mut data, buffer, b::volpan + c * 4, 0);
                    }
                    put(&mut data, buffer + 0x500, memory::memory, DATA + LEN);
                    put(&mut data, buffer, b::committedbuff, DATA + LEN);
                }
                8 => {
                    put(&mut data, buffer, b::num_filters, 1);
                    put(&mut data, buffer, 0x134, buffer + 0x200);
                }
                9 => {
                    put(&mut data, DATA, d::tmp_buffer, 0);
                    put(&mut data, DATA, d::cp_buffer, 0);
                }
                _ => unreachable!(),
            }
            compare(
                &mut native,
                &mut reference,
                &data,
                0x027f,
                0x1f80,
                &format!("alias {case} ratio {ratio}"),
            );
        }
    }
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
    let data = classes(&pe, &table, 2, 2, 1, 0);
    compare(
        &mut native,
        &mut reference,
        &data,
        0x027f,
        0x1f80,
        "before helper patch",
    );
    let helper = addr(&table, &pe, "get16");
    for m in [&mut native, &mut reference] {
        m.cpu.write(helper, &[0xd9, 0xe8, 0xc3]).unwrap();
    }
    let calls = native.sys.host_calls;
    compare(
        &mut native,
        &mut reference,
        &data,
        0x027f,
        0x1f80,
        "patched helper",
    );
    assert_eq!(
        native.sys.host_calls, calls,
        "modified helper retained the host mixer"
    );
}
