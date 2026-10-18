//! D3DX source metadata mapping over device-layer image decoding.
use super::ImageInfo;
use adv_device::image::{self, FileFormat, Format};

pub(super) use image::{Error, LIMIT};
type Result<T> = std::result::Result<T, Error>;

fn api_info(image: image::Info) -> ImageInfo {
    let format = match image.format {
        Format::Rgb8 if image.file_format == FileFormat::Png => 22,
        Format::Rgb8 => 20,
        Format::Rgbx8 => 22,
        Format::Rgba8 => 21,
        Format::Indexed8 => 41,
        Format::Luma8 => 50,
        Format::LumaAlpha8 => 51,
        Format::Rgb565 => 23,
        Format::Rgba4444 => 26,
        Format::Bc1 => 0x31545844,
        Format::Bc2 => 0x33545844,
        Format::Bc3 => 0x35545844,
    };
    ImageInfo {
        width: image.width,
        height: image.height,
        depth: 1,
        mips: image.mips,
        format,
        resource_type: 3,
        file_format: match image.file_format {
            FileFormat::Bmp => 0,
            FileFormat::Png => 3,
            FileFormat::Dds => 4,
        },
    }
}

pub(super) fn inspect(bytes: &[u8]) -> Result<ImageInfo> {
    image::inspect(bytes).map(api_info)
}

#[cfg(feature = "gpu")]
pub(super) fn decode(bytes: &[u8], image: ImageInfo) -> Result<Vec<u8>> {
    let file_format = match image.file_format {
        0 => FileFormat::Bmp,
        3 => FileFormat::Png,
        4 => FileFormat::Dds,
        _ => return Err(Error::Invalid),
    };
    image::decode(bytes, file_format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_formats_keep_d3dx_metadata_values_and_layout() {
        for (file_format, source, format) in [
            (FileFormat::Png, Format::Rgb8, 22),
            (FileFormat::Png, Format::Rgba8, 21),
            (FileFormat::Png, Format::Indexed8, 41),
            (FileFormat::Png, Format::Luma8, 50),
            (FileFormat::Png, Format::LumaAlpha8, 51),
            (FileFormat::Bmp, Format::Rgb8, 20),
            (FileFormat::Bmp, Format::Rgba8, 21),
            (FileFormat::Dds, Format::Rgb8, 20),
            (FileFormat::Dds, Format::Rgbx8, 22),
            (FileFormat::Dds, Format::Rgba8, 21),
            (FileFormat::Dds, Format::Rgb565, 23),
            (FileFormat::Dds, Format::Rgba4444, 26),
            (FileFormat::Dds, Format::Bc1, 0x31545844),
            (FileFormat::Dds, Format::Bc2, 0x33545844),
            (FileFormat::Dds, Format::Bc3, 0x35545844),
        ] {
            let info = api_info(image::Info {
                width: 4,
                height: 3,
                mips: 2,
                format: source,
                file_format,
            });
            let file = match file_format {
                FileFormat::Bmp => 0,
                FileFormat::Png => 3,
                FileFormat::Dds => 4,
            };
            let expected: Vec<_> = [4, 3, 1, 2, format, 3, file]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            assert_eq!(bytemuck::bytes_of(&info), expected);
        }
    }
}
