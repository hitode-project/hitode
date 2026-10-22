//! Public list mutations retain real icon snapshots and independently owned copies.
use super::*;

#[test]
// spec: DEV-IMAGE-12
fn list_counts_resize_removal_and_duplicate_keep_independent_pixel_owners() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        let first = f.icon(2, 1, [10, 20, 30, 255], 0);
        let second = f.icon(2, 1, [40, 50, 60, 128], 1);
        let list = f.call("ImageList_Create", &[I(2), I(1), I(0x21), I(0), I(1)]);
        let empty = f.call("ImageList_GetImageCount", &[M(list)]);
        f.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(first)]);
        f.call("ImageList_ReplaceIcon", &[M(list), I(u32::MAX), I(second)]);
        let previous = f.call("ImageList_SetBkColor", &[M(list), I(0x030201)]);
        let count = f.call("ImageList_GetImageCount", &[M(list)]);
        let copied = f.call("ImageList_Duplicate", &[M(list)]);
        f.run();
        assert_eq!(f.value(empty), 0);
        assert_eq!(f.value(count), 2);
        assert_eq!(f.value(previous), u32::MAX);
        assert_ne!(f.value(copied), f.value(list));
        assert_eq!(f.list(copied).images[0].rgba, [10, 20, 30, 255].repeat(2));
        assert_eq!(f.list(copied).images[1].mask, Some(vec![1, 1]));

        let old_color = f.call("ImageList_SetBkColor", &[M(list), I(0xff0000)]);
        let copied_color = f.call("ImageList_GetBkColor", &[M(copied)]);
        let grown = f.call("ImageList_SetImageCount", &[M(list), I(4)]);
        let grown_count = f.call("ImageList_GetImageCount", &[M(list)]);
        let invalid_size = f.call("ImageList_SetIconSize", &[M(list), I(0), I(1)]);
        let retained_count = f.call("ImageList_GetImageCount", &[M(list)]);
        let resize = f.call("ImageList_SetIconSize", &[M(list), I(2), I(1)]);
        let resized_count = f.call("ImageList_GetImageCount", &[M(list)]);
        let removed = f.call("ImageList_Remove", &[M(copied), I(0)]);
        let bad_remove = f.call("ImageList_Remove", &[M(copied), I(2)]);
        let copied_count = f.call("ImageList_GetImageCount", &[M(copied)]);
        let cleared = f.call("ImageList_Remove", &[M(list), I(u32::MAX)]);
        let destroyed = f.call("ImageList_Destroy", &[M(list)]);
        let still_copied = f.call("ImageList_GetImageCount", &[M(copied)]);
        let invalid_duplicate = f.call("ImageList_Duplicate", &[M(list)]);
        let invalid_resize = f.call("ImageList_SetImageCount", &[M(list), I(1)]);
        let invalid_color = f.call("ImageList_GetBkColor", &[M(list)]);
        f.run();
        for (at, expected) in [
            (old_color, 0x030201),
            (copied_color, 0x030201),
            (grown, 1),
            (grown_count, 4),
            (invalid_size, 0),
            (retained_count, 4),
            (resize, 1),
            (resized_count, 0),
            (removed, 1),
            (bad_remove, 0),
            (copied_count, 1),
            (cleared, 1),
            (destroyed, 1),
            (still_copied, 1),
            (invalid_duplicate, 0),
            (invalid_resize, 0),
            (invalid_color, u32::MAX),
        ] {
            assert_eq!(f.value(at), expected, "result {at:x}");
        }
        let copy = f.list(copied);
        assert_eq!((copy.width, copy.height), (2, 1));
        assert_eq!(copy.images[0].rgba, [40, 50, 60, 128].repeat(2));
        assert_eq!(copy.images[0].mask, Some(vec![1, 1]));
        assert_eq!(copy.background, 0x030201);
        assert!(
            f.machine
                .ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .is_empty()
        );
    }
}
