//! Authored x86 guests exercise real USER aliases, resource bytes and callbacks.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const PROC: u32 = CODE + 0x8000;
const DATA: u32 = 0x410000;
const LOG: u32 = DATA + 0x8000;

#[derive(Clone, Copy)]
enum Arg {
    Immediate(u32),
    Memory(u32),
}
use Arg::{Immediate as I, Memory as M};

struct Fixture {
    machine: Machine<JitCpu>,
    data: u32,
}
impl Fixture {
    fn new(jit: bool) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut cfg = Win32Config::new(
            "/nonexistent/accelerator-probe",
            "accelerator-probe.exe",
            "/nonexistent/accelerator-probe-data",
        );
        cfg.os.codepage = 932;
        Win32::install(&mut machine, cfg).unwrap();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.write_u32(DATA, LOG).unwrap();
        let registry = (machine.sys.thunks.resolver() as &dyn std::any::Any)
            .downcast_ref::<Registry>()
            .unwrap();
        assert!(registry.signature_conflicts().is_empty());
        Self {
            machine,
            data: DATA + 0x100,
        }
    }

    fn alloc(&mut self, size: u32) -> u32 {
        let address = self.data;
        self.data += size.next_multiple_of(4);
        assert!(self.data < LOG);
        address
    }

    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let address = self.alloc(bytes.len() as u32);
        self.machine.cpu.write(address, bytes).unwrap();
        address
    }

    fn words(&mut self, words: &[u32]) -> u32 {
        self.bytes(
            &words
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>(),
        )
    }

    fn call(&mut self, code: &mut Asm, api: &str, args: &[Arg]) -> u32 {
        let (dll, name) = api.split_once('!').unwrap_or(("user32.dll", api));
        let entry = self.machine.resolve_import(dll, name).unwrap();
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
        let output = self.alloc(4);
        code.call(entry).mov_mem_eax(output);
        output
    }

    fn run(&mut self, code: Asm) {
        self.run_with_input(code, &[]);
    }

    fn run_with_input(&mut self, mut code: Asm, input: &[(u32, u32)]) {
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        assert!(code.here() < PROC);
        self.machine.cpu.write(CODE, &code.bytes).unwrap();
        self.machine
            .spawn("accelerator probe", CODE, &[], 0x10000)
            .unwrap();
        // Use the real physical-input queue path. PostMessage deliberately does
        // not change GetKeyState, and cannot manufacture accelerator modifiers.
        let mut env = adv_machine::Env {
            context: self.machine.sys.current,
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk: 0,
        };
        for &(message, key) in input {
            assert!(adv_win32::user32::message::post_input(
                &mut env, 0, message, key, 0
            ));
        }
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    fn u32(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }

    fn read(&self, address: u32, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }

    fn window(&mut self, code: &mut Asm, style: u32) -> u32 {
        self.window_with(code, style, |_, _| {})
    }

    fn window_with(
        &mut self,
        code: &mut Asm,
        style: u32,
        body: impl FnOnce(&mut Self, &mut Asm),
    ) -> u32 {
        // Log all four WNDPROC arguments, preserving the registers needed by x86.
        let mut proc = Asm::new(PROC);
        proc.push_ecx().mov_ecx_imm(DATA);
        proc.bytes.extend_from_slice(&[0x8b, 0x09]); // mov ecx,[ecx]
        for (offset, destination) in [(8, 0), (12, 4), (16, 8), (20, 12)] {
            proc.mov_eax_esp(offset);
            proc.bytes.extend_from_slice(&[0x89, 0x41, destination]);
        }
        proc.bytes.extend_from_slice(&[0x83, 0xc1, 16, 0x89, 0x0d]);
        proc.bytes.extend_from_slice(&DATA.to_le_bytes());
        proc.pop_ecx();
        body(self, &mut proc);
        let default = self
            .machine
            .resolve_import("user32.dll", "DefWindowProcA")
            .unwrap();
        proc.jmp(default);
        self.machine.cpu.write(PROC, &proc.bytes).unwrap();
        let name = self.bytes(b"AcceleratorProbe\0");
        let class = self.words(&[0, PROC, 0, 0, CODE, 0, 0, 0, 0, name]);
        self.call(code, "RegisterClassA", &[I(class)]);
        self.call(
            code,
            "CreateWindowExA",
            &[
                I(0),
                I(name),
                I(0),
                I(style),
                I(0),
                I(0),
                I(100),
                I(80),
                I(0),
                I(0),
                I(CODE),
                I(0),
            ],
        )
    }

    fn messages(&self) -> Vec<[u32; 4]> {
        (LOG..self.u32(DATA))
            .step_by(16)
            .map(|address| std::array::from_fn(|index| self.u32(address + index as u32 * 4)))
            .collect()
    }

    fn resource(&mut self, bytes: &[u8], name: &str) -> u32 {
        use adv_os::module::{Lifetime, Module, ModuleKind, Modules};
        use adv_pe::{ResId, ResourceData, ResourceDir, ResourceEntry, ResourceNode};
        const BASE: u32 = 0x600000;
        self.machine.cpu.map(BASE, 0x10000, Prot::READ).unwrap();
        self.machine.cpu.write(BASE + 0x2000, bytes).unwrap();
        self.machine.cpu.write_u32(BASE + 0x1000, 0x2000).unwrap();
        self.machine
            .cpu
            .write_u32(BASE + 0x1004, bytes.len() as u32)
            .unwrap();
        let language = ResourceNode::Dir(ResourceDir {
            entries: vec![ResourceEntry {
                id: ResId::Id(0x409),
                node: ResourceNode::Data(ResourceData {
                    entry_rva: 0x1000,
                    rva: 0x2000,
                    size: bytes.len() as u32,
                    codepage: 0,
                }),
            }],
        });
        let resources = ResourceDir {
            entries: vec![ResourceEntry {
                id: ResId::Id(9),
                node: ResourceNode::Dir(ResourceDir {
                    entries: vec![
                        ResourceEntry {
                            id: ResId::Id(101),
                            node: language.clone(),
                        },
                        ResourceEntry {
                            id: ResId::Name(name.into()),
                            node: language,
                        },
                    ],
                }),
            }],
        };
        self.machine.ext.get_mut::<Modules>().add(Module {
            name: "accelerator-resource.dll".into(),
            path: "accelerator-resource.dll".into(),
            kind: ModuleKind::Image,
            base: BASE,
            size: 0x10000,
            is_dll: true,
            entry: 0,
            stack_reserve: 0,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: Some(resources),
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime::default(),
        });
        BASE
    }
}

