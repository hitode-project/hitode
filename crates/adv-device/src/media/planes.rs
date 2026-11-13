//! Decoder output is copied once into four reusable YUV presentable buffers.

use super::{Error, VideoFrame};
use adv_gpu::{
    Device, Format, FormatCaps, PresentPool,
    present::{PlaneMut, PoolDesc, YuvColor},
};
use ffmpeg_next::{self as ff, format::Pixel, frame, software};
use std::sync::Arc;

pub(super) struct Output {
    pool: PresentPool,
    pictures: [Arc<crate::frame::DecodedPicture>; 4],
    scaler: Option<software::scaling::Context>,
    source: Pixel,
    color: YuvColor,
    width: u32,
    height: u32,
    format: Format,
}

fn gpu_error(error: adv_gpu::Error) -> Error {
    Error::Decode(error.to_string())
}

pub(super) fn color(space: ff::color::Space, range: ff::color::Range) -> YuvColor {
    match (
        space == ff::color::Space::BT709,
        range == ff::color::Range::JPEG,
    ) {
        (false, false) => YuvColor::Bt601Limited,
        (false, true) => YuvColor::Bt601Full,
        (true, false) => YuvColor::Bt709Limited,
        (true, true) => YuvColor::Bt709Full,
    }
}

impl Output {
    fn build_scaler(
        source: Pixel,
        width: u32,
        height: u32,
        format: Format,
        color: YuvColor,
    ) -> Result<Option<software::scaling::Context>, Error> {
        let mut scaler = (!matches!(
            source,
            Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12 | Pixel::NV21
        ))
        .then(|| {
            software::scaling::Context::get(
                source,
                width,
                height,
                if format == Format::Nv21 {
                    Pixel::NV21
                } else {
                    Pixel::NV12
                },
                width,
                height,
                software::scaling::Flags::BILINEAR,
            )
        })
        .transpose()?;
        if let Some(scaler) = &mut scaler {
            configure(
                scaler,
                color,
                matches!(color, YuvColor::Bt601Full | YuvColor::Bt709Full),
            );
        }
        Ok(scaler)
    }

    pub(super) fn new(
        device: &Device,
        decoder: &ff::decoder::Video,
        source: Pixel,
    ) -> Result<Self, Error> {
        let (width, height) = (decoder.width(), decoder.height());
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(Error::Decode(
                "invalid or excessive video dimensions".into(),
            ));
        }
        let color = color(decoder.color_space(), decoder.color_range());
        let format = [Format::Nv12, Format::Nv21]
            .into_iter()
            .find(|&format| {
                device
                    .caps()
                    .format(format)
                    .contains(FormatCaps::PRESENTABLE)
            })
            .ok_or_else(|| Error::Decode("no presentable YUV format".into()))?;
        let mut pool = device
            .create_present_pool(&PoolDesc {
                width,
                height,
                format,
                count: 4,
                color: Some(color),
                storage: false,
                label: "decoded video",
            })
            .map_err(gpu_error)?;
        // Allocate at open. Every later acquire only leases an existing buffer.
        let buffers: [_; 4] = std::array::from_fn(|_| pool.acquire_cpu().unwrap());
        let pictures = std::array::from_fn(|i| {
            Arc::new(crate::frame::DecodedPicture::new(
                buffers[i].image().clone(),
                color,
            ))
        });
        drop(buffers);
        let scaler = Self::build_scaler(source, width, height, format, color)?;
        Ok(Self {
            pool,
            pictures,
            scaler,
            source,
            color,
            width,
            height,
            format,
        })
    }

    pub(super) fn convert(
        &mut self,
        pts_us: u64,
        decoded: &mut frame::Video,
    ) -> Result<VideoFrame, Error> {
        // The pool and scaler were sized for the stream's first dimensions.
        if (decoded.width(), decoded.height()) != (self.width, self.height) {
            return Err(Error::Decode("video dimensions changed mid-stream".into()));
        }
        if decoded.format() != self.source {
            self.scaler = Self::build_scaler(
                decoded.format(),
                self.width,
                self.height,
                self.format,
                self.color,
            )?;
            self.source = decoded.format();
        }
        for picture in &mut self.pictures {
            if let Some(picture) = Arc::get_mut(picture) {
                picture.release_unshown();
            }
        }
        let mut image = self
            .pool
            .acquire_cpu()
            .ok_or_else(|| Error::Decode("video pool exhausted".into()))?;
        let picture = self
            .pictures
            .iter_mut()
            .find(|p| p.image().id() == image.image().id())
            .unwrap();
        // The compositor released this buffer, but a guest or a queued frame
        // may still hold its last picture; that one keeps its decoder planes.
        if Arc::get_mut(picture).is_none() {
            *picture = Arc::new(crate::frame::DecodedPicture::new(
                image.image().clone(),
                picture.color(),
            ));
        }
        let picture_mut = Arc::get_mut(picture).unwrap();
        image.write(|planes| {
            if let Some(scaler) = &mut self.scaler {
                let input = unsafe { &*decoded.as_ptr() };
                let mut output = [
                    planes[0].data.as_mut_ptr(),
                    planes[1].data.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ];
                let strides = [planes[0].stride as i32, planes[1].stride as i32, 0, 0];
                // The decoder's dimensions and the pool's platform mapping bound every row.
                assert_eq!(
                    unsafe {
                        ff::ffi::sws_scale(
                            scaler.as_mut_ptr(),
                            input.data.as_ptr().cast(),
                            input.linesize.as_ptr(),
                            0,
                            self.height as i32,
                            output.as_mut_ptr(),
                            strides.as_ptr(),
                        )
                    },
                    self.height as i32
                );
            } else {
                copy(decoded, planes, self.format);
            }
        });
        picture_mut.set_image(image, decoded);
        Ok(VideoFrame {
            pts_us,
            width: self.width,
            height: self.height,
            picture: picture.clone(),
        })
    }
}

