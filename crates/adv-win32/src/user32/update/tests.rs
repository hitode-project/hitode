//! Guest region calls keep disjoint damage exact and roll back bounded failures.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
struct Fixture {
    machine: Machine<JitCpu>,
    code: Asm,
    next: u32,
    hwnd: u32,
}
impl Fixture {
    fn new(jit: bool, width: i32) -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/update-region-tests");
        Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "fixture.exe", &root),
        )
        .unwrap();
        machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let hwnd = machine.ext.get::<Win32>().user.desktop;
        let window = machine
            .ext
            .get::<Win32>()
            .user
            .windows
            .get_mut(&mut machine.sys.objects, &hwnd)
            .unwrap();
        window.rect = super::super::window::Rect::new(0, 0, width, 4);
        clear(window);
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA + 128,
            hwnd,
        }
    }
    fn call(&mut self, name: &str, args: &[(u32, bool)]) -> u32 {
        let (dll, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        for &(value, indirect) in args.iter().rev() {
            if indirect {
                self.code.mov_eax_mem(value).push_eax();
            } else {
                self.code.push_imm(value);
            }
        }
        let entry = self.machine.resolve_import(dll, name).unwrap();
        let output = self.next;
        self.next += 4;
        self.code.call(entry).mov_mem_eax(output);
        output
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        let start = self.code.here() - self.code.bytes.len() as u32;
        self.machine.cpu.write(start, &self.code.bytes).unwrap();
        self.machine
            .cpu
            .executor_mut()
            .add_region(Region::new(start, self.code.bytes.len()))
            .unwrap();
        self.machine
            .spawn("update region guest", start, &[], 65536)
            .unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        self.code = Asm::new(start + 8192);
    }
    fn value(&self, at: u32) -> u32 {
        self.machine.cpu.read_u32(at).unwrap()
    }
    fn damage(&self) -> Snapshot {
        snapshot(
            self.machine
                .ext
                .get::<Win32>()
                .user
                .windows
                .get(&self.machine.sys.objects, &self.hwnd)
                .unwrap(),
        )
    }
    fn region(&mut self, rect: [u32; 4]) -> u32 {
        self.call("gdi32.dll!CreateRectRgn", &rect.map(|n| (n, false)))
    }
}
#[test]
// spec: WIN32-GDI-2
fn invalidation_and_validation_keep_disjoint_damage_and_do_not_consume_region_objects() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit, 8);
        let a = f.region([0, 0, 2, 4]);
        let b = f.region([4, 0, 6, 4]);
        let cut = f.region([1, 1, 5, 3]);
        let combined = f.call(
            "gdi32.dll!CombineRgn",
            &[(a, true), (a, true), (b, true), (2, false)],
        );
        let added = f.call("InvalidateRgn", &[(f.hwnd, false), (a, true), (1, false)]);
        let removed = f.call("ValidateRgn", &[(f.hwnd, false), (cut, true)]);
        let bounds = f.call(
            "GetUpdateRect",
            &[(f.hwnd, false), (DATA, false), (0, false)],
        );
        let bad = f.call("ValidateRgn", &[(f.hwnd, false), (0xdeadbeef, false)]);
        let error = f.call("kernel32.dll!GetLastError", &[]);
        let mut deleted = Vec::new();
        for r in [a, b, cut] {
            deleted.push(f.call("gdi32.dll!DeleteObject", &[(r, true)]));
        }
        f.run();
        for (result, expected) in [
            (combined, 3),
            (added, 1),
            (removed, 1),
            (bounds, 1),
            (bad, 0),
            (error, 6),
        ] {
            assert_eq!(f.value(result), expected);
        }
        for result in deleted {
            assert_eq!(f.value(result), 1);
        }
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|i| f.value(DATA + i as u32 * 4)),
            [0, 0, 6, 4]
        );
        let damage = f.damage();
        assert!(damage.erase && damage.unerased);
        let expected = [
            [true, true, false, false, true, true, false, false],
            [true, false, false, false, false, true, false, false],
            [true, false, false, false, false, true, false, false],
            [true, true, false, false, true, true, false, false],
        ];
        for (y, row) in expected.into_iter().enumerate() {
            for (x, pending) in row.into_iter().enumerate() {
                assert_eq!(
                    damage.rects.iter().any(|r| x as i32 >= r[0]
                        && (x as i32) < r[2]
                        && y as i32 >= r[1]
                        && (y as i32) < r[3]),
                    pending
                );
            }
        }
        let all = f.call("InvalidateRgn", &[(f.hwnd, false), (0, false), (0, false)]);
        let full = f.call(
            "GetUpdateRect",
            &[(f.hwnd, false), (DATA + 16, false), (0, false)],
        );
        let clear = f.call("ValidateRgn", &[(f.hwnd, false), (0, false)]);
        let clean = f.call(
            "GetUpdateRect",
            &[(f.hwnd, false), (DATA + 32, false), (0, false)],
        );
        f.run();
        assert_eq!(
            (f.value(all), f.value(full), f.value(clear), f.value(clean)),
            (1, 1, 1, 0)
        );
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|i| f.value(DATA + 16 + i as u32 * 4)),
            [0, 0, 8, 4]
        );
        assert!(f.damage().rects.is_empty());
        assert!(
            f.machine
                .ext
                .try_get::<adv_abi::ApiDiagnostics>()
                .is_none_or(|d| d.warnings.is_empty())
        );
    }
}
#[test]
// spec: WIN32-GDI-2
fn a_complex_union_fails_atomically_without_erasing_prior_damage() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit, 140);
        let old = f.region([0, 3, 2, 4]);
        let first = f.call("InvalidateRgn", &[(f.hwnd, false), (old, true), (0, false)]);
        let union = f.region([0, 0, 1, 1]);
        for x in (2..=130).step_by(2) {
            let next = f.region([x, 0, x + 1, 1]);
            f.call(
                "gdi32.dll!CombineRgn",
                &[(union, true), (union, true), (next, true), (2, false)],
            );
            f.call("gdi32.dll!DeleteObject", &[(next, true)]);
        }
        let result = f.call(
            "InvalidateRgn",
            &[(f.hwnd, false), (union, true), (1, false)],
        );
        let error = f.call("kernel32.dll!GetLastError", &[]);
        f.run();
        assert_eq!(
            (f.value(first), f.value(result), f.value(error)),
            (1, 0, 50)
        );
        let damage = f.damage();
        assert_eq!(damage.rects, [[0, 3, 2, 4]]);
        assert!(!damage.erase && !damage.unerased);
        let warnings = &f.machine.ext.get::<adv_abi::ApiDiagnostics>().warnings;
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings
                .keys()
                .any(|(api, reason)| api == "user32.dll!InvalidateRgn"
                    && reason.contains("64 rectangles"))
        );
    }
}