#[test]
fn menu_initialization_can_disable_commands_and_system_routing_uses_its_own_parameters() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let mut a = Asm::new(CODE);
        let root = f.call(&mut a, "CreateMenu", &[]);
        let popup = f.call(&mut a, "CreatePopupMenu", &[]);
        let label = f.bytes(b"Command\0");
        f.call(&mut a, "AppendMenuA", &[M(popup), I(0), I(500), I(label)]);
        f.call(
            &mut a,
            "AppendMenuA",
            &[M(root), I(0x10), M(popup), I(label)],
        );
        let hwnd = f.window_with(&mut a, 0x80080000, |f, proc| {
            proc.mov_eax_esp(8);
            proc.bytes.extend_from_slice(&[0x3d, 0x17, 0x01, 0, 0]); // WM_INITMENUPOPUP
            let skip_message = proc.bytes.len();
            proc.jnz(0);
            proc.cmp_mem_imm8(DATA + 4, 1);
            let skip_mode = proc.bytes.len();
            proc.jnz(0);
            f.call(proc, "EnableMenuItem", &[M(popup), I(500), I(3)]);
            let end = proc.here();
            for index in [skip_message, skip_mode] {
                let next = PROC + index as u32 + 6;
                proc.bytes[index + 2..index + 6]
                    .copy_from_slice(&end.wrapping_sub(next).to_le_bytes());
            }
        });
        f.call(&mut a, "SetMenu", &[M(hwnd), M(root)]);
        let input = f.bytes(
            &[
                accel(1, b'N' as u16, 500),
                accel(1, b'X' as u16, 777),
                accel(1, b'C' as u16, 0xf060),
            ]
            .concat(),
        );
        let table = f.call(&mut a, "CreateAcceleratorTableW", &[I(input), I(3)]);
        let msg = f.words(&[0, 0x100, b'N' as u32, 0, 0, 0, 0]);
        a.mov_mem_imm(DATA, LOG);
        let enabled = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        f.call(&mut a, "EnableWindow", &[M(hwnd), I(0)]);
        let disabled_window = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        a.mov_mem_imm(msg + 8, b'X' as u32);
        let independent = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        f.call(&mut a, "EnableWindow", &[M(hwnd), I(1)]);
        f.call(&mut a, "EnableMenuItem", &[M(popup), I(500), I(3)]);
        a.mov_mem_imm(msg + 8, b'N' as u32);
        let disabled_item = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        f.call(&mut a, "EnableMenuItem", &[M(popup), I(500), I(0)]);
        a.mov_mem_imm(DATA + 4, 1);
        let callback_disabled = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        a.mov_mem_imm(DATA + 4, 0).mov_mem_imm(msg + 8, b'C' as u32);
        let system = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        f.call(&mut a, "DestroyAcceleratorTable", &[M(table)]);
        f.run(a);
        for result in [
            enabled,
            disabled_window,
            independent,
            disabled_item,
            callback_disabled,
            system,
        ] {
            assert_eq!(f.u32(result), 1);
        }
        let routed: Vec<_> = f
            .messages()
            .into_iter()
            .filter(|message| matches!(message[1], 0x111 | 0x112 | 0x116 | 0x117))
            .collect();
        assert_eq!(routed.len(), 13);
        assert_eq!(
            &routed[..3],
            &[
                [f.u32(hwnd), 0x116, f.u32(root), 0],
                [f.u32(hwnd), 0x117, f.u32(popup), 0],
                [f.u32(hwnd), 0x111, 0x101f4, 0],
            ]
        );
        assert_eq!(
            routed.iter().filter(|message| message[1] == 0x116).count(),
            5
        );
        assert_eq!(
            routed.iter().filter(|message| message[1] == 0x117).count(),
            5
        );
        let commands: Vec<_> = routed
            .iter()
            .filter(|message| matches!(message[1], 0x111 | 0x112))
            .copied()
            .collect();
        assert_eq!(
            commands,
            vec![
                [f.u32(hwnd), 0x111, 0x101f4, 0],
                [f.u32(hwnd), 0x111, 0x10309, 0],
                [f.u32(hwnd), 0x112, 0xf060, 0x10000],
            ]
        );
    }
}

