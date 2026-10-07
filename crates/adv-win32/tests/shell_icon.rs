//! ExtractIconEx calls originate in x86 guests; all PE/ICO input is authored here.
#[path = "shell_icon/associated.rs"]
mod associated;

use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

struct Fixture {
    machine: Machine<JitCpu>,
    next: u32,
}
impl Fixture {
    fn new(jit: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shell-icon-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("図像.dll"), pe()).unwrap();
        std::fs::write(root.join("図像.ico"), ico()).unwrap();
        std::fs::write(root.join("unknown.bin"), b"unknown image format").unwrap();
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut cfg = Win32Config::new(
            root.canonicalize().unwrap(),
            "icon-test.exe",
            root.join("data"),
        );
        cfg.os.codepage = 932;
        Win32::install(&mut machine, cfg).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let registry = (machine.sys.thunks.resolver() as &dyn std::any::Any)
            .downcast_ref::<Registry>()
            .unwrap();
        assert!(registry.signature_conflicts().is_empty());
        Self {
            machine,
            next: DATA,
        }
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let address = self.next;
        self.next += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.next < DATA + 4096);
        self.machine.cpu.write(address, bytes).unwrap();
        address
    }
    fn path(&mut self, name: &str, wide: bool) -> u32 {
        if wide {
            self.bytes(
                &name
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
        } else {
            let mut bytes = self.machine.ext.get::<Win32>().codec.encode(name);
            bytes.push(0);
            self.bytes(&bytes)
        }
    }
    fn call(&mut self, code: &mut Asm, api: &str, args: &[Arg]) -> u32 {
        let (module, name) = api.split_once('!').unwrap_or(("shell32.dll", api));
        let entry = self.machine.resolve_import(module, name).unwrap();
        for arg in args.iter().rev() {
            match arg {
                I(value) => {
                    code.push_imm(*value);
                }
                M(address) => {
                    code.mov_eax_mem(*address).push_eax();
                }
            }
        }
        let output = self.bytes(&[0; 4]);
        code.call(entry).mov_mem_eax(output);
        output
    }
    fn run(&mut self, mut code: Asm) {
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        assert!(code.bytes.len() < 4096);
        self.machine.cpu.write(CODE, &code.bytes).unwrap();
        self.machine
            .spawn("icon extraction", CODE, &[], 0x10000)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn u32(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
    fn destroy(&mut self, code: &mut Asm, address: u32, count: u32) {
        for offset in (0..count * 4).step_by(4) {
            self.call(code, "user32.dll!DestroyIcon", &[M(address + offset)]);
        }
    }
}

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn dib(size: u32, color: [u8; 4]) -> Vec<u8> {
    let mask = ((size + 31) / 32) * 4;
    let mut bytes = vec![0; (40 + size * size * 4 + mask * size) as usize];
    put32(&mut bytes, 0, 40);
    put32(&mut bytes, 4, size);
    put32(&mut bytes, 8, size * 2);
    put16(&mut bytes, 12, 1);
    put16(&mut bytes, 14, 32);
    for pixel in bytes[40..40 + (size * size * 4) as usize].chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
    bytes
}

fn ico() -> Vec<u8> {
    let images = [dib(16, [0, 0, 255, 255]), dib(32, [0, 255, 0, 255])];
    let mut bytes = vec![0; 38];
    put16(&mut bytes, 2, 1);
    put16(&mut bytes, 4, 2);
    for (index, image) in images.iter().enumerate() {
        let entry = 6 + index * 16;
        bytes[entry] = if index == 0 { 16 } else { 32 };
        bytes[entry + 1] = bytes[entry];
        put16(&mut bytes, entry + 4, 1);
        put16(&mut bytes, entry + 6, 32);
        put32(&mut bytes, entry + 8, image.len() as u32);
        let offset = bytes.len() as u32;
        put32(&mut bytes, entry + 12, offset);
        bytes.extend_from_slice(image);
    }
    bytes
}

fn pe() -> Vec<u8> {
    let images = [dib(16, [0, 0, 255, 255]), dib(32, [0, 255, 0, 255])];
    let mut group = vec![0; 34];
    put16(&mut group, 2, 1);
    put16(&mut group, 4, 2);
    for (index, image) in images.iter().enumerate() {
        let entry = 6 + index * 14;
        group[entry] = if index == 0 { 16 } else { 32 };
        group[entry + 1] = group[entry];
        put16(&mut group, entry + 4, 1);
        put16(&mut group, entry + 6, 32);
        put32(&mut group, entry + 8, image.len() as u32);
        put16(&mut group, entry + 12, index as u16 + 1);
    }
    let mut resource = vec![0; 0x200];
    for (directory, entries) in [
        (0, 2),
        (0x30, 2),
        (0x60, 2),
        (0x90, 1),
        (0xb0, 1),
        (0xd0, 1),
        (0xf0, 1),
    ] {
        put16(&mut resource, directory + 14, entries);
    }
    for (offset, id, target) in [
        (16, 3, 0x30),
        (24, 14, 0x60),
        (0x40, 1, 0x90),
        (0x48, 2, 0xb0),
        (0x70, 101, 0xd0),
        (0x78, 202, 0xf0),
    ] {
        put32(&mut resource, offset, id);
        put32(&mut resource, offset + 4, 0x80000000 | target);
    }
    for (index, directory) in [0x90, 0xb0, 0xd0, 0xf0].into_iter().enumerate() {
        put32(&mut resource, directory + 16, 0x409);
        put32(&mut resource, directory + 20, 0x110 + index as u32 * 16);
    }
    for (index, data) in [
        images[0].as_slice(),
        images[1].as_slice(),
        group.as_slice(),
        group.as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let rva = 0x1000 + resource.len() as u32;
        put32(&mut resource, 0x110 + index * 16, rva);
        put32(&mut resource, 0x114 + index * 16, data.len() as u32);
        resource.extend_from_slice(data);
    }
    let raw_size = resource.len().next_multiple_of(512);
    let mut file = vec![0; 512 + raw_size];
    file[..2].copy_from_slice(b"MZ");
    put32(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut file, 0x84, 0x14c);
    put16(&mut file, 0x86, 1);
    put16(&mut file, 0x94, 224);
    put16(&mut file, 0x96, 0x2102);
    let optional = 0x98;
    put16(&mut file, optional, 0x10b);
    put32(&mut file, optional + 28, 0x400000);
    put32(&mut file, optional + 32, 4096);
    put32(&mut file, optional + 36, 512);
    put32(
        &mut file,
        optional + 56,
        (0x1000 + resource.len()).next_multiple_of(4096) as u32,
    );
    put32(&mut file, optional + 60, 512);
    put32(&mut file, optional + 92, 16);
    put32(&mut file, optional + 112, 0x1000);
    put32(&mut file, optional + 116, resource.len() as u32);
    let section = optional + 224;
    file[section..section + 5].copy_from_slice(b".rsrc");
    put32(&mut file, section + 8, resource.len() as u32);
    put32(&mut file, section + 12, 0x1000);
    put32(&mut file, section + 16, raw_size as u32);
    put32(&mut file, section + 20, 512);
    put32(&mut file, section + 36, 0x40000040);
    file[512..512 + resource.len()].copy_from_slice(&resource);
    file
}

#[test]
fn pe_icons_use_group_counts_resource_ids_two_sizes_and_user_lifetimes() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let before = f.machine.sys.objects.object_count();
        let ansi = f.path("図像.dll", false);
        let wide = f.path("図像.dll", true);
        let large = f.bytes(&[0xcc; 16]);
        let small = f.bytes(&[0xdd; 16]);
        let second_large = f.bytes(&[0xcc; 4]);
        let second_small = f.bytes(&[0xdd; 4]);
        let mut a = Asm::new(CODE);
        let query = f.call(
            &mut a,
            "ExtractIconExA",
            &[I(ansi), I(u32::MAX), I(0), I(0), I(0)],
        );
        let query_w = f.call(
            &mut a,
            "ExtractIconExW",
            &[I(wide), I(u32::MAX), I(0), I(0), I(1)],
        );
        let partial = f.call(
            &mut a,
            "ExtractIconExA",
            &[I(ansi), I(0), I(large), I(small), I(4)],
        );
        let by_id = f.call(
            &mut a,
            "ExtractIconExW",
            &[
                I(wide),
                I((-202i32) as u32),
                I(second_large),
                I(second_small),
                I(1),
            ],
        );
        f.destroy(&mut a, large, 2);
        f.destroy(&mut a, small, 2);
        f.destroy(&mut a, second_large, 1);
        f.destroy(&mut a, second_small, 1);
        f.run(a);
        assert_eq!(f.u32(query), 2);
        assert_eq!(f.u32(query_w), 2);
        assert_eq!(f.u32(partial), 2);
        assert_eq!(f.u32(by_id), 2);
        let handles = [
            f.u32(large),
            f.u32(large + 4),
            f.u32(small),
            f.u32(small + 4),
            f.u32(second_large),
            f.u32(second_small),
        ];
        assert!(handles.iter().all(|&handle| handle != 0));
        assert_eq!(
            handles
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6
        );
        for address in [large + 8, large + 12, small + 8, small + 12] {
            assert_eq!(f.u32(address), 0);
        }
        assert_eq!(f.machine.sys.objects.object_count(), before + 1); // Retained guest thread.
    }
}

