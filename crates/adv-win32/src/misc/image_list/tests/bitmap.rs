//! Real GDI bitmap strips and indexed palettes become independent list snapshots.
use super::*;
fn bytes(f: &mut Fixture, bytes: &[u8]) -> u32 {
    let first = f.data;
    for chunk in bytes.chunks(4) {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        f.word(u32::from_le_bytes(word));
    }
    first
}
#[test]
// spec: DEV-IMAGE-14
fn bitmap_strips_copy_masks_replace_one_image_and_release_source_ownership() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let input = bytes(
            &mut f,
            &[
                10, 20, 30, 255, 40, 50, 60, 128, 70, 80, 90, 0, 100, 110, 120, 255,
            ],
        );
        let other = bytes(&mut f, &[1, 2, 3, 255, 4, 5, 6, 255]);
        let bits = bytes(&mut f, &[0xa0, 0]);
        let source = f.call(
            "gdi32.dll!CreateBitmap",
            &[I(4), I(1), I(1), I(32), I(input)],
        );
        let replacement = f.call(
            "gdi32.dll!CreateBitmap",
            &[I(2), I(1), I(1), I(32), I(other)],
        );
        let mask = f.call("gdi32.dll!CreateBitmap", &[I(4), I(1), I(1), I(1), I(bits)]);
        let list = f.call("ImageList_Create", &[I(2), I(1), I(0x21), I(0), I(1)]);
        let added = f.call("ImageList_Add", &[M(list), M(source), M(mask)]);
        let replaced = f.call("ImageList_Replace", &[M(list), I(1), M(replacement), I(0)]);
        let bad = f.call("ImageList_Replace", &[M(list), I(2), M(replacement), I(0)]);
        let unsupported = f.call("ImageList_AddMasked", &[M(list), M(source), I(0)]);
        let count = f.call("ImageList_GetImageCount", &[M(list)]);
        let mut deleted = Vec::new();
        for bitmap in [source, replacement, mask] {
            deleted.push(f.call("gdi32.dll!DeleteObject", &[M(bitmap)]));
        }
        f.run();
        for (slot, value) in [
            (added, 0),
            (replaced, 1),
            (bad, 0),
            (unsupported, u32::MAX),
            (count, 2),
        ] {
            assert_eq!(f.value(slot), value);
        }
        for slot in deleted {
            assert_eq!(f.value(slot), 1);
        }
        let list = f.list(list);
        assert_eq!(list.images[0].rgba, [30, 20, 10, 255, 60, 50, 40, 128]);
        assert_eq!(list.images[0].mask, Some(vec![1, 0]));
        assert_eq!(list.images[1].rgba, [3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(list.images[1].mask, Some(vec![0, 0]));
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
// spec: DEV-IMAGE-14 WIN32-GDI-6
fn indexed_color_keys_and_upper_left_defaults_copy_palette_colors_without_modifying_source() {
    use adv_device::surface::{Snapshot, SurfaceStore};
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let arena = f
            .machine
            .sys
            .vm
            .alloc(&mut f.machine.cpu, 65536, Prot::RW)
            .unwrap();
        f.machine
            .ext
            .insert(SurfaceStore::new(4, 1, arena, 65536, Snapshot::default()).unwrap());
        crate::gdi32::raster::front::set_provider(
            &mut f.machine.ext,
            crate::gdi32::raster::front::DrawingProvider {
                memory: crate::gdi32::raster::bitmap::memory,
                completed: |_, _| None,
            },
        );
        let info = bytes(
            &mut f,
            &[
                40u32,
                4,
                u32::MAX,
                1 | (8 << 16),
                0,
                4,
                0,
                0,
                2,
                0,
                0x00ff0000,
                0x0000ff00,
            ]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
        );
        let bits = f.word(0);
        let source = f.call(
            "gdi32.dll!CreateDIBSection",
            &[I(0), I(info), I(0), I(bits), I(0), I(0)],
        );
        f.run();
        let address = f.value(bits);
        assert_ne!(address, 0);
        f.machine.cpu.write(address, &[1, 0, 1, 0]).unwrap();
        let mut cases = Vec::new();
        for color in [0x00ff00, 0xff000000, 0x000000ff] {
            let list = f.call("ImageList_Create", &[I(2), I(1), I(0x21), I(0), I(1)]);
            let added = f.call("ImageList_AddMasked", &[M(list), M(source), I(color)]);
            let error = f.error();
            cases.push((list, added, color, error));
        }
        let source_copy = f.word(0);
        f.code.mov_eax_mem(address).mov_mem_eax(source_copy);
        let deleted = f.call("gdi32.dll!DeleteObject", &[M(source)]);
        f.run();
        assert_eq!(f.value(source_copy), 0x00010001);
        assert_eq!(f.value(deleted), 1);
        for (list, added, color, error) in cases {
            assert_eq!(
                f.value(added),
                0,
                "color {color:x}, error {}",
                f.value(error)
            );
            let images = &f.list(list).images;
            assert_eq!(images.len(), 2);
            for image in images {
                if color == 0x000000ff {
                    assert_eq!(image.rgba, [0, 255, 0, 255, 0, 0, 0, 0]);
                    assert_eq!(image.mask, Some(vec![0, 1]));
                } else {
                    assert_eq!(image.rgba, [0, 0, 0, 0, 255, 0, 0, 255]);
                    assert_eq!(image.mask, Some(vec![1, 0]));
                }
            }
        }
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
// spec: DEV-IMAGE-14
fn bounded_capacity_and_missing_ddb_palette_report_the_actual_api_without_mutating_lists() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let input = bytes(&mut f, &[1, 2, 3, 255]);
        let indexed_input = bytes(&mut f, &[0x10, 0]);
        let bitmap = f.call(
            "gdi32.dll!CreateBitmap",
            &[I(1), I(1), I(1), I(32), I(input)],
        );
        let indexed = f.call(
            "gdi32.dll!CreateBitmap",
            &[I(2), I(1), I(1), I(4), I(indexed_input)],
        );
        let list = f.call("ImageList_Create", &[I(1), I(1), I(0x21), I(0), I(1)]);
        f.call("ImageList_SetImageCount", &[M(list), I(MAX_ENTRIES as u32)]);
        let exhausted = f.call("ImageList_Add", &[M(list), M(bitmap), I(0)]);
        let exhausted_error = f.error();
        let no_palette = f.call("ImageList_AddMasked", &[M(list), M(indexed), I(0)]);
        let no_palette_error = f.error();
        let count = f.call("ImageList_GetImageCount", &[M(list)]);
        f.run();
        assert_eq!(f.value(exhausted), u32::MAX);
        assert_eq!(f.value(exhausted_error), 8);
        assert_eq!(f.value(no_palette), u32::MAX);
        assert_eq!(f.value(no_palette_error), 50);
        assert_eq!(f.value(count), MAX_ENTRIES as u32);
        let diagnostics = f.machine.ext.get::<adv_abi::ApiDiagnostics>();
        for api in [
            "comctl32.dll!ImageList_Add",
            "comctl32.dll!ImageList_AddMasked",
        ] {
            assert!(
                diagnostics
                    .warnings
                    .keys()
                    .any(|(name, reason)| name == api && !reason.is_empty())
            );
        }
        assert_eq!(diagnostics.warnings.len(), 2);
    }
}
