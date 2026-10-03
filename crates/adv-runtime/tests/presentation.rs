#[path = "../examples/support/pipeline.rs"]
mod pipeline;
use adv_cpu::RunOutcome;
use adv_runtime::api;
use pipeline::*;

#[test]
fn deferred_capture_keeps_latest_complete_pixels_without_holding_guest_surfaces() {
    for mailbox in [false, true] {
        let mut f = fixture(true, 4, 2, false);
        let (producer, mut consumer) = adv_runtime::frame_mailbox();
        if mailbox {
            f.graphics
                .set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
        } else {
            f.graphics.set_deferred_presentation(true).unwrap();
        }
        f.machine
            .run_with_services(STOP, 100_000, &mut f.graphics)
            .unwrap();
        assert_eq!(f.graphics.stats.presentations, 1);
        assert_eq!(f.graphics.presenter.frames, 0);
        let source = u32_at(&f.machine, SURFACE_OUT);
        let primary = u32_at(&f.machine, PRIMARY_OUT);
        assert_eq!(invoke(&mut f, api::LOCK, &[source, 0, LOCK_DESC, 0, 0]), 0);
        let pixels = u32_at(&f.machine, LOCK_DESC + 36);
        let latest = vec![0x37; 32];
        f.machine.write(pixels, &latest).unwrap();
        assert_eq!(invoke(&mut f, api::UNLOCK, &[source, 0]), 0);
        assert_eq!(invoke(&mut f, api::BLT, &[primary, 0, source, 0, 0, 0]), 0);
        assert_eq!(f.graphics.stats.presentations, 2);
        assert_eq!(f.graphics.stats.host_captures, 0);
        assert_eq!(f.graphics.stats.coalesced_frames, 1);
        // Remember the primary's storage, then finish the lock without changing it.
        assert_eq!(invoke(&mut f, api::LOCK, &[primary, 0, LOCK_DESC, 0, 0]), 0);
        let front = u32_at(&f.machine, LOCK_DESC + 36);
        assert_eq!(invoke(&mut f, api::UNLOCK, &[primary, 0]), 0);
        // Unlock of a primary is itself a completed presentation in this model.
        assert_eq!(f.graphics.stats.presentations, 3);
        // A later memory write is not a completed frame. Releasing the surface must
        // not make the frontend observe this partially modified/recycled storage.
        f.machine.write(front, &[0xff; 32]).unwrap();
        assert_eq!(invoke(&mut f, api::RELEASE, &[source]), 0);
        assert_eq!(invoke(&mut f, api::RELEASE, &[primary]), 0);
        assert!(f.graphics.capture_latest().unwrap());
        assert_eq!(f.graphics.presenter.bgra, latest);
        assert_eq!(f.graphics.presenter.frames, 3);
        assert!(!f.graphics.capture_latest().unwrap());
        assert_eq!(f.graphics.stats.host_captures, 1);
        if mailbox {
            assert!(consumer.update());
            let mut displayed = adv_runtime::Snapshot::default();
            consumer.current().snapshot(&mut displayed).unwrap();
            assert_eq!(displayed.bgra, latest);
            assert_eq!(displayed.frames, 3);
            assert!(!consumer.update());
        }
    }
}

#[test]
fn gdi_and_directdraw_render_guest_pixels_to_the_same_presenter() {
    for ddraw in [false, true] {
        for unicorn in [false, true] {
            let mut f = fixture(ddraw, 64, 32, false);
            let result = if unicorn {
                f.machine
                    .run_unicorn_with_services(STOP, 100_000, &mut f.graphics)
            } else {
                f.machine.run_with_services(STOP, 100_000, &mut f.graphics)
            };
            assert_eq!(result.unwrap(), RunOutcome::ReachedStop);
            assert_eq!(f.graphics.presenter.bgra, f.expected);
            assert_eq!(f.graphics.presenter.frames, 1);
            assert_eq!(f.graphics.stats.bytes_blitted, 64 * 32 * 4);
            if !unicorn {
                assert_eq!(f.machine.stats.interpreter_entries, 0);
                assert_eq!(f.machine.stats.native_entries, 1);
            }
            if ddraw {
                assert_eq!(f.graphics.stats.locks, 1);
                assert_eq!(f.graphics.stats.unlocks, 1);
            }
        }
    }
}

