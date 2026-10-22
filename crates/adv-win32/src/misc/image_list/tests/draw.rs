//! Authored icon pixels traverse public USER, list and GDI ownership paths.
use super::*;
use adv_device::surface::{Snapshot, SurfaceStore};
fn bytes(f: &mut Fixture, bytes: &[u8]) -> u32 {
    let address = f.data;
    for chunk in bytes.chunks(4) {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        f.word(u32::from_le_bytes(word));
    }
    address
}
fn install(f: &mut Fixture) {
    let arena = f
        .machine
        .sys
        .vm
        .alloc(&mut f.machine.cpu, 65536, Prot::RW)
        .unwrap();
    f.machine
        .ext
        .insert(SurfaceStore::new(12, 2, arena, 65536, Snapshot::default()).unwrap());
    crate::gdi32::raster::front::set_provider(
        &mut f.machine.ext,
        crate::gdi32::raster::front::DrawingProvider {
            memory: crate::gdi32::raster::bitmap::memory,
            completed: |_, _| None,
        },
    );
}
#[test]
// spec: DEV-IMAGE-15 DEV-IMAGE-12
fn icons_and_lists_paint_alpha_masks_clips_and_independent_extracted_handles() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        install(&mut f);
        let info = bytes(
            &mut f,
            &[40u32, 12, u32::MAX - 1, 1 | (32 << 16), 0, 96, 0, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let bits = f.word(0);
        let bitmap = f.call(
            "gdi32.dll!CreateDIBSection",
            &[I(0), I(info), I(0), I(bits), I(0), I(0)],
        );
        let dc = f.call("gdi32.dll!CreateCompatibleDC", &[I(0)]);
        f.call("gdi32.dll!SelectObject", &[M(dc), M(bitmap)]);
        f.run();
        let destination = f.value(bits);
        assert_ne!(destination, 0);
        f.machine
            .cpu
            .write(destination, &[10, 20, 30, 40].repeat(24))
            .unwrap();
        let mask = bytes(&mut f, &[0x50, 0]);
        let alpha_mask = bytes(&mut f, &[0xc0, 0]);
        let legacy_pixels = bytes(&mut f, &[0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255]);
        let alpha_pixels = bytes(&mut f, &[0, 0, 128, 128, 255, 0, 0, 255]);
        let mono_pixels = bytes(&mut f, &[0x30, 0]);
        let legacy = f.call(
            "user32.dll!CreateIcon",
            &[I(0), I(4), I(1), I(1), I(24), I(mask), I(legacy_pixels)],
        );
        let alpha = f.call(
            "user32.dll!CreateIcon",
            &[
                I(0),
                I(2),
                I(1),
                I(1),
                I(32),
                I(alpha_mask),
                I(alpha_pixels),
            ],
        );
        let mono = f.call(
            "user32.dll!CreateIcon",
            &[I(0), I(4), I(1), I(1), I(1), I(mask), I(mono_pixels)],
        );
        let mut results = Vec::new();
        for (icon, x, width) in [(legacy, 0, 4), (alpha, 4, 2), (mono, 8, 4)] {
            results.push(f.call(
                "user32.dll!DrawIconEx",
                &[M(dc), I(x), I(0), M(icon), I(width), I(1), I(0), I(0), I(3)],
            ));
        }
        let list = f.call("ImageList_Create", &[I(2), I(1), I(0x21), I(0), I(1)]);
        let added = f.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), M(alpha)]);
        let extracted = f.call("ImageList_GetIcon", &[M(list), I(0), I(0)]);
        results.push(f.call("ImageList_Draw", &[M(list), I(0), M(dc), I(0), I(1), I(1)]));
        results.push(f.call(
            "ImageList_DrawEx",
            &[
                M(list),
                I(0),
                M(dc),
                I(4),
                I(1),
                I(1),
                I(1),
                I(u32::MAX),
                I(0),
                I(1),
            ],
        ));
        let icon_info = bytes(&mut f, &[0; 20]);
        results.push(f.call("user32.dll!GetIconInfo", &[M(mono), I(icon_info)]));
        let alpha_info = bytes(&mut f, &[0; 20]);
        results.push(f.call("user32.dll!GetIconInfo", &[M(alpha), I(alpha_info)]));
        results.push(f.call("ImageList_Destroy", &[M(list)]));
        results.push(f.call("user32.dll!DestroyIcon", &[M(alpha)]));
        results.push(f.call(
            "user32.dll!DrawIconEx",
            &[
                M(dc),
                I(8),
                I(1),
                M(extracted),
                I(2),
                I(1),
                I(0),
                I(0),
                I(3),
            ],
        ));
        // DrawIcon uses the documented default 32-pixel size, constrained by the real DC clip.
        f.call(
            "gdi32.dll!IntersectClipRect",
            &[M(dc), I(10), I(1), I(11), I(2)],
        );
        let white_bits = bytes(&mut f, &[255, 255, 255, 0]);
        let white = f.call(
            "user32.dll!CreateIcon",
            &[I(0), I(1), I(1), I(1), I(24), I(mask), I(white_bits)],
        );
        results.push(f.call("user32.dll!DrawIcon", &[M(dc), I(10), I(1), M(white)]));
        f.run();
        assert_eq!(f.value(added), 0);
        for result in results {
            assert_eq!(f.value(result), 1);
        }
        let mut actual = [0; 96];
        f.machine.cpu.read(destination, &mut actual).unwrap();
        let mut expected = [10, 20, 30, 40].repeat(24);
        expected[..16].copy_from_slice(&[
            0, 0, 255, 0, 10, 235, 30, 40, 255, 0, 0, 0, 245, 235, 225, 40,
        ]);
        expected[16..24].copy_from_slice(&[5, 10, 143, 148, 255, 0, 0, 255]);
        expected[32..48].copy_from_slice(&[
            0, 0, 0, 0, 10, 20, 30, 40, 255, 255, 255, 0, 245, 235, 225, 40,
        ]);
        expected[48..56].copy_from_slice(&[5, 10, 143, 148, 255, 0, 0, 255]);
        expected[64..68].copy_from_slice(&[5, 10, 143, 148]);
        expected[80..88].copy_from_slice(&[5, 10, 143, 148, 255, 0, 0, 255]);
        expected[88..92].copy_from_slice(&[255, 255, 255, 0]);
        assert_eq!(actual.as_slice(), expected);
        assert_eq!(f.value(icon_info + 16), 0);
        let bitmap_pixels = |handle| {
            let Some(adv_os::gdi::GdiObj::Bitmap {
                height,
                bits,
                stride,
                ..
            }) = f
                .machine
                .ext
                .get::<Win32>()
                .gdi
                .get(&f.machine.sys.objects, handle)
            else {
                panic!("bitmap missing")
            };
            let mut pixels = vec![0; (*stride * height.unsigned_abs()) as usize];
            f.machine.cpu.read(*bits, &mut pixels).unwrap();
            (*height, *stride, pixels)
        };
        assert_eq!(
            bitmap_pixels(f.value(icon_info + 12)),
            (2, 4, vec![0x50, 0, 0, 0, 0x30, 0, 0, 0])
        );
        assert_eq!(
            bitmap_pixels(f.value(alpha_info + 16)),
            (1, 8, vec![0, 0, 128, 128, 255, 0, 0, 255])
        );
        assert!(
            f.machine
                .ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .is_empty()
        );
    }
}

