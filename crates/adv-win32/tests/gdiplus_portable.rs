//! Guest flat calls exercise owned images, vector coverage and value queries.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const REF: u32 = 0x7fab_0000;
#[path = "support/test_cpu.rs"]
mod test_cpu;
use test_cpu::TestCpu;
#[path = "gdiplus_portable/brush.rs"]
mod brush;
#[path = "gdiplus_portable/curves.rs"]
mod curves;
#[path = "gdiplus_portable/fill.rs"]
mod fill;
#[path = "gdiplus_portable/matrix.rs"]
mod matrix;
#[path = "gdiplus_portable/matrix_nonfinite.rs"]
mod matrix_nonfinite;
#[path = "gdiplus_portable/path.rs"]
mod path;
#[path = "gdiplus_portable/string_format.rs"]
mod string_format;
#[path = "gdiplus_portable/world.rs"]
mod world;

struct Fixture<B: TestCpu> {
    machine: Machine<B>,
    code: Asm,
    results: Vec<(&'static str, u32)>,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gdiplus-tests");
        Win32::install(&mut machine, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.write_u32(DATA + 256, 1).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            results: Vec::new(),
        }
    }
    fn call(&mut self, name: &'static str, args: &[u32], expected: u32) {
        for &arg in args.iter().rev() {
            if arg & 0xffff_0000 == REF {
                self.code.mov_eax_mem(DATA + (arg & 0xffff)).push_eax();
            } else {
                self.code.push_imm(arg);
            }
        }
        self.code
            .call(self.machine.resolve_import("gdiplus.dll", name).unwrap())
            .mov_mem_eax(DATA + 2048 + self.results.len() as u32 * 4);
        self.results.push((name, expected));
    }
    fn run(mut self) -> Machine<B> {
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.code_region(CODE, self.code.bytes.len());
        self.machine
            .spawn("GDI+ fixture", CODE, &[], 65536)
            .unwrap();
        let outcome = self.machine.run_virtual(1_000_000);
        assert!(matches!(outcome, RunResult::Exited(_)), "{outcome:?}");
        for (i, (api, expected)) in self.results.iter().enumerate() {
            assert_eq!(
                self.machine
                    .cpu
                    .read_u32(DATA + 2048 + i as u32 * 4)
                    .unwrap(),
                *expected,
                "{api} call {i}"
            );
        }
        assert!(
            self.machine
                .ext
                .try_get::<ApiDiagnostics>()
                .is_none_or(|diagnostics| diagnostics.warnings.is_empty())
        );
        self.machine
    }
}