#[test]
fn bottom_up_dib_is_presented_with_the_correct_row_order() {
    let mut f = fixture(false, 64, 32, true);
    assert_eq!(
        f.machine
            .run_with_services(STOP, 100_000, &mut f.graphics)
            .unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(f.graphics.presenter.bgra, f.expected);
}

#[test]
fn selected_bitmaps_cannot_be_deleted_and_handles_can_be_released() {
    let mut f = fixture(false, 64, 32, false);
    f.machine
        .run_with_services(STOP, 100_000, &mut f.graphics)
        .unwrap();
    let bitmap = u32_at(&f.machine, BITMAP_OUT);
    let dc = u32_at(&f.machine, DC_OUT);
    let old = u32_at(&f.machine, OLD_BITMAP);
    assert_eq!(invoke(&mut f, api::DELETE_OBJECT, &[bitmap]), 0);
    assert_eq!(invoke(&mut f, api::SELECT, &[dc, old]), bitmap);
    assert_eq!(invoke(&mut f, api::DELETE_OBJECT, &[bitmap]), 1);
    assert_eq!(invoke(&mut f, api::DELETE_OBJECT, &[bitmap]), 0);
    assert_eq!(invoke(&mut f, api::DELETE_DC, &[dc]), 1);
    assert_eq!(f.graphics.live_surfaces(), 1);
}

#[test]
fn directdraw_locking_enforces_busy_and_unlock_states() {
    let mut f = fixture(true, 64, 32, false);
    f.machine
        .run_with_services(STOP, 100_000, &mut f.graphics)
        .unwrap();
    let surface = u32_at(&f.machine, SURFACE_OUT);
    let primary = u32_at(&f.machine, PRIMARY_OUT);
    assert_eq!(invoke(&mut f, api::UNLOCK, &[surface, 0]), 0x88760248);
    assert_eq!(invoke(&mut f, api::LOCK, &[surface, 0, LOCK_DESC, 0, 0]), 0);
    assert_eq!(
        invoke(&mut f, api::LOCK, &[surface, 0, LOCK_DESC, 0, 0]),
        0x887601ae
    );
    assert_eq!(
        invoke(&mut f, api::BLT, &[primary, 0, surface, 0, 0, 0]),
        0x887601ae
    );
    assert_eq!(f.graphics.presenter.frames, 1);
    assert_eq!(invoke(&mut f, api::UNLOCK, &[surface, 0]), 0);
    assert_eq!(invoke(&mut f, api::BLT, &[primary, 0, surface, 0, 0, 0]), 0);
    assert_eq!(f.graphics.presenter.frames, 2);
    assert_eq!(invoke(&mut f, api::RELEASE, &[surface]), 0);
    assert_eq!(
        invoke(&mut f, api::LOCK, &[surface, 0, LOCK_DESC, 0, 0]),
        0x80070057
    );
}

#[test]
fn clipped_and_overlapping_gdi_blits_preserve_pixels() {
    for bottom_up in [false, true] {
        let mut f = fixture(false, 64, 32, bottom_up);
        f.machine
            .run_with_services(STOP, 100_000, &mut f.graphics)
            .unwrap();
        let dc = u32_at(&f.machine, DC_OUT);
        let pixels = u32_at(&f.machine, DATA + 0x200);
        let mut expected = f
            .machine
            .memory()
            .read(pixels, 64 * 32 * 4)
            .unwrap()
            .to_vec();
        let original = expected.clone();
        for y in 0..31usize {
            let row = if bottom_up { 31 - y } else { y };
            let to = if bottom_up { 30 - y } else { y + 1 };
            expected[to * 256 + 4..to * 256 + 256]
                .copy_from_slice(&original[row * 256..row * 256 + 252]);
        }
        assert_eq!(
            invoke(
                &mut f,
                api::BITBLT,
                &[dc, 1, 1, 63, 31, dc, 0, 0, 0x00cc0020]
            ),
            1
        );
        assert_eq!(f.machine.memory().read(pixels, 8192).unwrap(), expected);
        let mut shown = f.graphics.presenter.bgra.clone();
        for y in 0..2usize {
            let row = if bottom_up { 31 - y } else { y };
            shown[y * 256..y * 256 + 16].copy_from_slice(&expected[row * 256 + 8..row * 256 + 24]);
        }
        assert_eq!(
            invoke(
                &mut f,
                api::BITBLT,
                &[1, (-2i32) as u32, 0, 6, 2, dc, 0, 0, 0x00cc0020]
            ),
            1
        );
        assert_eq!(f.graphics.presenter.bgra, shown);
    }
}

#[test]
fn directdraw_subrect_pitch_and_com_identity_are_preserved() {
    let mut f = fixture(true, 64, 32, false);
    f.machine
        .run_with_services(STOP, 100_000, &mut f.graphics)
        .unwrap();
    let surface = u32_at(&f.machine, SURFACE_OUT);
    let pixels = u32_at(&f.machine, LOCK_DESC + 36);
    let rect = DATA + 0x400;
    f.machine
        .write(
            rect,
            &[2u32, 3, 6, 5]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(
        invoke(&mut f, api::LOCK, &[surface, rect, LOCK_DESC, 0, 0]),
        0
    );
    assert_eq!(u32_at(&f.machine, LOCK_DESC + 36), pixels + 3 * 256 + 8);
    assert_eq!(u32_at(&f.machine, LOCK_DESC + 16), 256);
    assert_eq!(invoke(&mut f, api::UNLOCK, &[surface, rect]), 0);
    f.machine
        .write(DATA + 0x440, &adv_runtime::IID_SURFACE7)
        .unwrap();
    assert_eq!(
        invoke(&mut f, api::QUERY, &[surface, DATA + 0x440, DATA + 0x450]),
        0
    );
    assert_eq!(u32_at(&f.machine, DATA + 0x450), surface);
    assert_eq!(invoke(&mut f, api::RELEASE, &[surface]), 1);
    let device = u32_at(&f.machine, DATA + 0x200);
    f.machine
        .write(DATA + 0x44, &(7u32 | 0x80000).to_le_bytes())
        .unwrap();
    assert_eq!(
        invoke(
            &mut f,
            api::CREATE_SURFACE,
            &[device, DATA + 0x40, DATA + 0x454, 0]
        ),
        0x80004001
    );
    assert_eq!(u32_at(&f.machine, DATA + 0x454), 0);
    assert_eq!(f.graphics.live_surfaces(), 2);
}

#[test]
fn legacy_directdraw_16_bit_backbuffer_flips_to_bgra() {
    for deferred in [false, true] {
        let mut f = fixture(true, 4, 2, false);
        f.graphics.set_deferred_presentation(deferred).unwrap();
        let out = DATA + 0x500;
        assert_eq!(invoke(&mut f, api::DD_CREATE_OLD, &[0, out, 0]), 0);
        let device = u32_at(&f.machine, out);
        assert_eq!(
            invoke(&mut f, api::DISPLAY_MODE_OLD, &[device, 4, 2, 16]),
            0
        );
        let table = u32_at(&f.machine, device);
        assert_eq!(
            u32_at(&f.machine, table + 21 * 4),
            f.entries[&api::DISPLAY_MODE_OLD]
        );
        let desc = DATA + 0x540;
        let mut bytes = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 0x21), (20, 1), (104, 0x218)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        f.machine.write(desc, &bytes).unwrap();
        assert_eq!(
            invoke(&mut f, api::CREATE_SURFACE, &[device, desc, out, 0]),
            0
        );
        let primary = u32_at(&f.machine, out);
        f.machine.write(desc, &4u32.to_le_bytes()).unwrap();
        assert_eq!(invoke(&mut f, api::GET_ATTACHED, &[primary, desc, out]), 0);
        let back = u32_at(&f.machine, out);
        f.machine.write(desc, &108u32.to_le_bytes()).unwrap();
        assert_eq!(invoke(&mut f, api::LOCK, &[back, 0, desc, 0, 0]), 0);
        assert_eq!(u32_at(&f.machine, desc + 16), 8);
        assert_eq!(u32_at(&f.machine, desc + 84), 16);
        let pixels = u32_at(&f.machine, desc + 36);
        let colors = [0xf800u16, 0x07e0, 0x001f, 0xffff, 0, 0xf800, 0x07e0, 0x001f];
        f.machine
            .write(
                pixels,
                &colors
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert_eq!(invoke(&mut f, api::UNLOCK, &[back, pixels]), 0);
        assert_eq!(invoke(&mut f, api::FLIP, &[primary, 0, 1]), 0);
        if deferred {
            assert_eq!(f.graphics.presenter.frames, 0);
            assert_eq!(f.graphics.stats.converted_pixels, 0);
            assert!(f.graphics.capture_latest().unwrap());
            assert_eq!(f.graphics.stats.converted_pixels, 8);
        }
        assert_eq!(f.graphics.presenter.frames, 1);
        assert_eq!(
            f.graphics.presenter.bgra,
            vec![
                0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255,
                0, 0, 255, 0, 0, 255, 0, 0, 0
            ]
        );
    }
}

#[test]
fn directdraw_bltfast_and_surface_dc_share_the_same_pixels() {
    let mut f = fixture(true, 4, 2, false);
    f.machine
        .run_with_services(STOP, 100_000, &mut f.graphics)
        .unwrap();
    let surface = u32_at(&f.machine, SURFACE_OUT);
    let primary = u32_at(&f.machine, PRIMARY_OUT);
    assert_eq!(
        invoke(&mut f, api::BLT_FAST, &[primary, 0, 0, surface, 0, 0x10]),
        0
    );
    assert_eq!(f.graphics.presenter.bgra, f.expected);
    let out = DATA + 0x500;
    assert_eq!(invoke(&mut f, api::SURFACE_DC, &[primary, out]), 0);
    let primary_dc = u32_at(&f.machine, out);
    assert_eq!(invoke(&mut f, api::SURFACE_DC, &[surface, out]), 0);
    let source_dc = u32_at(&f.machine, out);
    assert_eq!(
        invoke(
            &mut f,
            api::BITBLT,
            &[primary_dc, 0, 0, 4, 2, source_dc, 0, 0, 0x00cc0020]
        ),
        1
    );
    assert_eq!(
        invoke(&mut f, api::SURFACE_RELEASE_DC, &[primary, primary_dc]),
        0
    );
    assert_eq!(f.graphics.presenter.bgra, f.expected);
}
