//! Packet-level Theora decoding. The caller owns stream I/O and presentation time.
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub keyframe_num: i64,
    pub curframe_num: i64,
    pub granpos: i64,
    pub pp_level: i32,
    pub frame_type: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawPlane {
    width: i32,
    height: i32,
    stride: i32,
    data: *const u8,
}

#[repr(C)]
#[derive(Default)]
struct RawFrame {
    refs: [i32; 3],
    frame_type: i32,
    planes: [RawPlane; 3],
}

/// Tightly packed rows in the decoder's internal plane orientation.
#[derive(Default)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Parsed initialization tables, independent of guest pointer width.
pub struct Setup {
    pub info: [u8; 64],
    pub huffman: [Vec<i16>; 80],
    pub dc_scale: [u16; 64],
    pub ac_scale: [u16; 64],
    pub loop_filter_limits: [u8; 64],
    pub ranges: [(Vec<i32>, Vec<u8>); 6],
}

#[repr(C)]
struct RawRange {
    count: i32,
    sizes: *const i32,
    matrices: *const u8,
}

#[repr(C)]
struct RawSetup {
    huffman: [*const i16; 80],
    dc_scale: [u16; 64],
    ac_scale: [u16; 64],
    loop_filter_limits: [u8; 64],
    ranges: [RawRange; 6],
}

unsafe extern "C" {
    fn adv_theora_create(info: *const u8, setup: *const RawSetup, refs: i32) -> *mut c_void;
    fn adv_theora_info_size() -> usize;
    fn adv_theora_info(decoder: *mut c_void, info: *mut u8) -> u8;
    fn adv_theora_destroy(decoder: *mut c_void);
    fn adv_theora_decode(
        decoder: *mut c_void,
        data: *const u8,
        bytes: usize,
        packetno: i64,
        granulepos: i64,
        eos: i32,
        striped: i32,
        reserved: u32,
        state: *mut State,
    ) -> i32;
    fn adv_theora_stripe(decoder: *mut c_void, range: *mut i32);
    fn adv_theora_planes(decoder: *mut c_void, planes: *mut RawPlane);
    fn adv_theora_snapshot(decoder: *mut c_void, frame: *mut RawFrame, ready: i32);
    fn adv_theora_restore(decoder: *mut c_void, frame: *const RawFrame);
    fn adv_theora_verify_refs(
        staged: *const c_void,
        frame: *const RawFrame,
        synchronous: *const c_void,
    ) -> i32;
}

pub const RUN_AHEAD_DEPTH: usize = 3;
const REFERENCE_SLOTS: usize = 3 + RUN_AHEAD_DEPTH;

struct Native(NonNull<c_void>);

// SAFETY: only the unique Decoder may call into the mutable context. Frames
// keep the allocation alive and read only pixels in leased reference slots.
unsafe impl Send for Native {}
// SAFETY: sharing the allocation does not grant access to the mutable context.
unsafe impl Sync for Native {}

impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: the unique decoder and all leases have been released.
        unsafe { adv_theora_destroy(self.0.as_ptr()) };
    }
}

pub struct Decoder {
    native: Arc<Native>,
    slots: Option<[Arc<()>; REFERENCE_SLOTS]>,
    planes: [Plane; 3],
    ready: bool,
    pending_rows: Option<(usize, usize)>,
}

/// A completed decode's reference indices and output, pinned against reuse.
pub struct Frame {
    raw: RawFrame,
    ready: bool,
    native: Arc<Native>,
    _slots: [Option<Arc<()>>; 3],
}

// SAFETY: the leases pin the pixels and the allocation for the frame lifetime.
unsafe impl Send for Frame {}

impl Frame {
    /// Compare logical reference roles, padded pixels and postprocessing history.
    pub fn references_match(&self, synchronous: &Decoder) -> bool {
        assert!(synchronous.pending_rows.is_none());
        // SAFETY: the staged references are leased and the synchronous context
        // is borrowed. The verifier only reads immutable descriptors and pixels.
        unsafe {
            adv_theora_verify_refs(
                self.native.0.as_ptr(),
                &self.raw,
                synchronous.native.0.as_ptr(),
            ) != 0
        }
    }