fn accel(flags: u8, key: u16, command: u16) -> [u8; 6] {
    let key = key.to_le_bytes();
    let command = command.to_le_bytes();
    [flags, 0, key[0], key[1], command[0], command[1]]
}

#[test]
fn dynamic_tables_copy_guest_bytes_and_retire_the_real_object() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let before = f.machine.sys.objects.object_count();
        let input = f.bytes(
            &[
                accel(0x89, b'K' as u16, 0x1234),
                accel(0x80, 0x3042, 0x4321),
            ]
            .concat(),
        );
        let output = f.bytes(&[0xcc; 18]);
        let ansi_output = f.alloc(12);
        let ansi_input = f.bytes(&[accel(0, 0x0141, 500), accel(0, 0xa082, 501)].concat());
        let mut a = Asm::new(CODE);
        f.call(&mut a, "kernel32.dll!SetLastError", &[I(0x76543210)]);
        let table = f.call(&mut a, "CreateAcceleratorTableW", &[I(input), I(2)]);
        let created_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        // The input remains owned by the guest; modifying it cannot change the table.
        a.mov_mem_imm(input, 0);
        let count = f.call(&mut a, "CopyAcceleratorTableA", &[M(table), I(0), I(0)]);
        let partial = f.call(
            &mut a,
            "CopyAcceleratorTableW",
            &[M(table), I(output), I(1)],
        );
        let full = f.call(
            &mut a,
            "CopyAcceleratorTableW",
            &[M(table), I(output), I(3)],
        );
        let failed_copy = f.call(&mut a, "CopyAcceleratorTableW", &[M(table), I(CODE), I(1)]);
        let ansi_copy = f.call(
            &mut a,
            "CopyAcceleratorTableA",
            &[M(table), I(ansi_output), I(2)],
        );
        let ansi_table = f.call(&mut a, "CreateAcceleratorTableA", &[I(ansi_input), I(2)]);
        let ansi_roundtrip = f.call(
            &mut a,
            "CopyAcceleratorTableW",
            &[M(ansi_table), I(ansi_input), I(2)],
        );
        f.call(&mut a, "DestroyAcceleratorTable", &[M(ansi_table)]);
        let copy_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let destroyed = f.call(&mut a, "DestroyAcceleratorTable", &[M(table)]);
        let stale = f.call(&mut a, "CopyAcceleratorTableW", &[M(table), I(0), I(0)]);
        let stale_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let bad_count = f.call(&mut a, "CreateAcceleratorTableA", &[I(input), I(0)]);
        let count_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let bad_pointer = f.call(&mut a, "CreateAcceleratorTableW", &[I(0xdead0000), I(1)]);
        let pointer_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        f.call(&mut a, "kernel32.dll!SetLastError", &[I(0x76543210)]);
        let null_input = f.call(&mut a, "CreateAcceleratorTableW", &[I(0), I(1)]);
        let null_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        f.run(a);
        assert_ne!(f.u32(table), 0);
        assert_eq!(f.u32(created_error), 0x76543210);
        for (result, expected) in [
            (count, 2),
            (partial, 1),
            (full, 2),
            (failed_copy, 0),
            (copy_error, 87),
            (destroyed, 1),
            (stale, 0),
            (stale_error, 1403),
            (bad_count, 0),
            (count_error, 87),
            (bad_pointer, 0),
            (pointer_error, 87),
            (ansi_copy, 2),
            (ansi_roundtrip, 2),
            (null_input, 0),
            (null_error, 0x76543210),
        ] {
            assert_eq!(f.u32(result), expected);
        }
        assert_eq!(
            f.read(output, 12),
            [accel(9, b'K' as u16, 0x1234), accel(0, 0x3042, 0x4321)].concat()
        );
        assert_eq!(f.read(output + 12, 6), [0xcc; 6]);
        assert_eq!(
            f.read(ansi_output, 12),
            [accel(9, b'K' as u16, 0x1234), accel(0, 0xa082, 0x4321)].concat()
        );
        assert_eq!(
            f.read(ansi_input, 12),
            [accel(0, b'A' as u16, 500), accel(0, 0, 501)].concat()
        );
        // Machine::spawn retains one thread object after guest exit.
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}

