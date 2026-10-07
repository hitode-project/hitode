//! Authored x86 calls exercise list identity, pixel snapshots and checked failures.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
static NEXT: AtomicUsize = AtomicUsize::new(0);

enum Arg {
    Value(u32),
    Memory(u32),
}
use Arg::{Memory as M, Value as I};
mod streams;

struct Fixture {
    machine: Machine<JitCpu>,
    code: Asm,
    code_address: u32,
    data: u32,
}

impl Fixture {
    fn new(jit: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/image-list-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let game = root.join("game");
        std::fs::create_dir_all(&game).unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        for (name, argc) in [
            ("ImageList_Create", 5),
            ("ImageList_Destroy", 1),
            ("ImageList_GetIconSize", 3),
            ("ImageList_ReplaceIcon", 3),
            ("ImageList_Read", 1),
        ] {
            assert_eq!(
                registry.get("comctl32.dll", name).unwrap().conv,
                adv_abi::Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("comctl32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            crate::Win32Config::new(
                game.canonicalize().unwrap(),
                "fixture.exe",
                root.join("data"),
            ),
        )
        .unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write_u32(READ_ONLY, 0xcccccccc).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            code_address: CODE,
            data: DATA,
        }
    }

    fn word(&mut self, value: u32) -> u32 {
        let pointer = self.data;
        self.data += 4;
        assert!(self.data < DATA + 4096);
        self.machine.cpu.write_u32(pointer, value).unwrap();
        pointer
    }

    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("comctl32.dll", name));
        let entry = self.machine.resolve_import(module, name).unwrap();
        for arg in args.iter().rev() {
            match arg {
                I(value) => {
                    self.code.push_imm(*value);
                }
                M(pointer) => {
                    self.code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        let output = self.word(0);
        self.code.call(entry).mov_mem_eax(output);
        output
    }

    fn error(&mut self) -> u32 {
        self.call("kernel32.dll!GetLastError", &[])
    }

    fn icon(&mut self, width: u32, height: u32, pixel: [u8; 4], mask: u8) -> u32 {
        self.machine
            .ext
            .get_mut::<Win32>()
            .user
            .objects
            .create_icon(
                &mut self.machine.sys.objects,
                "authored decoded icon".into(),
                crate::user32::IconImage {
                    width,
                    height,
                    pixels: pixel.repeat((width * height) as usize),
                    mask: vec![mask; (width * height) as usize],
                },
            )
            .unwrap()
    }

    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine
            .cpu
            .write(self.code_address, &self.code.bytes)
            .unwrap();
        self.machine
            .cpu
            .executor_mut()
            .add_region(Region::new(self.code_address, self.code.bytes.len()))
            .unwrap();
        self.machine
            .spawn("ImageList guest", self.code_address, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        self.code_address += 4096;
        self.code = Asm::new(self.code_address);
    }

    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }

    fn list(&self, result: u32) -> &ImageList {
        self.machine
            .ext
            .get::<ImageLists>()
            .get(&self.machine.sys.objects, self.value(result))
            .unwrap()
    }
}

#[test]
fn decoded_icons_are_copied_in_order_with_real_sizes_and_survive_source_destruction() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let first = test.icon(12, 8, [10, 20, 30, 128], 1);
        let second = test.icon(12, 8, [90, 80, 70, 255], 0);
        let list = test.call("ImageList_Create", &[I(12), I(8), I(0x21), I(3), I(0)]);
        let other = test.call("ImageList_Create", &[I(16), I(16), I(0x20), I(0), I(1)]);
        let cx = test.word(99);
        let cy = test.word(99);
        let size = test.call("ImageList_GetIconSize", &[M(list), I(cx), I(cy)]);
        let append0 = test.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(first)]);
        let append1 = test.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(first)]);
        let replace0 = test.call("ImageList_ReplaceIcon", &[M(list), I(0), I(second)]);
        test.call("user32.dll!DestroyIcon", &[I(first)]);
        test.call("user32.dll!DestroyIcon", &[I(second)]);
        test.run();
        assert_ne!(test.value(list), 0);
        assert_ne!(test.value(list), test.value(other));
        assert!(test.machine.sys.objects.get(test.value(list)).is_none());
        assert_eq!(
            (test.value(size), test.value(cx), test.value(cy)),
            (1, 12, 8)
        );
        assert_eq!(
            (
                test.value(append0),
                test.value(append1),
                test.value(replace0)
            ),
            (0, 1, 0)
        );
        assert!(test.list(other).images.is_empty()); // cInitial never invents image entries.
        let images = &test.list(list).images;
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].rgba, [90, 80, 70, 255].repeat(96));
        assert_eq!(images[0].mask.as_ref().unwrap(), &vec![0; 96]);
        assert_eq!(images[1].rgba, [10, 20, 30, 128].repeat(96));
        assert_eq!(images[1].mask.as_ref().unwrap(), &vec![1; 96]);
        let resources = &test.machine.ext.get::<Win32>().user.objects;
        assert!(!resources.contains(&test.machine.sys.objects, first));
        assert!(!resources.contains(&test.machine.sys.objects, second));
        let destroyed = test.call("ImageList_Destroy", &[M(list)]);
        let repeated = test.call("ImageList_Destroy", &[M(list)]);
        let stale = test.call("ImageList_GetIconSize", &[M(list), I(cx), I(cy)]);
        test.call("ImageList_Destroy", &[M(other)]);
        test.run();
        assert_eq!(
            (
                test.value(destroyed),
                test.value(repeated),
                test.value(stale)
            ),
            (1, 0, 0)
        );
        let lists = test.machine.ext.get::<ImageLists>();
        assert_eq!(
            test.machine
                .sys
                .objects
                .alias_count(lists.namespace.unwrap()),
            0
        );
        assert_eq!(lists.bytes(&test.machine.sys.objects), 0);
        assert!(
            test.machine
                .ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .is_empty()
        );
    }
}

