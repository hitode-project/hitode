//! D3DX filter selection; pixel computation belongs to the device layer.
use adv_device::image::{self, Filter};

fn operation(filter: u32) -> Filter {
    match filter {
        1 => Filter::Copy,
        2 => Filter::Point,
        _ => Filter::LinearWrap,
    }
}

pub(super) fn pixels(input: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, filter: u32) -> Vec<u8> {
    image::resize(input, sw, sh, dw, dh, operation(filter))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d3dx_filters_select_the_original_device_operations() {
        assert_eq!(operation(1), Filter::Copy);
        assert_eq!(operation(2), Filter::Point);
        assert_eq!(operation(3), Filter::LinearWrap);
    }
}