#[test]
// spec: DEV-IMAGE-15
fn invalid_icons_lists_and_styles_leave_pixels_and_owners_unchanged() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        install(&mut f);
        let icon = f.icon(1, 1, [255, 0, 0, 128], 0);
        let list = f.call("ImageList_Create", &[I(1), I(1), I(0x21), I(0), I(1)]);
        f.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(icon)]);
        let mut failures = Vec::new();
        for (name, args, code) in [
            (
                "user32.dll!DrawIconEx",
                vec![I(0), I(0), I(0), I(0xdead), I(1), I(1), I(0), I(0), I(3)],
                1402,
            ),
            (
                "ImageList_Draw",
                vec![M(list), I(1), I(0), I(0), I(0), I(0)],
                6,
            ),
            (
                "ImageList_Draw",
                vec![M(list), I(0), I(0), I(0), I(0), I(0x80)],
                87,
            ),
            (
                "ImageList_Draw",
                vec![M(list), I(0), I(0), I(0), I(0), I(2)],
                50,
            ),
        ] {
            let result = f.call(name, &args);
            let error = f.error();
            failures.push((result, error, code));
        }
        f.run();
        for (result, error, code) in failures {
            assert_eq!((f.value(result), f.value(error)), (0, code));
        }
        assert_eq!(f.list(list).images.len(), 1);
        assert_eq!(f.list(list).images[0].rgba, [255, 0, 0, 128]);
        assert_eq!(
            f.machine
                .ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .len(),
            1
        );
    }
}
