//! Brush clones retain their colors while subsequent draws observe mutations.
use super::*;
fn colors<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[2, 1, 8, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipCreateSolidFill", &[0xffff0000, DATA + 12], 0);
    f.call("GdipCloneBrush", &[REF | 12, DATA + 16], 0);
    f.call("GdipSetSolidFillColor", &[REF | 12, 0x8000ff00], 0);
    f.call("GdipGetSolidFillColor", &[REF | 12, DATA + 800], 0);
    f.call("GdipGetSolidFillColor", &[REF | 16, DATA + 804], 0);
    f.call("GdipGetBrushType", &[REF | 12, DATA + 808], 0);
    f.call("GdipCreatePath", &[0, DATA + 20], 0);
    for (slot, x) in [(12, 0), (16, 1)] {
        f.call("GdipResetPath", &[REF | 20], 0);
        for p in [[x, 0, x + 1, 0], [x + 1, 0, x + 1, 1], [x + 1, 1, x, 1]] {
            f.call("GdipAddPathLineI", &[REF | 20, p[0], p[1], p[2], p[3]], 0);
        }
        f.call("GdipClosePathFigure", &[REF | 20], 0);
        f.call("GdipFillPath", &[REF | 8, REF | slot, REF | 20], 0);
    }
    f.call("GdipGetBrushType", &[REF | 20, DATA + 812], 2);
    f.call("GdipGetSolidFillColor", &[REF | 20, DATA + 812], 2);
    f.call("GdipSetSolidFillColor", &[REF | 20, 0xffffffff], 2);
    let m = f.run();
    assert_eq!(m.cpu.read_u32(DATA + 800).unwrap(), 0x8000ff00);
    assert_eq!(m.cpu.read_u32(DATA + 804).unwrap(), 0xffff0000);
    assert_eq!(m.cpu.read_u32(DATA + 808).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 512).unwrap(), 0x8000ff00);
    assert_eq!(m.cpu.read_u32(DATA + 516).unwrap(), 0xffff0000);
}
#[test]
// spec: DEV-IMAGE-6 DEV-IMAGE-9
fn solid_brush_mutation_is_drawn_and_clones_are_independent() {
    colors(JitCpu::new(PARK_ADDR, false).unwrap());
    colors(JitCpu::new(PARK_ADDR, true).unwrap());
    colors(UnicornCpu::new(PARK_ADDR).unwrap());
}
