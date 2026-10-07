use super::*;

fn query(m: &mut Machine<JitCpu>, data: u32, dc: u32, complexity: u32, expected: [i32; 4]) {
    m.cpu.write(data + 124, &[0xa5; 24]).unwrap();
    crate::util::set_last_error(&mut env(m), 0x12345678);
    assert_eq!(
        call(m, data, "gdi32.dll!GetClipBox", &[dc, data + 128]),
        complexity
    );
    assert_eq!(
        std::array::from_fn::<_, 4, _>(
            |i| m.cpu.read_u32(data + 128 + i as u32 * 4).unwrap() as i32
        ),
        expected
    );
    assert_eq!(m.cpu.read_u32(data + 124).unwrap(), 0xa5a5a5a5);
    assert_eq!(m.cpu.read_u32(data + 144).unwrap(), 0xa5a5a5a5);
    assert_eq!(crate::util::last_error(&env(m)), 0x12345678);
}

#[test]
fn get_clip_box_matches_native_surface_intersections_holes_origins_and_restore() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let objects = m.sys.objects.object_count();
    query(&mut m, data, dc, 2, [0, 0, 8, 8]);
    for (api, rect, complexity, expected) in [
        ("IntersectClipRect", [2, 2, 5, 5], 2, [2, 2, 5, 5]),
        ("IntersectClipRect", [20, 20, 30, 30], 1, [0; 4]),
        ("IntersectClipRect", [5, 5, 2, 2], 2, [2, 2, 5, 5]),
        ("IntersectClipRect", [2, 2, 2, 5], 1, [0; 4]),
        ("ExcludeClipRect", [2, 2, 5, 5], 3, [0, 0, 8, 8]),
        ("ExcludeClipRect", [0, 0, 8, 8], 1, [0; 4]),
        ("ExcludeClipRect", [20, 20, 30, 30], 2, [0, 0, 8, 8]),
        ("ExcludeClipRect", [0, 0, 2, 8], 2, [2, 0, 8, 8]),
        ("ExcludeClipRect", [0, 0, 8, 3], 2, [0, 3, 8, 8]),
    ] {
        assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
        assert_ne!(
            call(
                &mut m,
                data,
                &format!("gdi32.dll!{api}"),
                &[
                    dc,
                    rect[0] as u32,
                    rect[1] as u32,
                    rect[2] as u32,
                    rect[3] as u32
                ]
            ),
            0
        );
        query(&mut m, data, dc, complexity, expected);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
            1
        );
    }
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    assert_ne!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 2, 2, 5, 5]),
        0
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetWindowOrgEx",
            &[dc, u32::MAX, u32::MAX, 0]
        ),
        1
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetViewportOrgEx", &[dc, 1, 0, 0]),
        1
    );
    query(&mut m, data, dc, 3, [-2, -1, 6, 7]);
    assert_ne!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 1, 3, 4]
        ),
        0
    );
    query(&mut m, data, dc, 1, [0; 4]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    query(&mut m, data, dc, 2, [0, 0, 8, 8]);
    // This metadata query neither reads pixels nor requires the raster provider.
    m.cpu.protect(pixels, 4096, Prot(0)).unwrap();
    super::super::super::super::front::install(&mut m);
    query(&mut m, data, dc, 2, [0, 0, 8, 8]);
    assert_eq!(m.sys.objects.object_count(), objects);
}

#[test]
fn get_clip_box_uses_the_current_paint_union_across_application_save_restore() {
    let (mut m, data, dc, _) = fixture(8, 8, PixelFormat::Bgra32, false);
    let gdi = m.ext.get::<Win32>().gdi.clone();
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    let Some(GdiObj::Dc { clip, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
        panic!("DC owner");
    };
    clip.replace_paint_region(Some(vec![[0, 0, 2, 2], [6, 6, 8, 8]]));
    query(&mut m, data, dc, 3, [0, 0, 8, 8]);
    assert_ne!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 0, 2, 2]
        ),
        0
    );
    query(&mut m, data, dc, 2, [0, 0, 2, 2]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    query(&mut m, data, dc, 3, [0, 0, 8, 8]);
    for rectangles in [
        vec![[0, 0, 4, 8], [4, 0, 8, 8]],
        vec![[0, 0, 8, 4], [0, 4, 8, 8]],
    ] {
        let Some(GdiObj::Dc { clip, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
            panic!("DC owner");
        };
        clip.replace_paint_region(Some(rectangles));
        query(&mut m, data, dc, 2, [0, 0, 8, 8]);
    }
    let Some(GdiObj::Dc { clip, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
        panic!("DC owner");
    };
    clip.replace_paint_region(Some(Vec::new()));
    query(&mut m, data, dc, 1, [0; 4]);
}

#[test]
fn get_clip_box_invalid_outputs_preserve_last_error_and_unsupported_mapping_is_explicit() {
    let (mut m, data, dc, _) = fixture(8, 8, PixelFormat::Bgra32, false);
    m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    let pages = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.write_u32(pages + 4092, 0xdeadbeef).unwrap();
    m.cpu.protect(pages + 4096, 4096, Prot(0)).unwrap();
    for output in [0, 1, u32::MAX - 3, pages + 4092] {
        crate::util::set_last_error(&mut env(&mut m), 0x12345678);
        assert_eq!(call(&mut m, data, "gdi32.dll!GetClipBox", &[dc, output]), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    }
    assert_eq!(m.cpu.read_u32(pages + 4092).unwrap(), 0xdeadbeef);
    m.cpu.protect(pages, 4096, Prot::READ).unwrap();
    assert_eq!(call(&mut m, data, "gdi32.dll!GetClipBox", &[dc, pages]), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    m.cpu.write(data + 128, &[0xa5; 16]).unwrap();
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetClipBox", &[0, data + 128]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 6);
    let gdi = m.ext.get::<Win32>().gdi.clone();
    let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
        panic!("DC owner");
    };
    modes.map = 2;
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetClipBox", &[dc, data + 128]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
        panic!("DC owner");
    };
    modes.map = 1;
    modes.window_origin = [i32::MAX, 0];
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetClipBox", &[dc, data + 128]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert_eq!(m.cpu.read_u32(data + 128).unwrap(), 0xa5a5a5a5);
    assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(), 2);
}