#[test]
fn ico_count_and_failure_paths_preserve_documented_outputs_without_allocating_icons() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let before = f.machine.sys.objects.object_count();
        let path = f.path("図像.ico", true);
        let missing = f.path("missing.ico", false);
        let unsupported = f.path("unknown.bin", false);
        let large = f.bytes(&[0xcc; 8]);
        let small = f.bytes(&[0xdd; 8]);
        let negative_large = f.bytes(&[0xcc; 8]);
        let negative_small = f.bytes(&[0xdd; 8]);
        let zero_large = f.bytes(&[0xcc; 8]);
        let zero_small = f.bytes(&[0xdd; 8]);
        let mut a = Asm::new(CODE);
        let query = f.call(
            &mut a,
            "ExtractIconExW",
            &[I(path), I(u32::MAX), I(0), I(0), I(0)],
        );
        let extracted = f.call(
            &mut a,
            "ExtractIconExW",
            &[I(path), I(0), I(large), I(small), I(2)],
        );
        let negative = f.call(
            &mut a,
            "ExtractIconExW",
            &[
                I(path),
                I(u32::MAX),
                I(negative_large),
                I(negative_small),
                I(1),
            ],
        );
        f.call(&mut a, "kernel32.dll!SetLastError", &[I(0x12345678)]);
        let zero = f.call(
            &mut a,
            "ExtractIconExW",
            &[I(path), I(0), I(zero_large), I(zero_small), I(0)],
        );
        let zero_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let not_found_query = f.call(
            &mut a,
            "ExtractIconExA",
            &[I(missing), I(u32::MAX), I(0), I(0), I(1)],
        );
        let query_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let not_found = f.call(
            &mut a,
            "ExtractIconExA",
            &[I(missing), I(0), I(zero_large), I(zero_small), I(1)],
        );
        let file_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let invalid_output = f.call(
            &mut a,
            "ExtractIconExW",
            &[I(path), I(0), I(CODE), I(zero_small), I(1)],
        );
        let output_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let no_icon = f.call(
            &mut a,
            "ExtractIconExA",
            &[
                I(unsupported),
                I(0),
                I(negative_large),
                I(negative_small),
                I(1),
            ],
        );
        f.destroy(&mut a, large, 1);
        f.destroy(&mut a, small, 1);
        f.run(a);
        for (output, expected) in [
            (query, 1),
            (extracted, 1),
            (negative, 1),
            (zero, u32::MAX),
            (zero_error, 0x12345678),
            (not_found_query, 0),
            (query_error, 2),
            (not_found, u32::MAX),
            (file_error, 2),
            (invalid_output, u32::MAX),
            (output_error, 998),
            (no_icon, 0),
        ] {
            assert_eq!(f.u32(output), expected);
        }
        assert_ne!(f.u32(large), 0);
        assert_ne!(f.u32(small), 0);
        assert_ne!(f.u32(large), f.u32(small));
        assert_eq!(f.u32(large + 4), 0);
        assert_eq!(f.u32(small + 4), 0);
        assert_eq!(f.u32(negative_large), 0);
        assert_eq!(f.u32(negative_small), 0);
        assert_eq!(f.u32(negative_large + 4), 0xcccccccc);
        assert_eq!(f.u32(negative_small + 4), 0xdddddddd);
        for address in [zero_large, zero_large + 4] {
            assert_eq!(f.u32(address), 0xcccccccc);
        }
        for address in [zero_small, zero_small + 4] {
            assert_eq!(f.u32(address), 0xdddddddd);
        }
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}