#[test]
fn resource_tables_use_integer_and_unicode_names_and_shared_lifetime() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let resource = [
            9, 0, b'O', 0, 0x90, 1, 0, 0, 0x80, 0, 0x42, 0x30, 0x91, 1, 0, 0,
        ];
        let instance = f.resource(&resource, "快捷🌸");
        let name = f.bytes(
            &"快捷🌸\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let output = f.alloc(12);
        let before = f.machine.sys.objects.object_count();
        let mut a = Asm::new(CODE);
        let first = f.call(&mut a, "LoadAcceleratorsA", &[I(instance), I(101)]);
        let second = f.call(&mut a, "LoadAcceleratorsW", &[I(instance), I(name)]);
        let copied = f.call(
            &mut a,
            "CopyAcceleratorTableW",
            &[M(first), I(output), I(2)],
        );
        let missing = f.call(&mut a, "LoadAcceleratorsW", &[I(instance), I(999)]);
        let missing_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        let release_one = f.call(&mut a, "DestroyAcceleratorTable", &[M(first)]);
        let retained = f.call(&mut a, "CopyAcceleratorTableW", &[M(second), I(0), I(0)]);
        let release_last = f.call(&mut a, "DestroyAcceleratorTable", &[M(second)]);
        let stale = f.call(&mut a, "DestroyAcceleratorTable", &[M(second)]);
        let stale_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        f.run(a);
        assert_ne!(f.u32(first), 0);
        assert_eq!(f.u32(first), f.u32(second));
        assert_eq!(f.u32(copied), 2);
        assert_eq!(
            f.read(output, 12),
            [accel(9, b'O' as u16, 400), accel(0, 0x3042, 401)].concat()
        );
        for (result, expected) in [
            (missing, 0),
            (missing_error, 1814),
            (release_one, 0),
            (retained, 2),
            (release_last, 1),
            (stale, 0),
            (stale_error, 1403),
        ] {
            assert_eq!(f.u32(result), expected);
        }
        assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    }
}

