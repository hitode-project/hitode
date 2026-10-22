//! Authored windows expose nested and mirrored mapping plus immediate-child lookup.
use super::*;
pub(super) fn create<B: TestCpu>(
    f: &mut Fixture<B>,
    class: u32,
    parent: Arg,
    ex: u32,
    rectangle: [i32; 4],
    child: bool,
) -> u32 {
    f.call(
        "CreateWindowExW",
        &[
            I(ex),
            M(class),
            I(0),
            I(if child { 0x4000_0000 } else { WS_POPUP }),
            I(rectangle[0] as u32),
            I(rectangle[1] as u32),
            I(rectangle[2] as u32),
            I(rectangle[3] as u32),
            parent,
            I(0),
            I(CODE),
            I(0),
        ],
    )
}
fn points<B: TestCpu>(f: &mut Fixture<B>, values: &[i32]) -> u32 {
    f.bytes(
        &values
            .iter()
            .flat_map(|n| n.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}
fn geometry<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.machine
        .sys
        .thunks
        .resolver_mut::<Registry>()
        .unwrap()
        .add(
            "<geometry-fixture>",
            "post",
            Conv::Stdcall { argc: 0 },
            |env| {
                adv_machine::Outcome::Return(u32::from(adv_win32::user32::host_input::post(
                    env,
                    wm::MOUSEMOVE,
                    0,
                    185 | (26 << 16),
                )))
            },
        );
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let class = f.class(proc, "point mapping", true);
    let root = create(&mut f, class, I(0), 0, [-100, 50, 200, 100], false);
    let child = create(&mut f, class, M(root), 0, [10, 20, 100, 60], true);
    let grandchild = create(&mut f, class, M(child), 0, [5, 6, 20, 10], true);
    let other = create(&mut f, class, M(root), 0, [10, 20, 100, 60], true);
    f.call("EnableWindow", &[M(child), I(0)]);
    let inside = f.call("ChildWindowFromPoint", &[M(root), I(15), I(26)]);
    let parent = f.call("ChildWindowFromPoint", &[M(root), I(150), I(90)]);
    let edge = f.call("ChildWindowFromPoint", &[M(root), I(200), I(20)]);
    let outside = f.call("ChildWindowFromPoint", &[M(root), I(u32::MAX), I(20)]);
    let point = points(&mut f, &[15, 26]);
    let delta = f.call("MapWindowPoints", &[M(root), M(child), I(point), I(1)]);
    let screen_point = points(&mut f, &[0, 0]);
    let screen_delta = f.call(
        "MapWindowPoints",
        &[M(grandchild), I(0), I(screen_point), I(1)],
    );
    let identity_point = points(&mut f, &[17, -29]);
    f.call("kernel32.dll!SetLastError", &[I(1234)]);
    let identity = f.call(
        "MapWindowPoints",
        &[M(root), M(root), I(identity_point), I(1)],
    );
    let preserved = f.call("kernel32.dll!GetLastError", &[]);
    let rtl = create(&mut f, class, I(0), 0x0040_0000, [300, 50, 200, 100], false);
    let rtl_child = create(&mut f, class, M(rtl), 0, [10, 20, 100, 60], true);
    let inherited = f.call("GetWindowLongW", &[M(rtl_child), I((-20i32) as u32)]);
    let bounds = f.alloc(16);
    f.call("GetWindowRect", &[M(rtl_child), I(bounds)]);
    let mirrored_rect = points(&mut f, &[1, 2, 10, 20]);
    let rtl_delta = f.call(
        "MapWindowPoints",
        &[M(root), M(rtl), I(mirrored_rect), I(2)],
    );
    let mirrored_point = points(&mut f, &[5, 6]);
    let child_delta = f.call(
        "MapWindowPoints",
        &[M(rtl_child), I(0), I(mirrored_point), I(1)],
    );
    let same_direction = points(&mut f, &[15, 25, 25, 35]);
    let same_delta = f.call(
        "MapWindowPoints",
        &[M(rtl), M(rtl_child), I(same_direction), I(2)],
    );
    let rtl_hit = f.call("ChildWindowFromPoint", &[M(rtl), I(15), I(25)]);
    f.call("ShowWindow", &[M(rtl), I(5)]);
    f.call("ShowWindow", &[M(rtl_child), I(5)]);
    let from_screen = f.call("WindowFromPoint", &[I(485), I(76)]);
    let posted = f.call("<geometry-fixture>!post", &[]);
    let message = f.alloc(32);
    let received = f.call(
        "PeekMessageW",
        &[I(message), I(0), I(wm::MOUSEMOVE), I(wm::MOUSEMOVE), I(1)],
    );
    let invalid_points = points(&mut f, &[7, 8, 9, 10]);
    let invalid = f.call(
        "MapWindowPoints",
        &[I(0xdeadbeef), M(root), I(invalid_points), I(2)],
    );
    let invalid_error = f.call("kernel32.dll!GetLastError", &[]);
    let overflow = f.call(
        "MapWindowPoints",
        &[M(root), I(0), I(invalid_points), I(u32::MAX)],
    );
    let overflow_error = f.call("kernel32.dll!GetLastError", &[]);
    let readonly = DATA + 0xf000;
    f.machine.cpu.write(readonly, &[0xcc; 16]).unwrap();
    f.machine.cpu.protect(readonly, 4096, Prot::READ).unwrap();
    let bad_buffer = f.call("MapWindowPoints", &[M(root), I(0), I(readonly), I(2)]);
    let buffer_error = f.call("kernel32.dll!GetLastError", &[]);
    let bad_parent = f.call("ChildWindowFromPoint", &[I(0xdeadbeef), I(0), I(0)]);
    let parent_error = f.call("kernel32.dll!GetLastError", &[]);
    f.run();
    for (slot, expected) in [
        (inside, f.value(child)),
        (parent, f.value(root)),
        (edge, 0),
        (outside, 0),
        (
            delta,
            (-10i16 as u16 as u32) | ((-20i16 as u16 as u32) << 16),
        ),
        (screen_delta, (-85i16 as u16 as u32) | (76 << 16)),
        (identity, 0),
        (preserved, 1234),
        (inherited, 0x0040_0000),
        (rtl_delta, (-600i16 as u16 as u32)),
        (child_delta, (-490i16 as u16 as u32) | (70 << 16)),
        (
            same_delta,
            (-10i16 as u16 as u32) | ((-20i16 as u16 as u32) << 16),
        ),
        (rtl_hit, f.value(rtl_child)),
        (invalid, 0),
        (invalid_error, 1400),
        (overflow, 0),
        (overflow_error, 87),
        (bad_buffer, 0),
        (buffer_error, 998),
        (bad_parent, 0),
        (parent_error, 1400),
    ] {
        assert_eq!(f.value(slot), expected, "geometry result {slot:x}");
    }
    assert_eq!(f.value(from_screen), f.value(rtl_child));
    assert_eq!(f.value(posted), 1);
    assert_eq!(f.value(received), 1);
    assert_eq!(f.value(message), f.value(rtl_child));
    assert_eq!(f.value(message + 12), 5 | (6 << 16));
    assert_ne!(f.value(other), 0);
    for (address, expected) in [
        (point, vec![5, 6]),
        (screen_point, vec![-85, 76]),
        (identity_point, vec![17, -29]),
        (bounds, vec![390, 70, 490, 130]),
        (mirrored_rect, vec![590, 2, 599, 20]),
        (mirrored_point, vec![485, 76]),
        (same_direction, vec![5, 5, 15, 15]),
        (invalid_points, vec![7, 8, 9, 10]),
    ] {
        for (i, coordinate) in expected.into_iter().enumerate() {
            assert_eq!(
                f.value(address + i as u32 * 4),
                coordinate as u32,
                "coordinate {address:x}+{i}"
            );
        }
    }
    assert_eq!(f.read(readonly, 16), [0xcc; 16]);
}
#[test]
// spec: WIN32-USER-6
fn point_mapping_and_immediate_children_share_normal_and_mirrored_geometry() {
    geometry(JitCpu::new(PARK_ADDR, false).unwrap());
    geometry(JitCpu::new(PARK_ADDR, true).unwrap());
    geometry(UnicornCpu::new(PARK_ADDR).unwrap());
}