fn copy(source: &frame::Video, planes: &mut [PlaneMut<'_>], format: Format) {
    let width = source.width() as usize;
    let height = source.height() as usize;
    for y in 0..height {
        let start = y * planes[0].stride;
        planes[0].data[start..start + width]
            .copy_from_slice(&source.data(0)[y * source.stride(0)..][..width]);
    }
    let chroma = width.div_ceil(2);
    let vu = format == Format::Nv21;
    match source.format() {
        Pixel::NV12 | Pixel::NV21 => {
            let swap = (source.format() == Pixel::NV21) != vu;
            for y in 0..height.div_ceil(2) {
                let start = y * planes[1].stride;
                let dst = &mut planes[1].data[start..start + chroma * 2];
                let src = &source.data(1)[y * source.stride(1)..][..chroma * 2];
                if swap {
                    swap_chroma(src, dst);
                } else {
                    dst.copy_from_slice(src);
                }
            }
        }
        _ => {
            let (first, second) = if vu { (2, 1) } else { (1, 2) };
            for y in 0..height.div_ceil(2) {
                let start = y * planes[1].stride;
                interleave(
                    &source.data(first)[y * source.stride(first)..][..chroma],
                    &source.data(second)[y * source.stride(second)..][..chroma],
                    &mut planes[1].data[start..start + chroma * 2],
                );
            }
        }
    }
}

fn scalar(u: &[u8], v: &[u8], dst: &mut [u8]) {
    for ((u, v), pair) in u.iter().zip(v).zip(dst.as_chunks_mut::<2>().0.iter_mut()) {
        pair.copy_from_slice(&[*u, *v]);
    }
}

fn interleave(u: &[u8], v: &[u8], dst: &mut [u8]) {
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        let end = u.len() / 16 * 16;
        for x in (0..end).step_by(16) {
            // SAFETY: AArch64 always has NEON. These loads and stores stay in the row.
            unsafe {
                vst2q_u8(
                    dst.as_mut_ptr().add(x * 2),
                    uint8x16x2_t(vld1q_u8(u.as_ptr().add(x)), vld1q_u8(v.as_ptr().add(x))),
                );
            }
        }
        scalar(&u[end..], &v[end..], &mut dst[end * 2..]);
    }
    #[cfg(not(target_arch = "aarch64"))]
    scalar(u, v, dst);
}

fn swap_chroma(src: &[u8], dst: &mut [u8]) {
    #[cfg(not(target_arch = "aarch64"))]
    let end = 0;
    #[cfg(target_arch = "aarch64")]
    let end = src.len() / 16 * 16;
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        for x in (0..end).step_by(16) {
            // SAFETY: NEON is baseline; each load and store stays within the row.
            unsafe {
                vst1q_u8(
                    dst.as_mut_ptr().add(x),
                    vrev16q_u8(vld1q_u8(src.as_ptr().add(x))),
                );
            }
        }
    }
    for (src, dst) in src[end..]
        .as_chunks::<2>()
        .0
        .iter()
        .zip(dst[end..].as_chunks_mut::<2>().0.iter_mut())
    {
        *dst = [src[1], src[0]];
    }
}

