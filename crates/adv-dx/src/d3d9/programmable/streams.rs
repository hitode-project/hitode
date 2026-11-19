//! Immutable stream capture shared by translated and generated shaders.
//! A fixed UP record may need padding between vertices for GPU vertex fetch.
use super::*;
pub(super) fn capture(
    env: &Env<'_>,
    pointer: u32,
    inputs: &[Input],
    up: Option<(GuestPtr<u8>, u32)>,
    start: u32,
    vertices: std::ops::RangeInclusive<u32>,
    instanced: bool,
    fixed: bool,
) -> Result<Vec<VertexStream>, u32> {
    let (first, last) = vertices.into_inner();
    let d = device(env, pointer).ok_or(INVALID)?;
    let hot = d.hot.borrow();
    let mut streams = Vec::new();
    let mut total = 0u32;
    for n in 0..16 {
        let selected = inputs.iter().filter(|i| i.stream == n);
        if selected.clone().next().is_none() {
            continue;
        }
        let minimum = selected
            .clone()
            .map(|i| i.offset + i.format.size())
            .max()
            .unwrap();
        let (data, stride, available) = if let Some((data, stride)) = up {
            if n != 0 || data.is_null() || stride == 0 {
                return Err(INVALID);
            }
            (data.addr(), stride, u32::MAX)
        } else {
            let stream = hot.buffers.streams[n];
            let Some(Payload::Buffer(buffer)) = stream
                .buffer
                .and_then(|id| com(env).get_identity(&env.sys.objects, id))
            else {
                return Err(INVALID);
            };
            if buffer.locks != 0 || stream.offset > buffer.len {
                return Err(INVALID);
            }
            (
                buffer.storage.checked_add(stream.offset).ok_or(INVALID)?,
                stream.stride,
                buffer.len - stream.offset,
            )
        };
        let unaligned_up = fixed && up.is_some() && !stride.is_multiple_of(4);
        if stride > 2048
            || (!unaligned_up && !stride.is_multiple_of(4))
            || (stride != 0 && stride < minimum)
        {
            return Err(INVALID);
        }
        let instance = instanced && hot.buffers.frequencies[n] & buffers::INSTANCE_DATA != 0;
        let (start, first, last) = if instance {
            (0, 0, buffers::instances(&hot.buffers) - 1)
        } else {
            (start, first, last)
        };
        let offset = start
            .checked_add(first)
            .and_then(|v| v.checked_mul(stride))
            .ok_or(INVALID)?;
        let bytes = (last - first)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(minimum))
            .ok_or(INVALID)?;
        if offset.checked_add(bytes).is_none_or(|end| end > available) {
            return Err(INVALID);
        }
        let upload_stride = stride.next_multiple_of(4);
        let upload_bytes = if unaligned_up {
            (last - first + 1)
                .checked_mul(upload_stride)
                .ok_or(INVALID)?
        } else {
            bytes
        };
        total = total
            .checked_add(upload_bytes)
            .filter(|&v| v <= 16 * 1024 * 1024)
            .ok_or(INVALID)?;
        let at = data.checked_add(offset).ok_or(INVALID)?;
        adv_abi::guest::check_range(env.cpu, at, bytes, Prot::READ).map_err(|_| INVALID)?;
        let repack = unaligned_up.then_some((stride as usize, upload_stride as usize));
        let mut raw = hot.fixed.capture_stream(env, at, bytes, repack)?;
        if instanced && stride != 0 && !bytes.is_multiple_of(stride) {
            // Metal bounds vertex fetches by complete strides, including the
            // final record whose unused trailing bytes need no guest storage.
            raw.resize(bytes.next_multiple_of(stride) as usize, 0);
        }
        streams.push(VertexStream {
            instance,
            bytes: raw,
            stride: upload_stride,
            attributes: selected
                .map(|i| Attribute {
                    location: i.register,
                    offset: i.offset,
                    format: i.format,
                })
                .collect(),
        });
    }
    Ok(streams)
}
