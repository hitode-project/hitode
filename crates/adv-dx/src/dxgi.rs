//! DXGI factory, adapter and output. Assembly supplies display facts at query
//! time. The front is installed explicitly and is independent of DLL serving.
#![deny(clippy::todo, clippy::unimplemented, clippy::wildcard_enum_match_arm)]

mod abi;
pub mod caps;
mod desc;
pub mod formats;
mod glue;
mod object;
mod ops;
mod state;
pub mod swap;

pub use glue::{
    adapter_kind, create_device_adapter, create_device_factory, install, query_adapter, register,
};
pub use glue::{create_device_swap_chain, device_output, install_swap_chains};
pub use state::{DeviceCall, DeviceOperation, Devices, Provider, shared_display};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    InvalidCall,
    NoInterface,
    NotFound,
    MoreData,
    Unsupported,
    Unavailable,
    OutOfMemory,
    FrameStatisticsDisjoint,
}

impl Error {
    pub fn hresult(self) -> u32 {
        match self {
            Self::InvalidArgument => 0x80070057,
            Self::InvalidCall => 0x887a0001,
            Self::NoInterface => 0x80004002,
            Self::NotFound => 0x887a0002,
            Self::MoreData => 0x887a0003,
            Self::Unsupported => 0x887a0004,
            Self::Unavailable => 0x887a0022,
            Self::OutOfMemory => 0x8007000e,
            Self::FrameStatisticsDisjoint => 0x887a000b,
        }
    }
}

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
