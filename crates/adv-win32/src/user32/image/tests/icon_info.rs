use super::*;

#[test]
fn get_icon_info_returns_caller_owned_mask_and_color_bitmaps() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert_eq!(
        registry.get("user32.dll", "GetIconInfo").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 2 }
    );
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let module = test.resource();
        // One AND-mask bit at the top-left pixel; the DIB stores rows bottom up.
        let mask = RESOURCE + 0x2000 + 40 + 16 * 16 * 4 + 15 * 4;
        test.machine.cpu.write(mask, &[0x80]).unwrap();
        let icon = test.load(true, module, 101, 16, 16, 0);
        let info = test.bytes(&[0; 20]);
        let success = test.call("GetIconInfo", &[M(icon), I(info)]);
        let mask_object = test.bytes(&[0; 24]);
        let color_object = test.bytes(&[0; 24]);
        test.call(
            "gdi32.dll!GetObjectA",
            &[M(info + 12), I(24), I(mask_object)],
        );
        test.call(
            "gdi32.dll!GetObjectA",
            &[M(info + 16), I(24), I(color_object)],
        );
        // A second query owns a second pair, which the caller deletes.
        let again = test.bytes(&[0; 20]);
        test.call("GetIconInfo", &[M(icon), I(again)]);
        let mask_deleted = test.call("gdi32.dll!DeleteObject", &[M(again + 12)]);
        let color_deleted = test.call("gdi32.dll!DeleteObject", &[M(again + 16)]);

        // Invalid handles, cursors without pixel data and unwritable output fail.
        let cursor = test.call("LoadCursorW", &[I(0), I(32512)]);
        let spare = test.bytes(&[0xaa; 20]);
        let mut failures = Vec::new();
        for (source, output) in [
            (I(0), I(spare)),
            (I(0xdeadbeef), I(spare)),
            (M(cursor), I(spare)),
            (M(icon), I(CODE)),
        ] {
            test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
            let result = test.call("GetIconInfo", &[source, output]);
            let error = test.call("kernel32.dll!GetLastError", &[]);
            failures.push((result, error));
        }
        test.run();

        assert_eq!(test.value(success), 1);
        let word = |offset| test.value(info + offset);
        assert_eq!((word(0), word(4), word(8)), (1, 8, 8));
        assert_ne!(word(12), 0);
        assert_ne!(word(16), 0);
        let bitmap = |object: u32| {
            (
                test.value(object + 4),
                test.value(object + 8),
                test.machine.cpu.read_u16(object + 18).unwrap(),
            )
        };
        assert_eq!(bitmap(mask_object), (16, 16, 1));
        assert_eq!(bitmap(color_object), (16, 16, 32));
        let pixels = |handle: u32| {
            let gdi = &test.machine.ext.get::<Win32>().gdi;
            let Some(crate::gdi32::GdiObj::Bitmap { bits, stride, .. }) =
                gdi.get(&test.machine.sys.objects, handle)
            else {
                panic!("no bitmap {handle:#x}");
            };
            let mut rows = vec![0; (stride * 16) as usize];
            test.machine.cpu.read(*bits, &mut rows).unwrap();
            (*stride as usize, rows)
        };
        // Top-down rows: only the top-left mask bit is set.
        let (stride, mask) = pixels(word(12));
        assert_eq!(mask[0], 0x80);
        assert!(
            mask[1..].iter().all(|&byte| byte == 0),
            "{stride} {mask:x?}"
        );
        let (stride, color) = pixels(word(16));
        assert_eq!(stride, 64);
        assert_eq!(&color[..4], &[0, 0, 255, 255]);
        assert_ne!(
            (test.value(again + 12), test.value(again + 16)),
            (word(12), word(16))
        );
        assert_eq!(
            (test.value(mask_deleted), test.value(color_deleted)),
            (1, 1)
        );

        let results: Vec<_> = failures
            .into_iter()
            .map(|(result, error)| (test.value(result), test.value(error)))
            .collect();
        assert_eq!(results, [(0, 1402), (0, 1402), (0, 50), (0, 998)]);
        let mut untouched = [0; 20];
        test.machine.cpu.read(spare, &mut untouched).unwrap();
        assert_eq!(untouched, [0xaa; 20]);
        let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(diagnostics.warnings.keys().any(|key| {
            key.0 == "user32.dll!GetIconInfo" && key.1.contains("without decoded pixel data")
        }));
    }
}