fn configure(scaler: &mut software::scaling::Context, color: YuvColor, destination_full: bool) {
    let full = matches!(color, YuvColor::Bt601Full | YuvColor::Bt709Full);
    let matrix = match color {
        YuvColor::Bt601Limited | YuvColor::Bt601Full => ff::ffi::SWS_CS_ITU601,
        YuvColor::Bt709Limited | YuvColor::Bt709Full => ff::ffi::SWS_CS_ITU709,
    };
    unsafe {
        let coefficients = ff::ffi::sws_getCoefficients(matrix);
        assert_eq!(
            ff::ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                coefficients,
                i32::from(full),
                coefficients,
                i32::from(destination_full),
                0,
                1 << 16,
                1 << 16
            ),
            0
        );
    }
}

/// The single guest-pixel conversion. swscale reads the retained decoder
/// planes directly; no BGRA exists on the display path.
pub(crate) fn bgra(source: &frame::Video, color: YuvColor) -> Vec<u8> {
    let (width, height) = (source.width(), source.height());
    let mut scaler = software::scaling::Context::get(
        source.format(),
        width,
        height,
        Pixel::BGRA,
        width,
        height,
        software::scaling::Flags::BILINEAR,
    )
    .expect("guest BGRA scaler");
    configure(&mut scaler, color, true);
    let mut out = vec![0; width as usize * height as usize * 4];
    unsafe {
        let src = &*source.as_ptr();
        let mut dst = [
            out.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ];
        let strides = [width as i32 * 4, 0, 0, 0];
        assert_eq!(
            ff::ffi::sws_scale(
                scaler.as_mut_ptr(),
                src.data.as_ptr().cast(),
                src.linesize.as_ptr(),
                0,
                height as i32,
                dst.as_mut_ptr(),
                strides.as_ptr()
            ),
            height as i32
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: MEDIA-VIDEO-7
    fn decoder_copy_writes_both_chroma_orders_without_touching_padding() {
        for width in [1usize, 3, 17, 31, 33, 65] {
            for source_format in [Pixel::YUV420P, Pixel::NV12, Pixel::NV21] {
                let mut source = frame::Video::new(source_format, width as u32, 5);
                let y_value = |x: usize, y: usize| (x * 3 + y * 7) as u8;
                let u_value = |x: usize, y: usize| (x * 11 + y * 3 + 17) as u8;
                let v_value = |x: usize, y: usize| (x * 7 + y * 13 + 91) as u8;
                for y in 0..5 {
                    for x in 0..width {
                        let at = y * source.stride(0) + x;
                        source.data_mut(0)[at] = y_value(x, y);
                    }
                }
                for y in 0..3 {
                    for x in 0..width.div_ceil(2) {
                        if source_format == Pixel::YUV420P {
                            for (plane, value) in [(1, u_value(x, y)), (2, v_value(x, y))] {
                                let at = y * source.stride(plane) + x;
                                source.data_mut(plane)[at] = value;
                            }
                        } else {
                            let pair = if source_format == Pixel::NV21 {
                                [v_value(x, y), u_value(x, y)]
                            } else {
                                [u_value(x, y), v_value(x, y)]
                            };
                            let at = y * source.stride(1) + x * 2;
                            source.data_mut(1)[at..at + 2].copy_from_slice(&pair);
                        }
                    }
                }
                for format in [Format::Nv12, Format::Nv21] {
                    let stride = width + 7;
                    let chroma_stride = width.div_ceil(2) * 2 + 9;
                    let mut y_plane = vec![0xcd; stride * 5];
                    let mut chroma = vec![0xcd; chroma_stride * 3];
                    copy(
                        &source,
                        &mut [
                            PlaneMut {
                                data: &mut y_plane,
                                stride,
                            },
                            PlaneMut {
                                data: &mut chroma,
                                stride: chroma_stride,
                            },
                        ],
                        format,
                    );
                    for y in 0..5 {
                        for x in 0..width {
                            assert_eq!(y_plane[y * stride + x], y_value(x, y));
                        }
                        assert!(
                            y_plane[y * stride + width..(y + 1) * stride]
                                .iter()
                                .all(|&b| b == 0xcd)
                        );
                    }
                    for y in 0..3 {
                        for x in 0..width.div_ceil(2) {
                            let expected = if format == Format::Nv21 {
                                [v_value(x, y), u_value(x, y)]
                            } else {
                                [u_value(x, y), v_value(x, y)]
                            };
                            assert_eq!(
                                &chroma[y * chroma_stride + x * 2..][..2],
                                &expected,
                                "{source_format:?} -> {format:?}, width {width}"
                            );
                        }
                        assert!(
                            chroma[y * chroma_stride + width.div_ceil(2) * 2
                                ..(y + 1) * chroma_stride]
                                .iter()
                                .all(|&b| b == 0xcd)
                        );
                    }
                }
            }
        }
    }
    #[test]
    // spec: MEDIA-VIDEO-7
    fn chroma_interleave_odd_widths_and_strides() {
        for width in [1usize, 3, 17, 31, 33, 65] {
            let n = width.div_ceil(2);
            for padding in [0, 1, 15] {
                let stride = n + padding;
                let output_stride = n * 2 + padding;
                let u: Vec<_> = (0..stride * 3).map(|x| (x * 7) as u8).collect();
                let v: Vec<_> = (0..stride * 3).map(|x| (x * 11 + 1) as u8).collect();
                let mut expected = vec![0xcd; output_stride * 3];
                let mut actual = expected.clone();
                for y in 0..3 {
                    scalar(
                        &u[y * stride..][..n],
                        &v[y * stride..][..n],
                        &mut expected[y * output_stride..][..n * 2],
                    );
                    interleave(
                        &u[y * stride..][..n],
                        &v[y * stride..][..n],
                        &mut actual[y * output_stride..][..n * 2],
                    );
                }
                assert_eq!(actual, expected, "width {width}, padding {padding}");
            }
        }
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    #[test]
    // spec: MEDIA-VIDEO-7
    fn guest_bgra_matches_swscale_for_both_matrices_and_ranges() {
        for color in [
            YuvColor::Bt601Limited,
            YuvColor::Bt601Full,
            YuvColor::Bt709Limited,
            YuvColor::Bt709Full,
        ] {
            let mut source = frame::Video::new(Pixel::YUV420P, 18, 10);
            for plane in 0..3 {
                let (w, h) = if plane == 0 { (18, 10) } else { (9, 5) };
                let stride = source.stride(plane);
                for y in 0..h {
                    for x in 0..w {
                        source.data_mut(plane)[y * stride + x] =
                            (35 + (x * 13 + y * 9 + plane * 41) % 190) as u8;
                    }
                }
            }
            let actual = bgra(&source, color);
            let mut reference = software::scaling::Context::get(
                Pixel::YUV420P,
                18,
                10,
                Pixel::BGRA,
                18,
                10,
                software::scaling::Flags::BILINEAR,
            )
            .unwrap();
            unsafe {
                let coefficients = ff::ffi::sws_getCoefficients(
                    if matches!(color, YuvColor::Bt709Limited | YuvColor::Bt709Full) {
                        ff::ffi::SWS_CS_ITU709
                    } else {
                        ff::ffi::SWS_CS_ITU601
                    },
                );
                assert_eq!(
                    ff::ffi::sws_setColorspaceDetails(
                        reference.as_mut_ptr(),
                        coefficients,
                        i32::from(matches!(color, YuvColor::Bt601Full | YuvColor::Bt709Full)),
                        coefficients,
                        1,
                        0,
                        65536,
                        65536
                    ),
                    0
                );
            }
            let mut result = frame::Video::empty();
            reference.run(&source, &mut result).unwrap();
            for row in 0..10 {
                assert_eq!(
                    &actual[row * 72..][..72],
                    &result.data(0)[row * result.stride(0)..][..72],
                    "{color:?} row {row}"
                );
            }
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod ownership_tests {
    use crate::frame::{VideoGeometry, VideoPlane, VideoUpdate};
    #[test]
    // spec: MEDIA-VIDEO-7
    fn decoded_pictures_reuse_four_buffers_and_transfer_once() {
        let mut decoder = crate::media::Decoder::open_memory(
            include_bytes!("../../../adv-dx/src/dshow/graph/render/fixtures/guest_movie.mpg")
                .to_vec(),
        )
        .unwrap();
        let mut ids = std::collections::HashSet::new();
        // Shown frames a guest keeps, with their snapshots at the time.
        let mut held = vec![];
        for time in (0..1_000_000).step_by(40_000) {
            let Some(frame) = decoder.frame_at(time).unwrap().video else {
                continue;
            };
            ids.insert(frame.picture.image().id());
            held.push((frame.picture.clone(), frame.picture.bgra()));
            let plane = VideoPlane {
                id: 7,
                picture: frame.picture.clone(),
                geometry: VideoGeometry {
                    source: (frame.width, frame.height),
                    destination: adv_gpu::Rect {
                        x: 0,
                        y: 0,
                        w: frame.width,
                        h: frame.height,
                    },
                    source_uv: [0.0, 0.0, 1.0, 1.0],
                    background: None,
                    clip: adv_gpu::Rect {
                        x: 0,
                        y: 0,
                        w: frame.width,
                        h: frame.height,
                    },
                    hide_cursor: false,
                },
            };
            let VideoUpdate::Changed(image) = plane.take_picture() else {
                panic!("first handoff");
            };
            assert!(matches!(plane.take_picture(), VideoUpdate::Retained));
            assert!(matches!(plane.take_picture(), VideoUpdate::Retained));
            image.released(None);
        }
        assert!(!ids.is_empty());
        assert!(ids.len() <= 4);
        assert!(held.len() > 4);
        for (picture, bgra) in &held {
            assert_eq!(&picture.bgra(), bgra);
        }
    }

    #[test]
    // spec: MEDIA-VIDEO-7
    fn h264_mp4_decodes_through_videotoolbox_into_nv12_planes() {
        let mp4 =
            include_bytes!("../../../adv-runtime/tests/fixtures/media/synthetic.mp4").to_vec();
        let mut decoder = crate::media::Decoder::open_memory(mp4.clone()).unwrap();
        let mut frames = 0;
        for time in (0..600_000).step_by(100_000) {
            if let Some(frame) = decoder.frame_at(time).unwrap().video {
                assert_eq!((frame.width, frame.height), (64, 48));
                assert_eq!(frame.picture.image().desc().format, adv_gpu::Format::Nv12);
                assert_eq!(frame.bgra().len(), 64 * 48 * 4);
                frames += 1;
            }
        }
        assert!(frames >= 3);

        let input = crate::media::demux(mp4).unwrap();
        let stream = input
            .streams
            .iter()
            .find(|s| matches!(s.format, crate::media::CodecFormat::Video { .. }))
            .unwrap();
        let mut packet_decoder = crate::media::PacketDecoder::open(&stream.format).unwrap();
        let mut pts = Vec::new();
        for packet in &stream.packets {
            for decoded in packet_decoder
                .decode(&packet.data, Some(packet.time_100ns))
                .unwrap()
            {
                if let crate::media::Decoded::Video(frame) = decoded {
                    pts.push(frame.pts_us);
                }
            }
        }
        for decoded in packet_decoder.drain().unwrap() {
            if let crate::media::Decoded::Video(frame) = decoded {
                pts.push(frame.pts_us);
            }
        }
        assert!(pts.len() >= 3);
        assert!(pts.iter().any(|&t| t > 0));

        // Verify a 720p H.264 stream exercises the live VideoToolbox hardware session when ffmpeg CLI is present.
        let ffmpeg = std::env::var_os("FFMPEG_DIR")
            .map(|d| std::path::PathBuf::from(d).join("bin/ffmpeg"))
            .unwrap_or_else(|| "ffmpeg".into());
        let tmp = std::env::temp_dir().join(format!("adv-vt-720p-{}.mp4", std::process::id()));
        if std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=30:duration=0.2",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
            ])
            .arg(&tmp)
            .status()
            .is_ok_and(|s| s.success())
        {
            let bytes = std::fs::read(&tmp).unwrap();
            let _ = std::fs::remove_file(&tmp);
            let mut hd = crate::media::Decoder::open_memory(bytes).unwrap();
            let frame = hd.frame_at(100_000).unwrap().video.expect("720p frame");
            assert_eq!((frame.width, frame.height), (1280, 720));
            assert_eq!(frame.picture.image().desc().format, adv_gpu::Format::Nv12);
        }
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod packet_tests {
    use crate::media::{CodecFormat, Decoded, PacketDecoder, demux};
    #[test]
    // spec: MEDIA-VIDEO-7
    fn packet_video_decodes_into_reusable_planes() {
        let input = demux(include_bytes!("packets/ramp.wmv").to_vec()).unwrap();
        let stream = input
            .streams
            .iter()
            .find(|s| matches!(s.format, CodecFormat::Video { .. }))
            .unwrap();
        let mut decoder = PacketDecoder::open(&stream.format).unwrap();
        let mut count = 0;
        for packet in &stream.packets {
            for decoded in decoder
                .decode(&packet.data, Some(packet.time_100ns))
                .unwrap()
            {
                let Decoded::Video(frame) = decoded else {
                    panic!("video stream");
                };
                assert_eq!((frame.width, frame.height), (64, 48));
                assert_eq!(frame.picture.image().desc().format, adv_gpu::Format::Nv12);
                assert_eq!(frame.bgra().len(), 64 * 48 * 4);
                count += 1;
            }
        }
        for decoded in decoder.drain().unwrap() {
            assert!(matches!(decoded, Decoded::Video(_)));
            count += 1;
        }
        assert_eq!(count, 10);
    }
}