    pub fn output(&self) -> Option<Output<'_>> {
        self.ready.then_some(Output {
            raw: self.raw.planes,
            pending_rows: None,
            lifetime: PhantomData,
        })
    }
}

impl Decoder {
    pub fn new(setup: &Setup) -> Result<Self, &'static str> {
        Self::create(setup, false)
    }

    pub fn with_run_ahead(setup: &Setup) -> Result<Self, &'static str> {
        Self::create(setup, true)
    }

    fn create(setup: &Setup, run_ahead: bool) -> Result<Self, &'static str> {
        for (sizes, matrices) in &setup.ranges {
            if sizes.is_empty()
                || sizes.len() > 63
                || sizes.iter().any(|&n| n <= 0)
                || sizes.iter().map(|&n| i64::from(n)).sum::<i64>() != 63
                || matrices.len() != (sizes.len() + 1) * 64
            {
                return Err("Invalid Theora quantization ranges");
            }
        }
        for tree in &setup.huffman {
            validate_tree(tree)?;
        }
        // SAFETY: reports the compiled ABI's structure size.
        if setup.info.len() != unsafe { adv_theora_info_size() } {
            return Err("Theora info ABI size mismatch");
        }
        let raw = RawSetup {
            huffman: std::array::from_fn(|i| setup.huffman[i].as_ptr()),
            dc_scale: setup.dc_scale,
            ac_scale: setup.ac_scale,
            loop_filter_limits: setup.loop_filter_limits,
            ranges: std::array::from_fn(|i| RawRange {
                count: setup.ranges[i].0.len() as i32,
                sizes: setup.ranges[i].0.as_ptr(),
                matrices: setup.ranges[i].1.as_ptr(),
            }),
        };
        // SAFETY: validated setup arrays remain borrowed until create returns;
        // libtheora copies all tables and retains no borrowed pointers.
        let refs = if run_ahead { REFERENCE_SLOTS as i32 } else { 3 };
        let native = unsafe { adv_theora_create(setup.info.as_ptr(), &raw, refs) };
        Ok(Self {
            native: Arc::new(Native(
                NonNull::new(native).ok_or("Theora initialization failed")?,
            )),
            slots: run_ahead.then(|| std::array::from_fn(|_| Arc::new(()))),
            planes: std::array::from_fn(|_| Plane::default()),
            ready: false,
            pending_rows: None,
        })
    }

    pub fn snapshot(&self) -> Frame {
        assert!(self.pending_rows.is_none());
        let slots = self.slots.as_ref().expect("run-ahead decoder");
        let mut raw = RawFrame::default();
        // SAFETY: no mutable decode is active and the result contains descriptors only.
        unsafe {
            adv_theora_snapshot(self.native.0.as_ptr(), &mut raw, i32::from(self.ready));
        }
        let leases = raw.refs.map(|index| {
            usize::try_from(index)
                .ok()
                .map(|index| Arc::clone(&slots[index]))
        });
        Frame {
            raw,
            ready: self.ready,
            native: Arc::clone(&self.native),
            _slots: leases,
        }
    }

    pub fn restore(&mut self, frame: &Frame) {
        assert!(Arc::ptr_eq(&self.native, &frame.native));
        assert!(self.pending_rows.is_none());
        // SAFETY: the frame pins all referenced slots of this context.
        unsafe { adv_theora_restore(self.native.0.as_ptr(), &frame.raw) };
        self.ready = frame.ready;
    }

    pub fn initialized_info(&self) -> ([u8; 64], u8) {
        let mut info = [0; 64];
        // SAFETY: create verified the fixed-width info ABI; this is an owned copy.
        let bias = unsafe { adv_theora_info(self.native.0.as_ptr(), info.as_mut_ptr()) };
        (info, bias)
    }

    pub fn decode(
        &mut self,
        packet: &[u8],
        packetno: i64,
        granulepos: i64,
        eos: bool,
        striped: bool,
        state: &mut State,
    ) -> i32 {
        let reserved = self.slots.as_ref().map_or(0, |slots| {
            slots.iter().enumerate().fold(0, |mask, (index, slot)| {
                mask | (u32::from(Arc::strong_count(slot) > 1) << index)
            })
        });
        // SAFETY: the decoder is exclusively borrowed; packet bytes and state
        // remain valid until the synchronous decode completes.
        let result = unsafe {
            adv_theora_decode(
                self.native.0.as_ptr(),
                packet.as_ptr(),
                packet.len(),
                packetno,
                granulepos,
                i32::from(eos),
                i32::from(striped),
                reserved,
                state,
            )
        };
        self.ready |= result >= 0;
        self.pending_rows = (result == 2).then(|| self.stripe());
        result
    }

    pub fn stripe(&self) -> (usize, usize) {
        let mut range = [0; 2];
        // SAFETY: the two output integers belong to this decoder's continuation.
        unsafe {
            adv_theora_stripe(self.native.0.as_ptr(), range.as_mut_ptr());
        }
        (range[0] as usize, range[1] as usize)
    }

    pub fn output(&self) -> Option<Output<'_>> {
        if !self.ready {
            return None;
        }
        let mut raw = [RawPlane::default(); 3];
        // SAFETY: the decoder writes exactly three descriptors.
        unsafe {
            adv_theora_planes(self.native.0.as_ptr(), raw.as_mut_ptr());
        }
        Some(Output {
            raw,
            pending_rows: self.pending_rows,
            lifetime: PhantomData,
        })
    }

    pub fn dimensions(&self) -> Option<[(usize, usize); 3]> {
        Some(self.output()?.dimensions())
    }

    pub fn copy_plane(
        &self,
        plane: usize,
        destination: &mut [u8],
        stride: i32,
    ) -> Result<std::ops::Range<usize>, &'static str> {
        self.output()
            .ok_or("No decoded Theora frame")?
            .copy_plane(plane, destination, stride)
    }

    /// Owned tight planes for differential verification.
    pub fn planes(&mut self) -> Option<&[Plane; 3]> {
        let dimensions = self.dimensions()?;
        for (i, (width, height)) in dimensions.into_iter().enumerate() {
            let mut data = std::mem::take(&mut self.planes[i].data);
            data.resize(width * height, 0);
            self.copy_plane(i, &mut data, width as i32).ok()?;
            self.planes[i] = Plane {
                width,
                height,
                data,
            };
        }
        Some(&self.planes)
    }
}

