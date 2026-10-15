//! CPU streams for FVF-equivalent fixed vertex inputs. This reader has no
//! programmable attributes, GPU layouts or resource ownership of its own.
use super::*;

pub(super) enum Input {
    Up { data: u32, stride: u32 },
    Bound { start: u32 },
}
#[derive(Clone, Copy)]
struct Source {
    data: u32,
    stride: u32,
}
pub(super) struct Reader {
    sources: [Option<Source>; 16],
    raw: [Vec<u8>; 16],
    start: u32,
}
impl Reader {
    pub(super) fn key(&self) -> u128 {
        super::super::recording::hash(&self.raw)
    }
    pub(super) fn new(
        env: &Env<'_>,
        device: u32,
        layout: Layout,
        input: Input,
        first: u32,
        last: u32,
    ) -> Result<Self, Rejected> {
        let mut sizes = [0u32; 16];
        for (stream, offset, bytes) in std::iter::once((
            layout.streams[0],
            layout.position,
            if layout.transformed { 16 } else { 12 },
        ))
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
            sources[number] = Some(Source { data, stride });
        }
        Ok(Self {
            sources,
            raw: std::array::from_fn(|stream| vec![0; sizes[stream] as usize]),
            start,
        })
    }
    pub(super) fn read(&mut self, env: &Env<'_>, index: u32) -> Result<(), Rejected> {
        let index = index.checked_add(self.start).ok_or(INVALID)?;
        for (source, raw) in self.sources.iter().zip(&mut self.raw) {
            if let Some(source) = source {
                let address = index
                    .checked_mul(source.stride)
                    .and_then(|n| source.data.checked_add(n))
                    .ok_or(INVALID)?;
                env.cpu
                    .read(address, raw)
                    .map_err(|_| rejected(INVALID, "declared stream vertex read failed"))?;
            }
        }
        Ok(())
    }
    pub(super) fn word(&self, stream: u16, offset: u32) -> [u8; 4] {
        self.raw[stream as usize][offset as usize..offset as usize + 4]
            .try_into()
            .unwrap()
    }
}
fn check_span(env: &Env<'_>, address: u32, bytes: u32) -> Result<(), Rejected> {
    adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ)
        .map_err(|_| rejected(INVALID, "declared stream span is not readable"))
}
