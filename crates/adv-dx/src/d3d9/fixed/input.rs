//! CPU streams for FVF-equivalent fixed vertex inputs. This reader has no
//! programmable attributes, GPU layouts or resource ownership of its own.
use super::*;
use std::hash::{Hash, Hasher};

#[derive(Clone)]
pub(super) struct Geometry {
    pub vertices: Arc<Vec<Vertex>>,
    pub texcoords: Option<(usize, Arc<Vec<[f32; 4]>>)>,
}
#[derive(Default)]
pub(super) struct GeometryCache {
    current: std::collections::HashMap<u128, Geometry>,
    previous: std::collections::HashMap<u128, Geometry>,
    streams: [std::collections::HashMap<u128, adv_device::gpu::VertexBytes>; 2],
    bytes: usize,
}
impl GeometryCache {
    pub fn get(&mut self, key: u128) -> Option<Geometry> {
        let geometry = self
            .current
            .get(&key)
            .or_else(|| self.previous.get(&key))?
            .clone();
        self.insert(key, geometry.clone());
        Some(geometry)
    }
    pub fn insert(&mut self, key: u128, geometry: Geometry) {
        if self.current.contains_key(&key) {
            return;
        }
        let bytes = std::mem::size_of_val(geometry.vertices.as_slice())
            + geometry
                .texcoords
                .as_ref()
                .map_or(0, |(_, values)| std::mem::size_of_val(values.as_slice()));
        // Match the recording's command and payload budgets, for each of two
        // frame generations, including draws that never reach Present.
        if self.current.len() + self.streams[0].len() < 4096
            && self.bytes + bytes <= 64 * 1024 * 1024
        {
            self.bytes += bytes;
            self.current.insert(key, geometry);
        }
    }
    pub fn stream(&mut self, key: u128) -> Option<adv_device::gpu::VertexBytes> {
        let bytes = self.streams[0]
            .get(&key)
            .or_else(|| self.streams[1].get(&key))?
            .clone();
        self.insert_stream(key, bytes.clone());
        Some(bytes)
    }
    pub fn insert_stream(&mut self, key: u128, stream: adv_device::gpu::VertexBytes) {
        if self.streams[0].contains_key(&key) {
            return;
        }
        if self.current.len() + self.streams[0].len() < 4096
            && self.bytes + stream.len() <= 64 * 1024 * 1024
        {
            self.bytes += stream.len();
            self.streams[0].insert(key, stream);
        }
    }
    pub fn finish(&mut self) {
        std::mem::swap(&mut self.current, &mut self.previous);
        self.current.clear();
        self.streams.swap(0, 1);
        self.streams[0].clear();
        self.bytes = 0;
    }
}