/// Read-only planes borrowed from a decoder or a staged frame.
pub struct Output<'a> {
    raw: [RawPlane; 3],
    pending_rows: Option<(usize, usize)>,
    lifetime: PhantomData<&'a ()>,
}

impl Output<'_> {
    pub fn dimensions(&self) -> [(usize, usize); 3] {
        self.raw.map(|p| (p.width as usize, p.height as usize))
    }

    /// Copy valid decoded rows into padded output without an intermediate image.
    /// The destination starts at the lowest address, for either stride sign.
    pub fn copy_plane(
        &self,
        plane: usize,
        destination: &mut [u8],
        stride: i32,
    ) -> Result<std::ops::Range<usize>, &'static str> {
        let source = self.raw.get(plane).ok_or("Invalid Theora plane")?;
        let (width, height) = (source.width as usize, source.height as usize);
        let pitch = stride.unsigned_abs() as usize;
        let size = pitch
            .checked_mul(height - 1)
            .and_then(|n| n.checked_add(width))
            .ok_or("Plane size overflow")?;
        if pitch < width || destination.len() < size {
            return Err("Output plane is too small");
        }
        let (first, end) = self.pending_rows.map_or((0, height), |(first, end)| {
            (
                height - end * 8 * height / self.raw[0].height as usize,
                height - first * 8 * height / self.raw[0].height as usize,
            )
        });
        for row in first..end {
            // SAFETY: only initialized rows reported by the decoder are read;
            // the output borrow prevents reuse of the underlying frame.
            let input = unsafe {
                std::slice::from_raw_parts(
                    source.data.offset(row as isize * source.stride as isize),
                    width,
                )
            };
            let offset = if stride < 0 { height - 1 - row } else { row } * pitch;
            destination[offset..offset + width].copy_from_slice(input);
        }
        Ok(if stride < 0 {
            height - end..height - first
        } else {
            first..end
        })
    }
}

