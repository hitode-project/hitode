//! Data shared by DXGI format consumers.
pub mod formats;

#[cfg(test)]
mod tests {
    use super::formats::{FORMATS, format_layout};

    #[test]
    fn public_formats_and_cast_families_match_the_abi() {
        let enums = adv_abi::constants::for_type("Windows.Win32.Graphics.Dxgi.Common.DXGI_FORMAT");
        assert_eq!(enums.len(), FORMATS.len());
        for constant in enums {
            let row = format_layout(constant.value as u32).expect(constant.name);
            assert_eq!(row.name, constant.name);
            assert!(format_layout(row.parent).is_some());
            for &cast in row.cast_family {
                let other = format_layout(cast).unwrap();
                assert_eq!(other.unit_bits, row.unit_bits, "{}", row.name);
                assert_eq!(other.alignment, row.alignment, "{}", row.name);
            }
            if row.name.starts_with("DXGI_FORMAT_BC") {
                assert_eq!(row.alignment, [4, 4, 1], "{}", row.name);
                assert!(matches!(row.unit_bits, 64 | 128), "{}", row.name);
                assert_eq!(row.aligned_unit_bytes, row.unit_bits / 8, "{}", row.name);
            }
        }
    }
}