pub(super) enum Input {
    Up { data: u32, stride: u32 },
    Bound { start: u32 },
}
#[derive(Clone, Copy)]
struct Source {
    data: u32,
    stride: u32,
}
pub(super) struct Reader<'a> {
    sources: [Option<Source>; 16],
    raw: [std::borrow::Cow<'a, [u8]>; 16],
    batches: [Option<u32>; 16],
    sizes: [u32; 16],
    current: u32,
    start: u32,
}
impl<'a> Reader<'a> {
    pub(super) fn captured_key(&mut self, indices: Option<&[u32]>, length: u32) -> Option<u128> {
        if self
            .sources
            .iter()
            .zip(self.batches)
            .any(|(source, batch)| source.is_some() && batch.is_none())
        {
            return None;
        }
        let mut key = xxhash_rust::xxh3::Xxh3::new();
        indices.hash(&mut key);
        for ordinal in 0..length {
            self.current = indices
                .map_or(ordinal, |v| v[ordinal as usize])
                .checked_add(self.start)?;
            self.hash(&mut key);
        }
        Some(key.digest128())
    }
    pub(super) fn hash(&self, key: &mut impl Hasher) {
        let bytes: [&[u8]; 16] = std::array::from_fn(|n| self.bytes(n));
        bytes.hash(key);
    }
    fn bytes(&self, stream: usize) -> &[u8] {
        let offset = self.batches[stream].map_or(0, |first| {
            ((self.current - first) * self.sources[stream].unwrap().stride) as usize
        });
        &self.raw[stream][offset..offset + self.sizes[stream] as usize]
    }
    pub(super) fn new(
        env: &'a Env<'_>,
        device: u32,
        layout: Layout,
        input: Input,
        first: u32,
        last: u32,
        vertices: u32,
    ) -> Result<Self, Rejected> {
        let mut sizes = [0u32; 16];
        for (stream, offset, bytes) in std::iter::once((
            layout.streams[0],
            layout.position,
            if layout.transformed { 16 } else { 12 },
        ))
        .chain(layout.normal.map(|(stream, at)| (stream, at, 12)))
        .chain(layout.specular.map(|(stream, at)| (stream, at, 4)))
        .chain(layout.color.map(|at| (layout.streams[1], at, 4)))
        .chain(
            layout
                .texcoords
                .into_iter()
                .flatten()
                .map(|(stream, offset, dimensions)| (stream, offset, u32::from(dimensions) * 4)),
        ) {
            let size = sizes.get_mut(stream as usize).ok_or(INVALID)?;
            *size = (*size).max(offset.checked_add(bytes).ok_or(INVALID)?);
        }
        if matches!(input, Input::Up { .. }) && sizes[1..].iter().any(|&size| size != 0) {
            return Err(rejected(
                UNAVAILABLE,
                "UP fixed-function declarations require all inputs on stream 0",
            ));
        }
        let start = match input {
            Input::Bound { start } => start,
            Input::Up { .. } => 0,
        };
        let first = first.checked_add(start).ok_or(INVALID)?;
        let last = last.checked_add(start).ok_or(INVALID)?;
        let mut sources = [None; 16];
        let mut batches = [None; 16];
        let mut spans = [(0, 0); 16];
        // Validate every used stream before reading or allocating vertex data.
        // An unused bound stream has no bearing on this declaration's draw.
        for (number, &minimum) in sizes.iter().enumerate().filter(|(_, size)| **size != 0) {
            let (data, stride, limit) = match input {
                Input::Up { data, stride } => (data, stride, None),
                Input::Bound { .. } => {
                    let (data, stride, bytes) =
                        super::super::buffers::stream_data(env, device, number as u16)?;
                    if stride == 0 {
                        return Err(rejected(
                            UNAVAILABLE,
                            "zero-stride/instanced streams are outside the fixed CPU input profile",
                        ));
                    }
                    (data, stride, Some(bytes))
                }
            };
            if data == 0 || stride < minimum || stride > 2048 {
                return Err(rejected(
                    INVALID,
                    "declared stream pointer or stride is invalid",
                ));
            }
            let end = last
                .checked_mul(stride)
                .and_then(|n| n.checked_add(minimum))
                .ok_or(INVALID)?;
            if limit.is_some_and(|limit| end > limit) {
                return Err(rejected(
                    INVALID,
                    "declared stream extends past its vertex buffer",
                ));
            }
            let offset = first.checked_mul(stride).ok_or(INVALID)?;
            let span = data.checked_add(offset).ok_or(INVALID)?;
            check_span(env, span, end - offset)?;
            // Ordinary guest storage can be borrowed for synchronous decoding.
            // Copied spans remain bounded by the separate vertex read volume.
            if env
                .cpu
                .shared_bytes(span, (end - offset) as usize)
                .is_some()
                || (matches!(input, Input::Bound { .. })
                    && u64::from(end - offset) <= u64::from(vertices) * u64::from(minimum))
            {
                batches[number] = Some(first);
                spans[number] = (span, end - offset);
            }
            sources[number] = Some(Source { data, stride });
        }
        let mut raw = std::array::from_fn(|stream| {
            let (address, span) = spans[stream];
            if batches[stream].is_some()
                && let Some(bytes) = env.cpu.shared_bytes(address, span as usize)
            {
                return std::borrow::Cow::Borrowed(bytes);
            }
            std::borrow::Cow::Owned(vec![
                0;
                batches[stream].map_or(sizes[stream], |_| span) as usize
            ])
        });
        for (stream, first) in batches.iter().enumerate() {
            if first.is_some() && matches!(raw[stream], std::borrow::Cow::Owned(_)) {
                env.cpu
                    .read(spans[stream].0, raw[stream].to_mut())
                    .map_err(|_| rejected(INVALID, "declared stream vertex read failed"))?;
            }
        }
        Ok(Self {
            sources,
            raw,
            batches,
            sizes,
            current: first,
            start,
        })
    }
    pub(super) fn read(&mut self, env: &Env<'_>, index: u32) -> Result<(), Rejected> {
        let index = index.checked_add(self.start).ok_or(INVALID)?;
        self.current = index;
        for (number, (source, raw)) in self.sources.iter().zip(&mut self.raw).enumerate() {
            if self.batches[number].is_some() {
                continue;
            }
            if let Some(source) = source {
                let address = index
                    .checked_mul(source.stride)
                    .and_then(|n| source.data.checked_add(n))
                    .ok_or(INVALID)?;
                env.cpu
                    .read(address, raw.to_mut())
                    .map_err(|_| rejected(INVALID, "declared stream vertex read failed"))?;
            }
        }
        Ok(())
    }
    pub(super) fn word(&self, stream: u16, offset: u32) -> [u8; 4] {
        self.bytes(stream as usize)[offset as usize..offset as usize + 4]
            .try_into()
            .unwrap()
    }
}
fn check_span(env: &Env<'_>, address: u32, bytes: u32) -> Result<(), Rejected> {
    adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ)
        .map_err(|_| rejected(INVALID, "declared stream span is not readable"))
}
