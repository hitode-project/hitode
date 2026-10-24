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
const LEN: u32 = 0x10000;
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
fn machine(dist: &Path, pe: &Pe, image: &[u8], table: &str) -> Machine<Cpu> {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, true).unwrap(),
        Platform,
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
    let mut code = vec![0x68];
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
    m.cpu.write(DATA, data).unwrap();
    m.spawn("mixer", CODE, &[], 0x10000).unwrap();
    let result = match m.run() {
        RunResult::Exited(_) => Ok(()),
        RunResult::Failed(error) => Err(format!("{:#x} {}", error.pc, error.message)),
        other => Err(format!("{other:?}")),
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
    assert_eq!(native.sys.stats.leaf_abandons, 0);
    assert!(native.sys.host_calls > 60);
    assert_eq!(reference.sys.host_calls, 0);
    // Each rejected buffer is last: a decline must leave earlier buffers untouched.
    for case in 0..13 {
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
                put(&mut data, last, 0x134, last + 0x300); // filters
                // DSFilter.inplace, then the object's vtable and Process slot.
                put(&mut data, last + 0x300, 20, last + 0x340);
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
            12 => {
                put(&mut data, DATA, d::tmp_buffer, MIX);
                put(&mut data, DATA, d::tmp_buffer_len, FRAMES * 2 * 4);
            }
            _ => unreachable!(),
        }
        let before = native.sys.stats.leaf_abandons;
        let actual = call(&mut native, &data);
        let expected = call(&mut reference, &data);
        assert_eq!(actual.0, expected.0, "decline {case}");
        assert_eq!(
            actual.0.is_err(),
            matches!(case, 6 | 7 | 9),
            "decline {case}"
        );
        assert!(actual.1 == expected.1, "decline {case} changed memory");
        assert_eq!(
            native.sys.stats.leaf_abandons - before,
            u64::from(case != 8),
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
