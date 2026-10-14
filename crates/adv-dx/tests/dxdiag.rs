//! Real x86 COM activation, generated vtables and shared Automation ownership.
//! Device fronts are installed without opening audio or creating a GPU surface.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_dx::dxdiag::{CLSID_PROVIDER, IID_CONTAINER, IID_PROVIDER};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

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
    fn new(jit: bool, fronts: bool) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        adv_win32::ole32::register(&mut registry);
        adv_dx::dxdiag::register(&mut registry);
        adv_dx::dsound::register(&mut registry);
        #[cfg(feature = "gpu")]
        adv_dx::d3d9::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/dxdiag",
                "dxdiag-test.exe",
                "/nonexistent/dxdiag-data",
            ),
        )
        .unwrap();
        if fronts {
            adv_dx::dsound::install(&mut machine).unwrap();
            // disabled sound output still has the DirectSound COM protocol.
            machine.ext.get_mut::<adv_dx::dsound::Sound>().enabled = false;
            #[cfg(feature = "gpu")]
            adv_dx::d3d9::install(
                &mut machine,
                adv_dx::d3d9::SurfaceProvider {
                    acquire_dc: |_, _, _, _, _| panic!("DxDiag never borrows DCs"),
                    writeback_dc: |_, _, _, _| panic!("DxDiag never borrows DCs"),
                    release_dc: |_, _| panic!("DxDiag never borrows DCs"),
                    insert: |_, _| panic!("DxDiag never creates surfaces"),
                    target: |_, _| None,
                    remove: |_, _, _| panic!("DxDiag never removes surfaces"),
                    replace: |_, _, _| panic!("DxDiag never replaces surfaces"),
                    present: |_, _, _| panic!("DxDiag never presents"),
                    present_copy: |_, _, _| panic!("DxDiag never presents"),
                    window: |_, _, _| None,
                    visible: |_, _, _| None,
                    fast_forward: |_| false,
                    display: |_| (1920, 1080, 32),
                    mode_changed: |_, _, _| panic!("DxDiag never changes display mode"),
                    device_released: |_, _| panic!("DxDiag never creates devices"),
                    completed: |_, _| None,
                },
            )
            .unwrap();
        }
        adv_dx::dxdiag::install(&mut machine);
        machine.cpu.map(CODE, 0x2000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
        let registry = (machine.sys.thunks.resolver() as &dyn std::any::Any)
            .downcast_ref::<Registry>()
            .unwrap();
        assert!(registry.signature_conflicts().is_empty());
        Self {
            machine,
            next: DATA,
        }
    }
    fn alloc(&mut self, length: u32) -> u32 {
        let address = self.next;
        self.next += length.next_multiple_of(4);
        assert!(self.next < DATA + 0x4000);
        address
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let address = self.alloc(bytes.len() as u32);
        self.machine.cpu.write(address, bytes).unwrap();
        address
    }
    fn wide(&mut self, text: &str) -> u32 {
        self.bytes(
            &text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    fn arguments(code: &mut Asm, args: &[Arg]) {
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
    }
    fn call(&mut self, code: &mut Asm, module: &str, api: &str, args: &[Arg]) -> u32 {
        Self::arguments(code, args);
        let output = self.alloc(4);
        code.call(self.machine.resolve_import(module, api).unwrap())
            .mov_mem_eax(output);
        output
    }
    fn method(&mut self, code: &mut Asm, object: u32, slot: u32, args: &[Arg]) -> u32 {
        Self::arguments(code, args);
        code.mov_eax_mem(object).push_eax();
        code.mov_eax_mem(object)
            .mov_eax_ptr_eax()
            .add_eax_imm(slot * 4)
            .mov_eax_ptr_eax();
        code.bytes.extend_from_slice(&[0xff, 0xd0]); // call through the actual vtable.
        let output = self.alloc(4);
        code.mov_mem_eax(output);
        output
    }
    fn provider(&mut self, code: &mut Asm) -> (u32, u32) {
        let class = self.bytes(&CLSID_PROVIDER);
        let iid = self.bytes(&IID_PROVIDER);
        let output = self.alloc(4);
        let result = self.call(
            code,
            "ole32.dll",
            "CoCreateInstance",
            &[I(class), I(0), I(1), I(iid), I(output)],
        );
        (output, result)
    }
    fn params(&mut self, version: u32, reserved: u32) -> u32 {
        self.bytes(
            &[16u32, version, 0, reserved]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    fn run(&mut self, mut code: Asm) {
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        assert!(code.bytes.len() < 0x2000);
        self.machine.cpu.write(CODE, &code.bytes).unwrap();
        self.machine
            .spawn("DxDiag guest", CODE, &[], 0x10000)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn u32(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
    fn read(&self, address: u32, count: usize) -> Vec<u8> {
        let mut bytes = vec![0; count];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
}

#[cfg(feature = "gpu")]
#[test]
fn installed_fronts_report_9_0_c_with_shared_variants_and_independent_container_lifetimes() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit, true);
        let before = f.machine.sys.objects.object_count();
        let params = f.params(111, 0);
        let system_name = f.wide("DxDiag_SystemInfo");
        let prop_names = [
            f.wide("dwDirectXVersionMajor"),
            f.wide("dwDirectXVersionMinor"),
            f.wide("szDirectXVersionLetter"),
        ];
        let unknown_name = f.wide("InventedHostHardware");
        let unknown_iid = f.bytes(&IID_CONTAINER);
        let iunknown = f.bytes(&adv_os::com::IUNKNOWN);
        let root = f.alloc(4);
        let root_again = f.alloc(4);
        let system = f.alloc(4);
        let canonical = f.alloc(4);
        let rejected = f.alloc(4);
        let variants = [f.alloc(16), f.alloc(16), f.alloc(16)];
        let enum_name = f.bytes(&[0xcc; 80]);
        let too_small = f.bytes(&[0xdd; 8]);
        let child_count = f.alloc(4);
        let prop_count = f.alloc(4);
        let mut a = Asm::new(CODE);
        let (provider, created) = f.provider(&mut a);
        let not_initialized = f.method(&mut a, provider, 4, &[I(root)]);
        let initialized = f.method(&mut a, provider, 3, &[I(params)]);
        let queried = f.method(&mut a, provider, 0, &[I(iunknown), I(canonical)]);
        let unknown_interface = f.method(&mut a, provider, 0, &[I(unknown_iid), I(rejected)]);
        let canonical_released = f.method(&mut a, canonical, 2, &[]);
        let rooted = f.method(&mut a, provider, 4, &[I(root)]);
        let rooted_again = f.method(&mut a, provider, 4, &[I(root_again)]);
        let counted_children = f.method(&mut a, root, 3, &[I(child_count)]);
        let small_name = f.method(&mut a, root, 4, &[I(0), I(too_small), I(4)]);
        let named = f.method(&mut a, root, 4, &[I(0), I(enum_name), I(40)]);
        let child = f.method(&mut a, root, 5, &[I(system_name), I(system)]);
        let counted_props = f.method(&mut a, system, 6, &[I(prop_count)]);
        let enum_prop = f.alloc(80);
        let named_prop = f.method(&mut a, system, 7, &[I(2), I(enum_prop), I(40)]);
        let variant_tags = [f.alloc(4), f.alloc(4), f.alloc(4)];
        let variant_values = [f.alloc(4), f.alloc(4), f.alloc(4)];
        let mut gets = Vec::new();
        for index in 0..3 {
            gets.push(f.method(
                &mut a,
                system,
                8,
                &[I(prop_names[index]), I(variants[index])],
            ));
            a.mov_eax_mem(variants[index])
                .mov_mem_eax(variant_tags[index]);
            a.mov_eax_mem(variants[index] + 8)
                .mov_mem_eax(variant_values[index]);
        }
        let string_length = f.call(
            &mut a,
            "oleaut32.dll",
            "SysStringLen",
            &[M(variant_values[2])],
        );
        let letter = f.alloc(4);
        a.mov_eax_mem(variant_values[2])
            .mov_eax_ptr_eax()
            .mov_mem_eax(letter);
        let invalid_variant = f.bytes(&[0xff; 16]);
        let unknown_prop = f.method(&mut a, system, 8, &[I(unknown_name), I(invalid_variant)]);
        let bad_output = f.method(&mut a, system, 8, &[I(prop_names[2]), I(CODE)]);
        let invalid_child = f.method(&mut a, root, 5, &[I(unknown_name), I(rejected)]);
        // Containers have their own lifetime and immutable capability snapshot.
        let provider_released = f.method(&mut a, provider, 2, &[]);
        let again_released = f.method(&mut a, root_again, 2, &[]);
        let root_released = f.method(&mut a, root, 2, &[]);
        let alive = f.method(&mut a, system, 6, &[I(prop_count)]);
        let clear: Vec<_> = variants
            .iter()
            .map(|&variant| f.call(&mut a, "oleaut32.dll", "VariantClear", &[I(variant)]))
            .collect();
        let final_release = f.method(&mut a, system, 2, &[]);
        f.run(a);
        for output in [
            created,
            initialized,
            queried,
            rooted,
            rooted_again,
            counted_children,
            named,
            child,
            counted_props,
            named_prop,
            alive,
            provider_released,
            root_released,
            final_release,
        ] {
            assert_eq!(f.u32(output), 0);
        }
        for output in gets.into_iter().chain(clear) {
            assert_eq!(f.u32(output), 0);
        }
        assert_eq!(f.u32(not_initialized), 0x800401f0);
        assert_eq!(f.u32(unknown_interface), 0x80004002);
        assert_eq!(f.u32(canonical), f.u32(provider));
        assert_eq!(f.u32(canonical_released), 1);
        assert_eq!(f.u32(root), f.u32(root_again));
        assert_eq!(f.u32(again_released), 1);
        assert_eq!(f.u32(child_count), 1);
        assert_eq!(f.u32(prop_count), 3);
        assert_eq!(f.u32(small_name), 0x8007007a);
        assert_eq!(f.read(too_small, 8), [0xdd; 8]);
        assert_eq!(
            f.read(enum_name, 36),
            "DxDiag_SystemInfo\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let expected: Vec<_> = "szDirectXVersionLetter\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(f.read(enum_prop, expected.len()), expected);
        assert_eq!(f.u32(variant_tags[0]), 19);
        assert_eq!(f.u32(variant_values[0]), 9);
        assert_eq!(f.u32(variant_tags[1]), 19);
        assert_eq!(f.u32(variant_values[1]), 0);
        assert_eq!(f.u32(variant_tags[2]), 8);
        assert_eq!(f.u32(string_length), 1);
        assert_eq!(f.u32(letter), u32::from(b'c'));
        assert_eq!(f.u32(unknown_prop), 0x80070057);
        assert_eq!(f.u32(bad_output), 0x80004003);
        assert_eq!(f.u32(invalid_child), 0x80070057);
        assert_eq!(f.u32(rejected), 0);
        assert_eq!(f.read(invalid_variant, 16), [0; 16]);
        for (index, variant) in variants.into_iter().enumerate() {
            // VariantClear releases ownership and empties vt; native Windows
            // retains the former payload, including a freed BSTR's address.
            let mut expected = [0; 16];
            expected[8..12].copy_from_slice(&f.u32(variant_values[index]).to_le_bytes());
            assert_eq!(f.read(variant, 16), expected);
        }
        let bstr = f.u32(variant_values[2]);
        let env = Env {
            cpu: &mut f.machine.cpu,
            sys: &mut f.machine.sys,
            ext: &mut f.machine.ext,
            context: 0,
            thunk: 0,
        };
        assert!(!adv_os::com::owns_bstr(&env, bstr));
        let interfaces = [f.u32(provider), f.u32(root), f.u32(system)];
        let heaps = f.machine.ext.get_mut::<adv_os::Heaps>();
        assert_eq!(heaps.size(&f.machine.cpu, 0, bstr - 4), None);
        for pointer in interfaces {
            assert_eq!(
                heaps.size(&f.machine.cpu, heaps.process_heap, pointer),
                None
            );
        }
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}

#[test]
fn missing_fronts_and_invalid_requests_fail_without_inventing_a_runtime_or_leaking_objects() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit, false);
        let before = f.machine.sys.objects.object_count();
        let params = f.params(111, 0);
        let bad_version = f.params(110, 0);
        let reserved = f.params(111, 1);
        let root = f.alloc(4);
        let system = f.alloc(4);
        let count = f.alloc(4);
        let name = f.wide("DxDiag_SystemInfo");
        let version = f.wide("dwDirectXVersionMajor");
        let variant = f.bytes(&[0xcc; 16]);
        let absent = f.alloc(4);
        let mut a = Asm::new(CODE);
        let (provider, created) = f.provider(&mut a);
        let invalid_header = f.method(&mut a, provider, 3, &[I(bad_version)]);
        let invalid_reserved = f.method(&mut a, provider, 3, &[I(reserved)]);
        let invalid_memory = f.method(&mut a, provider, 3, &[I(0xdead0000)]);
        let uninitialized = f.method(&mut a, provider, 4, &[I(root)]);
        let initialized = f.method(&mut a, provider, 3, &[I(params)]);
        let root_result = f.method(&mut a, provider, 4, &[I(root)]);
        let child_result = f.method(&mut a, root, 5, &[I(name), I(system)]);
        let unavailable = f.method(&mut a, system, 8, &[I(version), I(variant)]);
        let counted = f.method(&mut a, system, 6, &[I(count)]);
        let invalid_index = f.method(&mut a, system, 7, &[I(0), I(absent), I(2)]);
        let provider_release = f.method(&mut a, provider, 2, &[]);
        let root_release = f.method(&mut a, root, 2, &[]);
        let system_release = f.method(&mut a, system, 2, &[]);
        f.run(a);
        for output in [
            created,
            initialized,
            root_result,
            child_result,
            counted,
            provider_release,
            root_release,
            system_release,
        ] {
            assert_eq!(f.u32(output), 0);
        }
        for output in [invalid_header, invalid_reserved, invalid_index] {
            assert_eq!(f.u32(output), 0x80070057);
        }
        assert_eq!(f.u32(invalid_memory), 0x80004003);
        assert_eq!(f.u32(uninitialized), 0x800401f0);
        assert_eq!(f.u32(unavailable), 0x80004001);
        assert_eq!(f.u32(count), 0);
        assert_eq!(f.read(variant, 16), [0; 16]);
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}