fn validate_tree(tree: &[i16]) -> Result<(), &'static str> {
    fn node(tree: &[i16], at: usize, depth: usize, visited: &mut [bool]) -> Option<usize> {
        if depth > 32 || *visited.get(at)? {
            return None;
        }
        visited[at] = true;
        let bits = *tree.get(at)?;
        if !(0..=15).contains(&bits) {
            return None;
        }
        let count = 1usize << bits;
        let children = tree.get(at + 1..at + 1 + count)?;
        let mut size = count + 1;
        let mut i = 0;
        while i < count {
            let child = children[i];
            if child > 0 {
                size += node(tree, child as usize, depth + 1, visited)?;
                i += 1;
            } else {
                let leaf = -i32::from(child);
                let used = leaf >> 8;
                if used > i32::from(bits) || leaf & 255 >= 92 {
                    return None;
                }
                let skip = 1usize << (i32::from(bits) - used);
                if i + skip > count || children[i..i + skip].iter().any(|&n| n != child) {
                    return None;
                }
                i += skip;
            }
        }
        Some(size)
    }
    if tree.len() > 32768 || node(tree, 0, 0, &mut vec![false; tree.len()]) != Some(tree.len()) {
        return Err("Invalid Theora Huffman tree");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn adv_theora_test_huffman(tree: *const i16, data: *const u8, size: i32) -> i32;
        fn adv_theora_test_setup_huffman(
            header: *const u8,
            bytes: usize,
            data: *const u8,
            size: i32,
        ) -> i32;
    }

    fn random_word(seed: &mut u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed
    }

    fn random_huffman_node(tree: &mut Vec<i16>, seed: &mut u32, width: u32, depth: u32) -> i16 {
        let node = tree.len();
        let count = 1usize << width;
        tree.resize(node + 1 + count, 0);
        tree[node] = width as i16;
        let mut i = 0;
        while i < count {
            let random = random_word(seed);
            let entry = node + 1 + i;
            if depth != 0 && random & 3 == 0 {
                tree[entry] = random_huffman_node(tree, seed, 1 + (random >> 2) % 4, depth - 1);
                i += 1;
            } else {
                let alignment = (i | count).trailing_zeros();
                let repeat = (random >> 2) % (alignment + 1);
                let leaf = ((width - repeat) << 8) | ((random >> 8) % 92);
                let slots = 1 << repeat;
                tree[entry..entry + slots].fill(-(leaf as i16));
                i += slots;
            }
        }
        node.try_into().unwrap()
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn theora_entropy_matches_reference_on_setup_and_random_trees() {
        // A setup packet produced by libtheora 1.2.0 for a synthetic 32x32 stream.
        let header = include_bytes!("../tests/data/theora-setup.bin");
        let mut seed = 0xa6d5_fc31;
        let data: [u8; 48] = std::array::from_fn(|_| random_word(&mut seed) as u8);
        // SAFETY: both pointers cover the supplied sizes for the synchronous test.
        let result = unsafe {
            adv_theora_test_setup_huffman(
                header.as_ptr(),
                header.len(),
                data.as_ptr(),
                data.len() as i32,
            )
        };
        assert_eq!(result, 0, "setup parse or table {result} differed");
        for case in 0..128 {
            let mut tree = Vec::new();
            let width = case % 10 + 1;
            let depth = if width > 6 { 0 } else { 8 };
            random_huffman_node(&mut tree, &mut seed, width, depth);
            validate_tree(&tree).unwrap();
            let data: [u8; 48] = std::array::from_fn(|_| random_word(&mut seed) as u8);
            // SAFETY: the validated tree and all packet prefixes are live.
            let result =
                unsafe { adv_theora_test_huffman(tree.as_ptr(), data.as_ptr(), data.len() as i32) };
            assert_eq!(result, 1, "random tree {case}");
        }
    }

    fn setup(format: u32) -> Setup {
        let mut info = [0u8; 64];
        info[..3].copy_from_slice(&[3, 2, 1]);
        for (offset, value) in [
            (4, 32u32),
            (8, 160),
            (12, 32),
            (16, 160),
            (28, 30),
            (32, 1),
            (48, format),
            (60, 6),
        ] {
            info[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Setup {
            info,
            huffman: std::array::from_fn(|_| vec![1, -271, -312]),
            dc_scale: [100; 64],
            ac_scale: [100; 64],
            loop_filter_limits: [16; 64],
            ranges: std::array::from_fn(|_| (vec![63], vec![16; 128])),
        }
    }
    // Authored Theora data packets: two one-bit Huffman symbols, EOB and +1.
    fn keyframe(format: u32, dc: bool) -> Vec<u8> {
        packet_for(format, 80, dc)
    }
    fn packet_for(format: u32, luma_fragments: usize, dc: bool) -> Vec<u8> {
        let fragments = luma_fragments
            + match format {
                0 => luma_fragments / 2,
                2 => luma_fragments,
                3 => luma_fragments * 2,
                _ => unreachable!(),
            };
        let mut bits = vec![false; 20];
        bits.extend(std::iter::repeat_n(dc, fragments));
        bits.extend([false; 8]);
        if dc {
            bits.extend(std::iter::repeat_n(false, fragments));
        }
        pack_bits(bits)
    }

    fn pack_bits(bits: Vec<bool>) -> Vec<u8> {
        let mut packet = vec![0; bits.len().div_ceil(8)];
        for (i, bit) in bits.into_iter().enumerate() {
            packet[i / 8] |= u8::from(bit) << (7 - i % 8);
        }
        packet
    }
    fn pixels(decoder: &mut Decoder) -> Vec<u8> {
        decoder
            .planes()
            .unwrap()
            .iter()
            .flat_map(|p| p.data.iter().copied())
            .collect()
    }
    // spec: DEV-VIDEO-1
    #[test]
    fn packet_state_duplicate_error_and_stripe_continuations() {
        for format in [0, 2, 3] {
            let setup = setup(format);
            let mut whole = Decoder::new(&setup).unwrap();
            let mut striped = Decoder::new(&setup).unwrap();
            let mut a = State::default();
            let mut b = State::default();
            for dc in [false, true] {
                a.pp_level = 7;
                b.pp_level = 7;
                let packet = keyframe(format, dc);
                assert_eq!(whole.decode(&packet, 0, -1, false, false, &mut a), 0);
                let mut ranges = Vec::new();
                loop {
                    let result = striped.decode(&packet, 0, -1, false, true, &mut b);
                    if result != 2 {
                        assert_eq!(result, 0);
                        break;
                    }
                    let range = striped.stripe();
                    striped.planes().unwrap();
                    assert!(range.0 < range.1 && range.1 <= 20);
                    if let Some(&last) = ranges.last() {
                        assert_eq!(range.1, last);
                    }
                    ranges.push(range.0);
                }
                assert_eq!(ranges.last(), Some(&0));
                assert_eq!(a.curframe_num, b.curframe_num);
                assert_eq!(a.granpos, b.granpos);
                let expected = pixels(&mut whole);
                assert_eq!(expected, pixels(&mut striped));
                if !dc {
                    assert!(expected.iter().all(|&v| v == 128));
                } else {
                    assert!(expected.iter().any(|&v| v != 128));
                }
            }
            let before = pixels(&mut whole);
            let frame = a.curframe_num;
            assert_eq!(whole.decode(&[], 2, -1, false, false, &mut a), 1);
            assert_eq!(a.curframe_num, frame + 1);
            assert_eq!(before, pixels(&mut whole));
            assert_eq!(whole.decode(&[0x80], 3, -1, false, false, &mut a), -24);
            assert_eq!(a.curframe_num, frame + 1);
        }
    }
    // spec: DEV-VIDEO-1
    #[test]
    fn parallel_planes_match_serial_stripes_with_postprocessing() {
        for format in [0, 2, 3] {
            let mut setup = setup(format);
            setup.info[4..8].copy_from_slice(&512u32.to_le_bytes());
            setup.info[12..16].copy_from_slice(&512u32.to_le_bytes());
            let mut parallel = Decoder::new(&setup).unwrap();
            let mut serial = Decoder::new(&setup).unwrap();
            let mut a = State {
                pp_level: 7,
                ..State::default()
            };
            let mut b = a;
            for dc in [false, true] {
                let packet = packet_for(format, 1280, dc);
                assert_eq!(parallel.decode(&packet, 0, -1, false, false, &mut a), 0);
                while serial.decode(&packet, 0, -1, false, true, &mut b) == 2 {}
                assert_eq!(pixels(&mut parallel), pixels(&mut serial));
                assert_eq!(a.curframe_num, b.curframe_num);
            }
        }
    }

    // Authored I/P packets with a partial final band, uncoded luma,
    // half-pel prediction and EOB runs that cross band boundaries.
    fn band_test_stream(format: u32) -> (Setup, [Vec<u8>; 3]) {
        let mut setup = setup(format);
        let (width, height) = (528usize, 176usize);
        for (offset, value) in [(4, width), (8, height), (12, 525), (16, 173)] {
            setup.info[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        setup.huffman = std::array::from_fn(|_| vec![2, -527, -568, -569, -512]);
        let cw = width >> usize::from(format & 1 == 0);
        let ch = height >> usize::from(format & 2 == 0);
        let luma_sb_cols = width.div_ceil(32);
        let sbs = luma_sb_cols * height.div_ceil(32) + 2 * cw.div_ceil(32) * ch.div_ceil(32);
        let fragments = (width * height + 2 * cw * ch) / 64;
        let packet = |inter: bool, golden: bool| {
            let mut bits = Vec::new();
            let mut write = |value: usize, n: usize| {
                bits.extend((0..n).rev().map(|i| value & (1 << i) != 0));
            };
            let coded = if inter {
                write(0b010000000, 9); // Inter, one quantizer.
                write(0, 1); // All superblocks are fully coded or fully uncoded.
                write(0b111111, 6);
                write(sbs - 34, 12);
                write(0, 1); // First luma superblock row is uncoded.
                write(0b11110, 5);
                write(luma_sb_cols - 10, 3);
                write(0b111111, 6);
                write(sbs - luma_sb_cols - 34, 12);
                let coded_mbs = width / 16 * ((height - 32) / 16);
                write(7, 3); // Fixed macroblock mode codes.
                for _ in 0..coded_mbs {
                    write(if golden { 6 } else { 2 }, 3);
                }
                write(1, 1); // Fixed motion vector codes.
                for _ in 0..coded_mbs {
                    write(2, 6);
                    write(3, 6);
                }
                fragments - width * 32 / 64
            } else {
                write(0, 12);
                fragments
            };
            write(0, 8); // DC Huffman table selectors.
            let eob_count = coded / 3;
            for i in 0..coded - eob_count {
                if i == coded / 4 {
                    write(3, 2);
                    write(eob_count, 12);
                }
                write(1 + ((i / 12) & 1), 2);
            }
            write(0, 8); // AC Huffman table selectors.
            for i in 0..coded - eob_count {
                write(1 + (i & 1), 2);
            }
            write(3, 2);
            write(0, 12); // End of frame, spanning the remaining coefficients.
            pack_bits(bits)
        };
        (
            setup,
            [
                packet(false, false),
                packet(true, false),
                packet(true, true),
            ],
        )
    }

    fn compare_band_stream(setup: &Setup, packets: &[Vec<u8>], pp_level: i32) {
        let mut parallel = Decoder::new(setup).unwrap();
        let mut serial = Decoder::new(setup).unwrap();
        let mut a = State {
            pp_level,
            ..State::default()
        };
        let mut b = a;
        let mut previous = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            assert_eq!(
                parallel.decode(packet, i as i64, -1, false, false, &mut a),
                0
            );
            loop {
                let result = serial.decode(packet, i as i64, -1, false, true, &mut b);
                if result != 2 {
                    assert_eq!(result, 0);
                    break;
                }
            }
            let frame = pixels(&mut parallel);
            assert_eq!(a.frame_type, u8::from(i != 0));
            assert_eq!((a.curframe_num, a.granpos), (b.curframe_num, b.granpos));
            assert!(
                frame == pixels(&mut serial),
                "frame {i}, pp_level {pp_level}"
            );
            assert!(frame != previous, "frame {i} must change the image");
            previous = frame;
        }
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn parallel_bands_match_serial_for_keyframes_and_inter_frames() {
        for format in [0, 2, 3] {
            let (mut setup, packets) = band_test_stream(format);
            for limit in [0, 16] {
                setup.loop_filter_limits.fill(limit);
                for pp_level in 0..=7 {
                    compare_band_stream(&setup, &packets, pp_level);
                }
            }
        }
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn theora_decoders_share_workers_across_threads() {
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for format in [0, 3] {
                let start = &start;
                scope.spawn(move || {
                    let (setup, packets) = band_test_stream(format);
                    start.wait();
                    compare_band_stream(&setup, &packets, 7);
                });
            }
        });
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn padded_plane_copy_preserves_orientation_and_row_boundaries() {
        let mut decoder = Decoder::new(&setup(0)).unwrap();
        assert_eq!(
            decoder.decode(
                &keyframe(0, true),
                0,
                -1,
                false,
                false,
                &mut State::default()
            ),
            0
        );
        let expected = decoder.planes().unwrap()[0].data.clone();
        for stride in [64i32, -64] {
            let mut output = vec![0xa5; 64 * 159 + 32 + 16];
            assert_eq!(decoder.copy_plane(0, &mut output, stride).unwrap(), 0..160);
            for y in 0..160 {
                let offset = if stride < 0 { 159 - y } else { y } * 64;
                assert_eq!(&output[offset..offset + 32], &expected[y * 32..y * 32 + 32]);
            }
            for y in 0..159 {
                assert!(output[y * 64 + 32..(y + 1) * 64].iter().all(|&b| b == 0xa5));
            }
            assert!(output[64 * 159 + 32..].iter().all(|&b| b == 0xa5));
        }
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn malformed_setup_never_reaches_native_pointer_walks() {
        let mut value = setup(0);
        value.huffman[0] = vec![1, 1, 1];
        assert!(Decoder::new(&value).is_err());
        value = setup(0);
        value.ranges[0].1.clear();
        assert!(Decoder::new(&value).is_err());
    }
}

#[cfg(test)]
mod neon_tests {
    unsafe extern "C" {
        fn adv_theora_idct(y: *mut i16, x: *mut i16, last: i32);
        fn oc_idct8x8_c(y: *mut i16, x: *mut i16, last: i32);
        fn adv_theora_intra(dst: *mut u8, stride: i32, residue: *const i16);
        fn oc_frag_recon_intra_c(dst: *mut u8, stride: i32, residue: *const i16);
        fn adv_theora_inter(dst: *mut u8, src: *const u8, stride: i32, residue: *const i16);
        fn oc_frag_recon_inter_c(dst: *mut u8, src: *const u8, stride: i32, residue: *const i16);
        fn adv_theora_inter2(
            dst: *mut u8,
            a: *const u8,
            b: *const u8,
            stride: i32,
            residue: *const i16,
        );
        fn oc_frag_recon_inter2_c(
            dst: *mut u8,
            a: *const u8,
            b: *const u8,
            stride: i32,
            residue: *const i16,
        );
        fn adv_theora_filter_h(pixels: *mut u8, stride: i32, limit: i32);
        fn adv_theora_filter_v(pixels: *mut u8, stride: i32, limit: i32);
        static OC_FZIG_ZAG: [u8; 128];
    }
    // spec: DEV-VIDEO-1
    #[test]
    fn neon_filters_match_scalar_edges_and_negative_strides() {
        let mut seed = 13u32;
        for iteration in 0..256 {
            let mut input = [0u8; 16 * 16];
            for b in &mut input {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *b = seed as u8;
            }
            for limit in [0, 1, 7, 31, 63, 127] {
                for stride in [16i32, -16] {
                    for horizontal in [false, true] {
                        let origin = if stride < 0 { 12 * 16 + 4 } else { 3 * 16 + 4 };
                        let mut expected = input;
                        let mut actual = input;
                        for n in 0..8 {
                            let at = origin + if horizontal { n * stride } else { n };
                            let step = if horizontal { 1 } else { stride };
                            let a = expected[(at - step * 2) as usize] as i32;
                            let b = expected[(at - step) as usize] as i32;
                            let c = expected[at as usize] as i32;
                            let d = expected[(at + step) as usize] as i32;
                            let value = (a - d + 3 * (c - b) + 4) >> 3;
                            let delta =
                                value.signum() * value.abs().min(2 * limit - value.abs()).max(0);
                            expected[(at - step) as usize] = (b + delta).clamp(0, 255) as u8;
                            expected[at as usize] = (c - delta).clamp(0, 255) as u8;
                        }
                        // SAFETY: both edge orientations have the four required rows/columns.
                        unsafe {
                            let ptr = actual.as_mut_ptr().add(origin as usize);
                            if horizontal {
                                adv_theora_filter_h(ptr, stride, limit);
                            } else {
                                adv_theora_filter_v(ptr, stride, limit);
                            }
                        }
                        assert_eq!(actual, expected, "filter iteration={iteration}");
                    }
                }
            }
        }
    }

    // spec: DEV-VIDEO-1
    #[test]
    fn neon_reconstruction_matches_scalar_for_sparse_and_wrapping_coefficients() {
        let mut seed = 0x12345678u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for iteration in 0..2048 {
            let last = [2, 3, 4, 10, 11, 64][iteration % 6];
            let mut source = [0i16; 64];
            for z in 0..last {
                source[unsafe { OC_FZIG_ZAG[z as usize] } as usize] = next() as i16;
            }
            let mut a = source;
            let mut b = source;
            let mut expected = [0i16; 64];
            let mut actual = [0i16; 64];
            // SAFETY: independent, complete blocks and the standard zig-zag prefix.
            unsafe {
                oc_idct8x8_c(expected.as_mut_ptr(), a.as_mut_ptr(), last);
                adv_theora_idct(actual.as_mut_ptr(), b.as_mut_ptr(), last);
            }
            assert_eq!(actual, expected, "IDCT last={last} iteration={iteration}");
            assert_eq!(a, b);
            let src1 = std::array::from_fn::<_, 256, _>(|_| next() as u8);
            let src2 = std::array::from_fn::<_, 256, _>(|_| next() as u8);
            let residue = std::array::from_fn::<_, 64, _>(|_| next() as i16);
            for stride in [24, -24] {
                let offset = if stride < 0 { 7 * 24 } else { 0 };
                for mode in 0..3 {
                    let mut expected = [0xacu8; 256];
                    let mut actual = expected;
                    // SAFETY: eight padded rows, including negative strides, and 64 residues.
                    unsafe {
                        let dst1 = expected.as_mut_ptr().add(offset);
                        let dst2 = actual.as_mut_ptr().add(offset);
                        let p = src1.as_ptr().add(offset);
                        let q = src2.as_ptr().add(offset);
                        let r = residue.as_ptr();
                        match mode {
                            0 => {
                                oc_frag_recon_intra_c(dst1, stride, r);
                                adv_theora_intra(dst2, stride, r);
                            }
                            1 => {
                                oc_frag_recon_inter_c(dst1, p, stride, r);
                                adv_theora_inter(dst2, p, stride, r);
                            }
                            _ => {
                                oc_frag_recon_inter2_c(dst1, p, q, stride, r);
                                adv_theora_inter2(dst2, p, q, stride, r);
                            }
                        }
                    }
                    assert_eq!(actual, expected, "reconstruction mode={mode}");
                }
            }
        }
    }
}
