use super::*;

#[test]
fn copy_icon_preserves_real_payload_and_independent_lifetimes_with_native_invalid_handle_errors() {
    // Authored x86 Windows oracle: CopyIcon(NULL/invalid/bitmap/stale) gives
    // ERROR_INVALID_CURSOR_HANDLE (1402). Icons and cursors are both accepted,
    // successful copies preserve LastError and survive source destruction.
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert_eq!(
        registry.get("user32.dll", "CopyIcon").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 1 }
    );
    assert!(registry.dynamic_available("user32.dll", "CopyIcon"));
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let module = test.resource();
        // One actual AND-mask bit in the top row; the DIB stores rows bottom up.
        let mask = RESOURCE + 0x2000 + 40 + 16 * 16 * 4 + 15 * 4;
        test.machine.cpu.write(mask, &[0x80]).unwrap();
        let private = test.load(true, module, 101, 16, 16, 0);
        test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
        let first = test.call("CopyIcon", &[M(private)]);
        let success_error = test.call("kernel32.dll!GetLastError", &[]);
        let second = test.call("CopyIcon", &[M(first)]);
        test.call("DestroyIcon", &[M(private)]);
        let doomed = test.call("CopyIcon", &[M(second)]);
        test.call("DestroyIcon", &[M(doomed)]);

        let bitmap = test.call("gdi32.dll!CreateBitmap", &[I(8), I(8), I(1), I(32), I(0)]);
        let mut failures = Vec::new();
        for source in [I(0), I(0xdeadbeef), M(bitmap), M(private), M(doomed)] {
            test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
            let copy = test.call("CopyIcon", &[source]);
            let error = test.call("kernel32.dll!GetLastError", &[]);
            failures.push((copy, error));
        }
        test.call("gdi32.dll!DeleteObject", &[M(bitmap)]);

        let shared = test.load(true, module, 101, 32, 32, SHARED);
        let shared_copy = test.call("CopyIcon", &[M(shared)]);
        test.call("DestroyIcon", &[M(shared_copy)]);
        test.call("DestroyIcon", &[M(shared)]);
        let shared_again = test.load(false, module, 101, 32, 32, SHARED);
        let stock = test.call("LoadIconW", &[I(0), I(32516)]);
        test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
        let stock_copy = test.call("CopyIcon", &[M(stock)]);
        let stock_error = test.call("kernel32.dll!GetLastError", &[]);
        test.call("DestroyIcon", &[M(stock)]);
        let cursor = test.call("LoadCursorW", &[I(0), I(32512)]);
        let cursor_copy = test.call("CopyIcon", &[M(cursor)]);
        test.call("DestroyCursor", &[M(cursor_copy)]);
        test.call("DestroyCursor", &[M(cursor)]);
        test.run();

        assert_eq!(test.value(success_error), 0x12345678);
        assert_eq!(test.value(stock_error), 0x12345678);
        assert_ne!(test.value(bitmap), 0);
        for (copy, error) in failures {
            assert_eq!((test.value(copy), test.value(error)), (0, 1402));
        }
        let resources = &test.machine.ext.get::<Win32>().user.objects;
        let objects = &test.machine.sys.objects;
        for result in [private, doomed, shared_copy, cursor_copy] {
            assert!(!resources.contains(objects, test.value(result)));
        }
        let a = test.image(first);
        let b = test.image(second);
        assert_ne!(test.value(first), test.value(second));
        assert_ne!(test.value(first), test.value(private));
        assert_eq!((a.width, a.height), (16, 16));
        assert_eq!((b.width, b.height), (16, 16));
        assert_eq!(a.pixels, b.pixels);
        assert_eq!(a.mask, b.mask);
        assert_ne!(a.pixels.as_ptr(), b.pixels.as_ptr());
        assert_ne!(a.mask.as_ptr(), b.mask.as_ptr());
        assert_eq!(&a.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(a.mask[0], 1);
        assert!(a.mask[1..].iter().all(|&bit| bit == 0));
        assert_eq!(test.value(shared), test.value(shared_again));
        assert_ne!(test.value(shared_copy), test.value(shared));
        for result in [stock, stock_copy, cursor] {
            assert!(resources.contains(objects, test.value(result)));
            assert!(resources.image(objects, test.value(result)).is_none());
        }
        assert_ne!(test.value(stock), test.value(stock_copy));
        assert_ne!(test.value(cursor), test.value(cursor_copy));
        let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(diagnostics.warnings.keys().any(|key| {
            key.0 == "user32.dll!CopyIcon" && key.1.contains("source pixel data is unavailable")
        }));
    }

    // Common owner capacity failure cannot retire or rewrite the source.
    let mut objects = adv_machine::Objects::with_limits(adv_machine::ObjectLimits {
        objects: 1,
        ..adv_machine::ObjectLimits::default()
    });
    let mut resources = super::super::super::resources::UserObjects::default();
    let source = resources
        .create_icon(
            &mut objects,
            "authored source".into(),
            IconImage {
                alpha: true,
                monochrome: false,
                width: 1,
                height: 1,
                pixels: vec![1, 2, 3, 255],
                mask: vec![1],
            },
        )
        .unwrap();
    assert_eq!(resources.copy_icon(&mut objects, source), Err(8));
    assert_eq!(objects.object_count(), 1);
    assert_eq!(
        resources.image(&objects, source).unwrap().pixels,
        [1, 2, 3, 255]
    );
    assert_eq!(resources.image(&objects, source).unwrap().mask, [1]);
}
