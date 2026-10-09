//! Usage: cargo run -p adv-win32 --release --example hot_hle -- --data-dir DIR.
//! Inputs: an owned data directory; only repository-authored names and tones.
//! Outputs: aggregate ns/call for parameters, guest reads, NLS, heap and codec
//! service, plus a complete authored trap loop. No private guest state is printed.
//! Direct calls use the owning backend's conservative copied-read interface;
//! the trap loop enters the production guest view with borrowable storage.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_device::vorbis::{ByteOrder, Next, PcmFormat, Stream};
use adv_machine::{Clock, Env, Handler, Machine, Outcome, Resume, RunResult, layout::PARK_ADDR};
use adv_os::{
    Heaps,
    global::{Block, Globals},
    nls::Codec,
};
use adv_win32::{
    Win32, Win32Config,
    vorbisfile::{self, Policy},
};
use std::{hint::black_box, time::Instant};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const STACK: u32 = DATA + 0xe000;
const TONE: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-mono.ogg");
const S16: PcmFormat = PcmFormat {
    bytes_per_sample: 2,
    byte_order: ByteOrder::Little,
    signed: true,
};

fn measure(name: &str, count: usize, mut body: impl FnMut()) {
    for _ in 0..100 {
        body();
    }
    let start = Instant::now();
    for _ in 0..count {
        body();
    }
    println!(
        "{name}: calls={count} ns/call={:.2}",
        start.elapsed().as_nanos() as f64 / count as f64
    );
}
fn arguments(machine: &mut Machine<JitCpu>, args: &[u32]) {
    machine.cpu.set_reg(Reg::Esp, STACK);
    for (i, arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(STACK + 4 + i as u32 * 4, *arg)
            .unwrap();
    }
}
fn invoke(machine: &mut Machine<JitCpu>, handler: Handler, thunk: u32) -> Outcome {
    handler(&mut Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    })
}
fn value(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(v) => v,
        _ => panic!("expected synchronous service"),
    }
}
fn binding(machine: &mut Machine<JitCpu>, module: &str, name: &str) -> u32 {
    machine.resolve_import(module, name).unwrap();
    machine
        .sys
        .thunks
        .iter()
        .find(|(_, t)| t.module == module && t.name == name)
        .unwrap()
        .0
}
fn compare_loop(cmp: u32, calls: u32) -> Asm {
    assert!(calls != 0);
    let mut code = Asm::new(CODE);
    code.bytes.push(0xbb); // mov ebx, calls
    code.bytes.extend_from_slice(&calls.to_le_bytes());
    let loop_at = code.here();
    code.push_imm(DATA + 256).push_imm(DATA).call(cmp);
    code.bytes.push(0x4b); // dec ebx
    code.jnz(loop_at);
    code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    code
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let data_dir = &args
        .windows(2)
        .find(|a| a[0] == "--data-dir")
        .expect("--data-dir DIR")[1];
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    vorbisfile::register(&mut registry, Policy::Native);
    let compare = registry.get("kernel32.dll", "lstrcmpiA").unwrap().handler;
    let alloc = registry.get("kernel32.dll", "GlobalAlloc").unwrap().handler;
    let free = registry.get("kernel32.dll", "GlobalFree").unwrap().handler;
    let open = registry
        .get("vorbisfile.dll", "ov_open_callbacks")
        .unwrap()
        .handler;
    let read = registry.get("vorbisfile.dll", "ov_read").unwrap().handler;
    let clear = registry.get("vorbisfile.dll", "ov_clear").unwrap().handler;
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(data_dir, "hot-hle.exe", data_dir),
    )
    .unwrap();
    vorbisfile::install(&mut machine, Policy::Native).unwrap();
    machine.cpu.map(CODE, 65536, Prot::RX).unwrap();
    machine.cpu.map(DATA, 65536, Prot::RW).unwrap();
    machine
        .cpu
        .write(CODE + 0x8000, &[0x31, 0xc0, 0xc3])
        .unwrap();
    machine.cpu.write(DATA + 0x4000, TONE).unwrap();
    machine
        .spawn("authored HLE benchmark", CODE, &[], 65536)
        .unwrap();
    let cmp_id = binding(&mut machine, "kernel32.dll", "lstrcmpiA");
    let alloc_id = binding(&mut machine, "kernel32.dll", "GlobalAlloc");
    let free_id = binding(&mut machine, "kernel32.dll", "GlobalFree");
    let open_id = binding(&mut machine, "vorbisfile.dll", "ov_open_callbacks");
    let read_id = binding(&mut machine, "vorbisfile.dll", "ov_read");
    let clear_id = binding(&mut machine, "vorbisfile.dll", "ov_clear");
    arguments(&mut machine, &[DATA, DATA + 256]);
    measure("stack_two_parameters", 1_000_000, || {
        black_box((machine.cpu.arg(0).unwrap(), machine.cpu.arg(1).unwrap()));
    });
    for (name, a, b, cp) in [
        (
            "ascii_equal",
            &b"resource_001.ogg"[..],
            &b"RESOURCE_001.OGG"[..],
            932,
        ),
        (
            "ascii_mismatch",
            &b"resource_001.ogg"[..],
            &b"sound_002.ogg"[..],
            932,
        ),
        (
            "sjis",
            &b"\x82\xa0name.ogg"[..],
            &b"\x82\xa2NAME.OGG"[..],
            932,
        ),
        (
            "gbk",
            &b"\xd6\xd0name.ogg"[..],
            &b"\xce\xc4NAME.OGG"[..],
            936,
        ),
    ] {
        machine.ext.get_mut::<Win32>().codec = Codec::new(cp);
        machine.cpu.write(DATA, &[a, &[0]].concat()).unwrap();
        machine.cpu.write(DATA + 256, &[b, &[0]].concat()).unwrap();
        measure(&format!("{name}_byte_fold"), 500_000, || {
            black_box(
                black_box(a)
                    .iter()
                    .map(u8::to_ascii_lowercase)
                    .cmp(black_box(b).iter().map(u8::to_ascii_lowercase)),
            );
        });
        measure(&format!("{name}_two_cstr_copies"), 500_000, || {
            black_box((
                machine.cpu.read_cstr(DATA, 65536).unwrap(),
                machine.cpu.read_cstr(DATA + 256, 65536).unwrap(),
            ));
        });
        measure(&format!("{name}_nls_decode_fold"), 250_000, || {
            let codec = Codec::new(cp);
            black_box(
                codec
                    .to_utf16(0, a)
                    .iter()
                    .map(|&c| adv_os::nls::unit_case(c, false))
                    .cmp(
                        codec
                            .to_utf16(0, b)
                            .iter()
                            .map(|&c| adv_os::nls::unit_case(c, false)),
                    ),
            );
        });
        measure(&format!("{name}_handler"), 500_000, || {
            black_box(value(invoke(&mut machine, compare, cmp_id)));
        });
    }
    for live in [false, true] {
        if live {
            Win32::prepare_process_infrastructure(&mut machine).unwrap();
        }
        for flags in [0, 0x40, 2, 0x42] {
            measure(
                &format!("global_pair_live_{live}_flags_{flags}"),
                100_000,
                || {
                    arguments(&mut machine, &[flags, 64]);
                    let h = value(invoke(&mut machine, alloc, alloc_id));
                    assert_ne!(h, 0);
                    arguments(&mut machine, &[h]);
                    assert_eq!(value(invoke(&mut machine, free, free_id)), 0);
                },
            );
            if live {
                let mut alloc_ns = 0;
                let mut free_ns = 0;
                for _ in 0..50_000 {
                    arguments(&mut machine, &[flags, 64]);
                    let start = Instant::now();
                    let h = value(invoke(&mut machine, alloc, alloc_id));
                    alloc_ns += start.elapsed().as_nanos();
                    assert_ne!(h, 0);
                    arguments(&mut machine, &[h]);
                    let start = Instant::now();
                    assert_eq!(value(invoke(&mut machine, free, free_id)), 0);
                    free_ns += start.elapsed().as_nanos();
                }
                println!(
                    "global_live_flags_{flags}: calls=50000 alloc_ns/call={:.2} free_ns/call={:.2}",
                    alloc_ns as f64 / 50_000.0,
                    free_ns as f64 / 50_000.0
                );
            }
        }
    }
    for movable in [false, true] {
        let mut globals = Globals::default();
        let mut objects = adv_machine::Objects::default();
        measure(
            &format!("global_metadata_pair_movable_{movable}"),
            100_000,
            || {
                let handle = globals
                    .insert(
                        &mut objects,
                        Block {
                            handle: 0,
                            allocation: DATA,
                            pointer: DATA,
                            size: 64,
                            movable,
                            locks: 0,
                            attributes: 0,
                            clipboard: false,
                        },
                    )
                    .unwrap();
                assert!(globals.remove(&mut objects, handle).is_some());
            },
        );
    }
    measure("heap_service_pair", 100_000, || {
        let heaps = machine.ext.get_mut::<Heaps>();
        let h = heaps
            .alloc(&mut machine.cpu, &mut machine.sys.vm, 0, 64, false)
            .unwrap();
        assert!(heaps.free(&mut machine.cpu, &mut machine.sys.vm, 0, h));
    });
    let mut total_calls = 0;
    let mut total_bytes = 0;
    let mut read_ns = 0;
    let mut decode_ns = 0;
    let mut decode_calls = 0;
    for _ in 0..100 {
        arguments(
            &mut machine,
            &[
                1,
                DATA + 0x1000,
                DATA + 0x4000,
                TONE.len() as u32,
                CODE + 0x8000,
                0,
                0,
                0,
            ],
        );
        assert_eq!(value(invoke(&mut machine, open, open_id)), 0);
        arguments(
            &mut machine,
            &[DATA + 0x1000, DATA + 0x2000, 512, 0, 2, 1, DATA + 0x3000],
        );
        loop {
            let packets = vorbisfile::stats(&machine).unwrap().decoded_packets;
            let start = Instant::now();
            let result = invoke(&mut machine, read, read_id);
            let ns = start.elapsed().as_nanos();
            match result {
                Outcome::Return(n) if n > 0 && n < 513 => {
                    total_calls += 1;
                    total_bytes += n;
                    read_ns += ns;
                    if vorbisfile::stats(&machine).unwrap().decoded_packets != packets {
                        decode_ns += ns;
                        decode_calls += 1;
                    }
                }
                Outcome::Call { target, then, .. } => {
                    assert_eq!(target, CODE + 0x8000);
                    let result = then(
                        &mut Env {
                            context: 0,
                            cpu: &mut machine.cpu,
                            sys: &mut machine.sys,
                            ext: &mut machine.ext,
                            thunk: read_id,
                        },
                        Resume::Returned(0),
                    );
                    assert_eq!(value(result), 0);
                    break;
                }
                Outcome::Return(0) => break,
                _ => panic!("codec read failed"),
            }
        }
        arguments(&mut machine, &[DATA + 0x1000]);
        assert_eq!(value(invoke(&mut machine, clear, clear_id)), 0);
    }
    println!(
        "ov_read_512: streams=100 calls={total_calls} bytes={total_bytes} ns/call={:.2}",
        read_ns as f64 / total_calls as f64
    );
    println!(
        "ov_read_decoding: calls={decode_calls} ns/call={:.2}; buffered_calls={} buffered_ns/call={:.2}",
        decode_ns as f64 / decode_calls as f64,
        total_calls - decode_calls,
        (read_ns - decode_ns) as f64 / (total_calls - decode_calls) as f64
    );
    let mut stream = Stream::new(TONE.to_vec());
    for _ in 0..3 {
        let Next::Packet(p) = stream.next_packet() else {
            panic!("header")
        };
        stream.accept_header(p).unwrap();
    }
    while !stream.has_pcm() {
        let Next::Packet(p) = stream.next_packet() else {
            panic!("packet")
        };
        stream.decode_packet(p).unwrap();
    }
    measure("pcm_peek_512", 250_000, || {
        black_box(stream.peek_pcm(512, S16));
    });
    machine.cpu.write(DATA, b"resource_001.ogg\0").unwrap();
    machine
        .cpu
        .write(DATA + 256, b"RESOURCE_001.OGG\0")
        .unwrap();
    let cmp = machine.resolve_import("kernel32.dll", "lstrcmpiA").unwrap();
    let code = compare_loop(cmp, 100_000);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let start = Instant::now();
    let result = machine.run_virtual(100_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    println!(
        "ascii_trap_loop: calls=100000 ns/call={:.2}",
        start.elapsed().as_nanos() as f64 / 100_000.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::Conv;

    #[test]
    fn trap_loop_denominator_matches_actual_calls_and_preserves_arguments() {
        for jit in [false, true] {
            let mut registry = Registry::default();
            registry.add("fixture", "compare", Conv::Stdcall { argc: 2 }, |e| {
                assert_eq!((e.arg(0), e.arg(1)), (DATA, DATA + 256));
                let count = e.cpu.read_u32(DATA).unwrap();
                e.cpu.write_u32(DATA, count + 1).unwrap();
                Outcome::Return(0)
            });
            let mut machine = Machine::new(
                JitCpu::new(PARK_ADDR, jit).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
            machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
            let cmp = machine.resolve_import("fixture", "compare").unwrap();
            let code = compare_loop(cmp, 257);
            machine.cpu.write(CODE, &code.bytes).unwrap();
            machine
                .spawn("benchmark denominator", CODE, &[], 65536)
                .unwrap();
            let result = machine.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 257);
        }
    }
}