fn pixels<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[4, 4, 16, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipGraphicsClear", &[REF | 8, 0xff0000ff], 0);
    f.call("GdipCreateSolidFill", &[0x80ff0000, DATA + 12], 0);
    f.call("GdipCreatePath", &[1, DATA + 16], 0);
    for points in [[1, 1, 3, 1], [3, 1, 3, 3], [3, 3, 1, 3]] {
        f.call(
            "GdipAddPathLineI",
            &[REF | 16, points[0], points[1], points[2], points[3]],
            0,
        );
    }
    f.call("GdipClosePathFigure", &[REF | 16], 0);
    f.call("GdipClonePath", &[REF | 16, DATA + 20], 0);
    f.call("GdipResetPath", &[REF | 16], 0);
    f.call("GdipGetPointCount", &[REF | 16, DATA + 800], 0);
    f.call("GdipGetPointCount", &[REF | 20, DATA + 804], 0);
    f.call("GdipGetPathFillMode", &[REF | 16, DATA + 808], 0);
    f.call("GdipGetPathFillMode", &[REF | 20, DATA + 812], 0);
    f.call("GdipGetPathWorldBounds", &[REF | 20, DATA + 816, 0, 0], 0);
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 20], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 1, 1, DATA + 832], 0);
    f.call(
        "GdipBitmapGetPixel",
        &[REF | 4, (-1i32) as u32, 0, DATA + 836],
        2,
    );
    f.call("GdipBitmapSetPixel", &[REF | 4, 3, 3, 0xff00ff00], 0);
    f.call("GdipCloneImage", &[REF | 4, DATA + 24], 0);
    f.call("GdipDisposeImage", &[REF | 4], 0);
    // The live Graphics retains the original borrowed image after alias disposal.
    f.call("GdipGraphicsClear", &[REF | 8, 0x80402010], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 1, 1, DATA + 836], 2);
    f.call("GdipBitmapGetPixel", &[REF | 24, 1, 1, DATA + 840], 0);
    f.call("GdipBitmapGetPixel", &[REF | 24, 3, 3, DATA + 844], 0);
    f.call("GdipBitmapGetPixel", &[REF | 24, 0, 0, DATA + 848], 0);
    f.call(
        "GdipGetImageDimension",
        &[REF | 24, DATA + 852, DATA + 856],
        0,
    );
    f.call("GdipGetImageBounds", &[REF | 24, DATA + 860, DATA + 876], 0);
    f.call("GdipGetImageType", &[REF | 24, DATA + 880], 0);
    f.call("GdipDeleteGraphics", &[REF | 8], 0);
    f.call("GdipDeletePath", &[REF | 16], 0);
    f.call("GdipDeletePath", &[REF | 20], 0);
    f.call("GdipDeleteBrush", &[REF | 12], 0);
    f.call("GdipDisposeImage", &[REF | 24], 0);
    f.call("GdiplusShutdown", &[REF], 0);
    let m = f.run();
    for (offset, value) in [
        (800, 0),
        (804, 4),
        (808, 0),
        (812, 1),
        (832, 0xff80007f),
        (840, 0xff80007f),
        (844, 0xff00ff00),
        (848, 0xff0000ff),
        (852, 4f32.to_bits()),
        (856, 4f32.to_bits()),
        (876, 2),
        (880, 1),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            value,
            "output {offset}"
        );
    }
    for (i, value) in [1f32, 1., 2., 2.].iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 816 + i as u32 * 4).unwrap(),
            value.to_bits()
        );
    }
    for (i, value) in [0f32, 0., 4., 4.].iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 860 + i as u32 * 4).unwrap(),
            value.to_bits()
        );
    }
    for i in 0..16 {
        assert_eq!(m.cpu.read_u32(DATA + 512 + i * 4).unwrap(), 0x80402010);
    }
}

fn formats<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call("GdipCreateStringFormat", &[0x1000, 0x409, DATA + 4], 0);
    f.call("GdipSetStringFormatAlign", &[REF | 4, 2], 0);
    f.call("GdipSetStringFormatLineAlign", &[REF | 4, 1], 0);
    f.call("GdipCloneStringFormat", &[REF | 4, DATA + 8], 0);
    f.call("GdipSetStringFormatFlags", &[REF | 4, 0x2000], 0);
    f.call("GdipSetStringFormatAlign", &[REF | 4, 0], 0);
    f.call("GdipSetStringFormatAlign", &[REF | 8, 3], 2);
    f.call("GdipGetStringFormatFlags", &[REF | 8, DATA + 800], 0);
    f.call("GdipGetStringFormatAlign", &[REF | 8, DATA + 804], 0);
    f.call("GdipGetStringFormatLineAlign", &[REF | 8, DATA + 808], 0);
    f.call("GdipGetStringFormatFlags", &[REF | 4, DATA + 812], 0);
    f.call("GdipCreatePath", &[1, DATA + 12], 0);
    f.call("GdipSetPathFillMode", &[REF | 12, 0], 0);
    f.call("GdipSetPathFillMode", &[REF | 12, 3], 2);
    f.call("GdipGetPathFillMode", &[REF | 12, DATA + 816], 0);
    f.call("GdipDeletePath", &[REF | 12], 0);
    f.call("GdipDeleteStringFormat", &[REF | 4], 0);
    f.call("GdipDeleteStringFormat", &[REF | 8], 0);
    f.call("GdiplusShutdown", &[REF], 0);
    let m = f.run();
    for (offset, value) in [(800, 0x1000), (804, 2), (808, 1), (812, 0x2000), (816, 0)] {
        assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), value);
    }
}