#[test]
fn stock_pixels_streams_formats_and_bad_outputs_fail_without_mutating_list_or_sibling_output() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let wrong_size = test.icon(16, 16, [50, 60, 70, 255], 0);
        let stock = test.call("user32.dll!LoadIconW", &[I(0), I(32516)]);
        let list = test.call("ImageList_Create", &[I(12), I(12), I(0x21), I(0), I(0)]);
        let missing_pixels = test.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), M(stock)]);
        let missing_error = test.error();
        let resize = test.call(
            "ImageList_ReplaceIcon",
            &[M(list), I(u32::MAX), I(wrong_size)],
        );
        let resize_error = test.error();
        let index = test.call(
            "ImageList_ReplaceIcon",
            &[M(list), I(u32::MAX - 1), I(wrong_size)],
        );
        let index_error = test.error();
        let bad_icon = test.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(0xdead)]);
        let bad_icon_error = test.error();
        let invalid_handle = test.call(
            "ImageList_ReplaceIcon",
            &[I(0x20001), I(u32::MAX), I(wrong_size)],
        );
        let invalid_handle_error = test.error();
        let stream = test.call("ImageList_Read", &[I(0xdead0000)]);
        let stream_error = test.error();
        let unsupported_format =
            test.call("ImageList_Create", &[I(12), I(12), I(0x11), I(0), I(0)]);
        let unsupported_format_error = test.error();
        let bad_dimension = test.call("ImageList_Create", &[I(0), I(12), I(0x21), I(0), I(0)]);
        let bad_dimension_error = test.error();
        let cx = test.word(0xcccccccc);
        let cy = test.word(0xcccccccc);
        test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
        let null = test.call("ImageList_GetIconSize", &[M(list), I(cx), I(0)]);
        let null_error = test.error();
        let protected = test.call("ImageList_GetIconSize", &[M(list), I(cx), I(READ_ONLY)]);
        let protected_error = test.error();
        let overflow = test.call("ImageList_GetIconSize", &[M(list), I(u32::MAX - 1), I(cy)]);
        let overflow_error = test.error();
        test.run();
        for (result, error, expected, code) in [
            (missing_pixels, missing_error, u32::MAX, 50),
            (resize, resize_error, u32::MAX, 50),
            (index, index_error, u32::MAX, 6),
            (bad_icon, bad_icon_error, u32::MAX, 6),
            (invalid_handle, invalid_handle_error, u32::MAX, 6),
            (stream, stream_error, 0, 998),
            (unsupported_format, unsupported_format_error, 0, 50),
            (bad_dimension, bad_dimension_error, 0, 87),
            (null, null_error, 0, 0x12345678),
            (protected, protected_error, 0, 998),
            (overflow, overflow_error, 0, 998),
        ] {
            assert_eq!((test.value(result), test.value(error)), (expected, code));
        }
        assert_eq!(test.value(cx), 0xcccccccc);
        assert_eq!(test.value(cy), 0xcccccccc);
        assert_eq!(test.value(READ_ONLY), 0xcccccccc);
        assert!(test.list(list).images.is_empty());
        let lists = test.machine.ext.get::<ImageLists>();
        assert_eq!(
            test.machine
                .sys
                .objects
                .alias_count(lists.namespace.unwrap()),
            1
        );
        assert_eq!(lists.bytes(&test.machine.sys.objects), 0);
        let diagnostic = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostic.stub_calls, 0);
        assert!(
            diagnostic
                .warnings
                .keys()
                .any(|key| key.0 == "comctl32.dll!ImageList_ReplaceIcon"
                    && key.1.contains("decoded pixels are unavailable"))
        );
    }
}