#[test]
fn translation_matches_queue_modifiers_and_sends_commands_synchronously() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let input = f.bytes(
            &[
                accel(9, b'K' as u16, 0x1234),
                accel(5, b'S' as u16, 0x2345),
                accel(17, b'A' as u16, 0x3456),
                accel(0, 0x3042, 0x4567),
                accel(9, b'K' as u16, 999), // First matching entry wins.
            ]
            .concat(),
        );
        let msg = f.words(&[0, 0x100, b'K' as u32, 0, 0, 0, 0]);
        let dequeued = f.alloc(28);
        let mut a = Asm::new(CODE);
        let hwnd = f.window(&mut a, 0x80000000);
        let table = f.call(&mut a, "CreateAcceleratorTableW", &[I(input), I(5)]);
        let no_ctrl = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        let mut translated = Vec::new();
        for (key, kind, command) in [
            (b'K' as u32, 0x100, 0x1234),
            (b'S' as u32, 0x100, 0x2345),
            (b'A' as u32, 0x104, 0x3456),
        ] {
            f.call(
                &mut a,
                "PeekMessageA",
                &[I(dequeued), I(0), I(0x100), I(0x100), I(1)],
            );
            a.mov_mem_imm(msg + 4, kind).mov_mem_imm(msg + 8, key);
            let result = f.call(
                &mut a,
                "TranslateAcceleratorW",
                &[M(hwnd), M(table), I(msg)],
            );
            // Callback recording must already have happened before the call returns.
            let log_end = f.alloc(4);
            a.mov_eax_mem(DATA).mov_mem_eax(log_end);
            translated.push((result, log_end, command));
            f.call(
                &mut a,
                "PeekMessageA",
                &[I(dequeued), I(0), I(0x101), I(0x101), I(1)],
            );
        }
        a.mov_mem_imm(msg + 4, 0x102).mov_mem_imm(msg + 8, 0x3042);
        let character = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        a.mov_mem_imm(msg + 8, 0x82a0)
            .mov_mem_imm(msg + 12, 1 << 29);
        let ansi_character = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        // The original MSG is caller-owned and must not be marshalled in place.
        let unchanged_character = f.alloc(4);
        a.mov_eax_mem(msg + 8).mov_mem_eax(unchanged_character);
        a.mov_mem_imm(msg + 4, 0x106).mov_mem_imm(msg + 8, 0xa082);
        let ansi_system_character = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        let unchanged_system_character = f.alloc(4);
        a.mov_eax_mem(msg + 8)
            .mov_mem_eax(unchanged_system_character);
        a.mov_mem_imm(msg + 4, 0x102);
        let wrong_character_order = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        a.mov_mem_imm(msg + 4, 0x106).mov_mem_imm(msg + 8, 0x82a0);
        let wrong_system_order = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        a.mov_mem_imm(msg + 4, 0x101)
            .mov_mem_imm(msg + 8, b'K' as u32);
        let key_up = f.call(
            &mut a,
            "TranslateAcceleratorA",
            &[M(hwnd), M(table), I(msg)],
        );
        let bad_msg = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(0xdead0000)],
        );
        let bad_msg_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        f.call(&mut a, "DestroyAcceleratorTable", &[M(table)]);
        let stale = f.call(
            &mut a,
            "TranslateAcceleratorW",
            &[M(hwnd), M(table), I(msg)],
        );
        let stale_error = f.call(&mut a, "kernel32.dll!GetLastError", &[]);
        f.run_with_input(
            a,
            &[
                (0x100, 0x11),
                (0x101, 0x11),
                (0x100, 0x10),
                (0x101, 0x10),
                (0x100, 0x12),
                (0x101, 0x12),
            ],
        );
        assert_eq!(f.u32(no_ctrl), 0);
        let commands: Vec<_> = f
            .messages()
            .into_iter()
            .filter(|message| message[1] == 0x111)
            .collect();
        assert_eq!(commands.len(), 6);
        for (index, (result, end, command)) in translated.into_iter().enumerate() {
            assert_eq!(f.u32(result), 1);
            assert_eq!(
                f.u32(end),
                LOG + (f
                    .messages()
                    .iter()
                    .position(|message| message[1] == 0x111 && message[2] == (0x10000 | command))
                    .unwrap() as u32
                    + 1)
                    * 16
            );
            assert_eq!(commands[index], [f.u32(hwnd), 0x111, 0x10000 | command, 0]);
        }
        assert_eq!(commands[3..], [[f.u32(hwnd), 0x111, 0x14567, 0]; 3]);
        for (result, expected) in [
            (character, 1),
            (ansi_character, 1),
            (ansi_system_character, 1),
            (wrong_character_order, 0),
            (wrong_system_order, 0),
            (unchanged_character, 0x82a0),
            (unchanged_system_character, 0xa082),
            (key_up, 0),
            (bad_msg, 0),
            (bad_msg_error, 87),
            (stale, 0),
            (stale_error, 1403),
        ] {
            assert_eq!(f.u32(result), expected);
        }
    }
}
