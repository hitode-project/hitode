//! Display DC origins follow their USER owner rather than logical viewport state.
use super::*;
fn origins<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let class = f.class(proc, "device origins", true);
    let a = geometry::create(&mut f, class, I(0), 0, [10, 20, 100, 100], false);
    let child = geometry::create(&mut f, class, M(a), 0, [3, 4, 10, 10], true);
    let dc = f.call("GetDC", &[M(child)]);
    let window_dc = f.call("GetWindowDC", &[M(a)]);
    let origin = f.alloc(8);
    let window_origin = f.alloc(8);
    let moved = f.alloc(8);
    let read_only = DATA + 0xf000;
    f.machine.cpu.write(read_only, &[0xcc; 8]).unwrap();
    f.machine.cpu.protect(read_only, 4096, Prot::READ).unwrap();
    let get = f.call("gdi32.dll!GetDCOrgEx", &[M(dc), I(origin)]);
    let get_window = f.call("gdi32.dll!GetDCOrgEx", &[M(window_dc), I(window_origin)]);
    f.call("gdi32.dll!SetViewportOrgEx", &[M(dc), I(100), I(200), I(0)]);
    f.call("MoveWindow", &[M(a), I(30), I(40), I(100), I(100), I(0)]);
    let get_moved = f.call("gdi32.dll!GetDCOrgEx", &[M(dc), I(moved)]);
    let invalid = f.call("gdi32.dll!GetDCOrgEx", &[I(0xdeadbeef), I(origin)]);
    let invalid_error = f.call("kernel32.dll!GetLastError", &[]);
    let bad_output = f.call("gdi32.dll!GetDCOrgEx", &[M(dc), I(read_only)]);
    let output_error = f.call("kernel32.dll!GetLastError", &[]);
    f.call("ReleaseDC", &[M(child), M(dc)]);
    f.call("ReleaseDC", &[M(a), M(window_dc)]);
    f.run();
    for (slot, value) in [
        (get, 1),
        (get_window, 1),
        (get_moved, 1),
        (invalid, 0),
        (invalid_error, 6),
        (bad_output, 0),
        (output_error, 87),
    ] {
        assert_eq!(f.value(slot), value);
    }
    for (at, xy) in [
        (origin, [13, 24]),
        (window_origin, [10, 20]),
        (moved, [33, 44]),
    ] {
        assert_eq!([f.value(at), f.value(at + 4)], xy);
    }
    assert_eq!(f.read(read_only, 8), [0xcc; 8]);
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}
#[test]
// spec: WIN32-GDI-1 WIN32-USER-6
fn dc_origins_follow_window_moves_without_including_viewport_offsets() {
    origins(JitCpu::new(PARK_ADDR, false).unwrap());
    origins(JitCpu::new(PARK_ADDR, true).unwrap());
    origins(UnicornCpu::new(PARK_ADDR).unwrap());
}