fn coverage<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[4, 4, 16, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipCreateSolidFill", &[0xffff0000, DATA + 12], 0);
    f.call("GdipCreatePath", &[0, DATA + 16], 0);
    for p in [[1, 1, 2, 1], [2, 1, 2, 2], [2, 2, 1, 2]] {
        f.call("GdipAddPathLineI", &[REF | 16, p[0], p[1], p[2], p[3]], 0);
    }
    f.call("GdipClosePathFigure", &[REF | 16], 0);
    f.call("GdipSetSmoothingMode", &[REF | 8, 4], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 3], 0);
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
    for (i, (x, y)) in [(1, 1), (2, 1), (1, 2), (2, 2)].into_iter().enumerate() {
        f.call(
            "GdipBitmapGetPixel",
            &[REF | 4, x, y, DATA + 800 + i as u32 * 4],
            0,
        );
    }
    f.call("GdipGraphicsClear", &[REF | 8, 0], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 4], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 5], 2);
    f.call("GdipGetPixelOffsetMode", &[REF | 8, DATA + 848], 0);
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
    for (i, (x, y)) in [(1, 1), (2, 1), (1, 2), (2, 2)].into_iter().enumerate() {
        f.call(
            "GdipBitmapGetPixel",
            &[REF | 4, x, y, DATA + 816 + i as u32 * 4],
            0,
        );
    }
    f.call("GdipGraphicsClear", &[REF | 8, 0], 0);
    f.call("GdipSetSmoothingMode", &[REF | 8, 3], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 3], 0);
    f.call("GdipResetPath", &[REF | 16], 0);
    f.call(
        "GdipAddPathArc",
        &[
            REF | 16,
            0.75f32.to_bits(),
            0.75f32.to_bits(),
            2.5f32.to_bits(),
            2.5f32.to_bits(),
            0,
            360f32.to_bits(),
        ],
        0,
    );
    f.call("GdipClosePathFigure", &[REF | 16], 0);
    f.call("GdipGetPointCount", &[REF | 16, DATA + 852], 0);
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
    let m = f.run();
    // Integer centers divide a one-unit square over four quarter-covered pixels.
    for i in 0..4 {
        assert_eq!(m.cpu.read_u32(DATA + 800 + i * 4).unwrap(), 0x40ff0000);
    }
    // Half-integer centers place the same square on exactly one whole pixel.
    for (i, value) in [0xffff0000, 0, 0, 0].into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + 816 + i as u32 * 4).unwrap(), value);
    }
    assert_eq!(m.cpu.read_u32(DATA + 848).unwrap(), 4);
    assert_eq!(m.cpu.read_u32(DATA + 852).unwrap(), 13);
    let expected = [
        0, 0, 0, 0, 0, 0, 0xffff0000, 0, 0, 0xffff0000, 0xffff0000, 0xffff0000, 0, 0, 0xffff0000, 0,
    ];
    for (i, color) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 512 + i as u32 * 4).unwrap(),
            color,
            "ellipse pixel {i}"
        );
    }
}

#[test]
// spec: DEV-IMAGE-2 DEV-IMAGE-5 DEV-IMAGE-6
fn bitmap_pixels_paths_and_backing_lifetimes() {
    pixels(JitCpu::new(PARK_ADDR, false).unwrap());
    pixels(JitCpu::new(PARK_ADDR, true).unwrap());
    pixels(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
// spec: DEV-IMAGE-6
fn path_modes_and_format_clones_retain_independent_state() {
    formats(JitCpu::new(PARK_ADDR, false).unwrap());
    formats(JitCpu::new(PARK_ADDR, true).unwrap());
    formats(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
// spec: DEV-IMAGE-5 DEV-IMAGE-7
fn antialias_pixel_centers_and_aliased_curve_coverage() {
    coverage(JitCpu::new(PARK_ADDR, false).unwrap());
    coverage(JitCpu::new(PARK_ADDR, true).unwrap());
    coverage(UnicornCpu::new(PARK_ADDR).unwrap());
}
