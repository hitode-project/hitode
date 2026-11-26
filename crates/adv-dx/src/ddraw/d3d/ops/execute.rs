//! Execute buffers share the legacy device registers and recorded draw path.
use super::super::{
    desc::{execute, vertex},
    device, device_mut,
    object::{ExecuteBuffer, PrimitiveBatch},
};
use super::{
    Calls, Face, INVALID, OUT_OF_MEMORY, Object, decode,
    process::{Processor, TL_SIZE, tl_layout},
    read, write,
};
use crate::{
    argument::{Arguments, CallError},
    fixed::{Setting, tnl},
};
use adv_abi::{Env, Outcome, types::*};

const EXECUTE_LOCKED: u32 = 0x887602ca;
const EXECUTE_FAILED: u32 = 0x887602cc;
const EXECUTE_NOT_LOCKED: u32 = 0x887602cb;

impl Calls<'_> {
    pub(super) fn create_execute_buffer(&mut self, args: &[u32]) -> Result<u32, CallError> {
        let [this, desc, output, outer] = <[u32; 4]>::try_from(args).unwrap();
        self.memory.arg_put_u32(output, 0)?;
        if outer != 0 {
            return Ok(0x80040110);
        }
        if device(self.draw, self.objects, this).is_none() {
            return Ok(INVALID);
        }
        let desc: D3DEXECUTEBUFFERDESC = read(self.memory, desc)?;
        if desc.dwSize != size_of::<D3DEXECUTEBUFFERDESC>() as u32
            || desc.dwFlags & 1 == 0
            || desc.dwBufferSize == 0
            || (desc.dwFlags & 2 != 0 && desc.dwCaps & !1 != 0)
        {
            return Ok(INVALID);
        }
        let size = desc.dwBufferSize;
        let owned = desc.dwFlags & 4 == 0;
        let data = if owned {
            self.store.alloc(size, self.memory)?
        } else {
            desc.lpData
        };
        let buffer = ExecuteBuffer {
            device: this,
            data,
            size,
            owned,
            locked: false,
            execute: D3DEXECUTEDATA {
                dwSize: size_of::<D3DEXECUTEDATA>() as u32,
                ..Default::default()
            },
        };
        let result = self.make(&[Face::ExecuteBuffer], &[], Object::ExecuteBuffer(buffer));
        if owned && !matches!(result, Ok(pointer) if pointer != OUT_OF_MEMORY) {
            self.store.release(data, size);
        }
        let pointer = result?;
        if pointer == OUT_OF_MEMORY {
            return Ok(pointer);
        }
        super::super::add_ref(self.draw, self.objects, this);
        self.memory.arg_put_u32(output, pointer)?;
        Ok(0)
    }

    pub(super) fn execute_buffer(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let com = self.draw.com.unwrap();
        let Some(Object::ExecuteBuffer(buffer)) = com.get_mut::<Object>(self.objects, args[0])
        else {
            return Ok(INVALID);
        };
        match method {
            "Lock" => {
                if buffer.locked {
                    return Ok(EXECUTE_LOCKED);
                }
                let size = size_of::<D3DEXECUTEBUFFERDESC>() as u32;
                if self.memory.arg_u32(args[1])? != size {
                    return Ok(INVALID);
                }
                write(
                    self.memory,
                    args[1],
                    &D3DEXECUTEBUFFERDESC {
                        dwSize: size,
                        dwFlags: 7,
                        dwCaps: 0,
                        dwBufferSize: buffer.size,
                        lpData: buffer.data,
                    },
                )?;
                buffer.locked = true;
            }
            "Unlock" => {
                if !buffer.locked {
                    return Ok(EXECUTE_NOT_LOCKED);
                }
                buffer.locked = false;
            }
            "GetExecuteData" | "SetExecuteData" => {
                if self.memory.arg_u32(args[1])? != size_of::<D3DEXECUTEDATA>() as u32 {
                    return Ok(INVALID);
                }
                if method == "GetExecuteData" {
                    write(self.memory, args[1], &buffer.execute)?;
                } else {
                    buffer.execute = read(self.memory, args[1])?;
                }
            }
            "Optimize" => {
                if buffer.locked {
                    return Ok(EXECUTE_LOCKED);
                }
                let bytes = self.memory.arg_read(buffer.data, buffer.size as usize)?;
                if execute::decode(&bytes, buffer.execute).is_err() {
                    return Ok(EXECUTE_FAILED);
                }
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    pub(super) fn execute(&mut self, args: &[u32]) -> Result<u32, CallError> {
        self.run_execute(args, None)
    }

    pub(super) fn run_execute(
        &mut self,
        args: &[u32],
        pick: Option<D3DRECT>,
    ) -> Result<u32, CallError> {
        let [this, pointer, viewport, flags] = <[u32; 4]>::try_from(args).unwrap();
        let com = self.draw.com.unwrap();
        let Some(Object::ExecuteBuffer(buffer)) = com.get::<Object>(self.objects, pointer) else {
            return Ok(INVALID);
        };
        if buffer.locked {
            return Ok(EXECUTE_LOCKED);
        }
        if !matches!(flags, 1 | 2)
            || device(self.draw, self.objects, this).is_none()
            || !std::ptr::eq(
                device(self.draw, self.objects, this).unwrap(),
                device(self.draw, self.objects, buffer.device).unwrap(),
            )
        {
            return Ok(INVALID);
        }
        let data = buffer.execute;
        if (data.dwVertexOffset as usize + data.dwVertexCount as usize * TL_SIZE)
            > buffer.size as usize
        {
            return Ok(EXECUTE_FAILED);
        }
        let bytes = self
            .memory
            .arg_read(buffer.data, buffer.size as usize)?
            .into_owned();
        let Ok(instructions) = execute::decode(&bytes, data) else {
            return Ok(EXECUTE_FAILED);
        };
        let hr = self.select_viewport(this, viewport)?;
        if hr != 0 {
            return Ok(hr);
        }
        let mut vertices = vec![0u8; data.dwVertexCount as usize * TL_SIZE];
        let mut status = data.dsStatus;
        let mut extent = None::<D3DRECT>;
        let mut pc = 0;
        let mut picked = Vec::new();
        while let Some(instruction) = instructions.get(pc) {
            pc += 1;
            if instruction.opcode == 11 {
                break;
            }
            let mut indices = Vec::new();
            for (record_index, record) in instruction
                .records
                .chunks_exact(instruction.size)
                .enumerate()
            {
                let word = |i: usize| u32::from_le_bytes(record[i * 4..][..4].try_into().unwrap());
                let short = |i: usize| {
                    u32::from(u16::from_le_bytes(record[i * 2..][..2].try_into().unwrap()))
                };
                let hr = match instruction.opcode {
                    1 | 13 => {
                        indices.extend(short(1)..short(1) + short(0));
                        0
                    }
                    2 | 3 => {
                        indices.extend((0..if instruction.opcode == 2 { 2 } else { 3 }).map(short));
                        0
                    }
                    4..=6 => {
                        let dev = device_mut(self.draw, self.objects, this).unwrap();
                        let matrices = &mut dev.legacy.matrices;
                        let Some(&matrix) = matrices.get(&word(1)) else {
                            return Ok(EXECUTE_FAILED);
                        };
                        if instruction.opcode == 6 {
                            let Ok(slot) = decode::transform(word(0)) else {
                                return Ok(EXECUTE_FAILED);
                            };
                            dev.state
                                .as_mut()
                                .unwrap()
                                .set(Setting::Transform(slot, matrix));
                        } else {
                            let matrix = if instruction.opcode == 5 {
                                let Some(right) = matrices.get(&word(2)) else {
                                    return Ok(EXECUTE_FAILED);
                                };
                                std::array::from_fn(|i| {
                                    tnl::row(matrix[i / 4 * 4..][..4].try_into().unwrap(), right)
                                        [i % 4]
                                })
                            } else {
                                matrix
                            };
                            let Some(dest) = matrices.get_mut(&word(0)) else {
                                return Ok(EXECUTE_FAILED);
                            };
                            *dest = matrix;
                        }
                        0
                    }
                    7 => self.light_state(this, "SetLightState", word(0), word(1))?,
                    8 => self.device_state(
                        Face::Device1,
                        "SetRenderState",
                        &[this, word(0), word(1)],
                    )?,
                    9 => {
                        let op: D3DPROCESSVERTICES = bytemuck::pod_read_unaligned(record);
                        let operation = op.dwFlags & 7;
                        let layout = vertex::layout(match operation {
                            0 => 0x112,
                            1 => 0x1e2,
                            _ => 0x1c4,
                        })
                        .unwrap();
                        let dev = device_mut(self.draw, self.objects, this).unwrap();
                        let Ok(processor) =
                            Processor::new(&dev.state.as_ref().unwrap().fixed, operation == 0)
                        else {
                            return Ok(EXECUTE_FAILED);
                        };
                        let source =
                            data.dwVertexOffset as usize + usize::from(op.wStart) * TL_SIZE;
                        let dest = usize::from(op.wDest) * TL_SIZE;
                        let mut union = 0;
                        let mut intersection = 0x3f;
                        for i in 0..op.dwCount as usize {
                            let src = &bytes[source + i * TL_SIZE..][..TL_SIZE];
                            let dst = &mut vertices[dest + i * TL_SIZE..][..TL_SIZE];
                            let (_, clip) = processor.vertex(
                                src,
                                layout,
                                dst,
                                tl_layout(),
                                op.dwFlags & 16 == 0,
                            );
                            union |= clip;
                            intersection &= clip;
                        }
                        if operation != 2 && flags == 1 && op.dwCount != 0 {
                            status.dwStatus =
                                (status.dwStatus & !0x3f03f) | union | intersection << 12;
                        }
                        if op.dwFlags & 8 != 0 {
                            update_extent(
                                &mut extent,
                                vertices[dest..][..op.dwCount as usize * TL_SIZE]
                                    .as_chunks::<TL_SIZE>()
                                    .0
                                    .iter()
                                    .map(<[u8; TL_SIZE]>::as_slice),
                            );
                        }
                        dev.legacy.stats[4] += op.dwCount;
                        0
                    }
                    10 => {
                        let dev = device(self.draw, self.objects, this).unwrap();
                        let (Some(dst), Some(src)) = (
                            dev.texture_for_handle(word(0)),
                            dev.texture_for_handle(word(1)),
                        ) else {
                            return Ok(EXECUTE_FAILED);
                        };
                        self.load_texture(dst, src, 0, 0, 0)?
                    }
                    12 => {
                        if ((status.dwStatus & word(0)) == word(1)) != (word(2) != 0) {
                            if word(3) == 0 {
                                pc = instructions.len();
                            } else {
                                let offset = instruction.offset + word(3) as usize;
                                pc = instructions
                                    .binary_search_by_key(&offset, |op| op.offset)
                                    .unwrap();
                            }
                            break;
                        }
                        0
                    }
                    14 => {
                        let value: D3DSTATUS = bytemuck::pod_read_unaligned(record);
                        if value.dwFlags & 1 != 0 {
                            status.dwStatus = value.dwStatus;
                        }
                        if value.dwFlags & 2 != 0 {
                            extent = Some(value.drExtent);
                        }
                        0
                    }
                    _ => unreachable!(),
                };
                if hr != 0 {
                    return Ok(hr);
                }
                if let Some(rect) = pick
                    && let Some(depth) = pick_record(instruction.opcode, record, &vertices, rect)
                {
                    picked.push(D3DPICKRECORD {
                        bOpcode: instruction.opcode,
                        dwOffset: (instruction.offset + 4 + record_index * instruction.size) as u32,
                        dvZ: depth,
                        ..Default::default()
                    });
                }
            }
            if pick.is_none() && !indices.is_empty() {
                update_extent(
                    &mut extent,
                    indices
                        .iter()
                        .map(|&i| &vertices[i as usize * TL_SIZE..][..TL_SIZE]),
                );
                let primitive = match instruction.opcode {
                    1 | 13 => 1,
                    2 => 2,
                    3 => 4,
                    _ => unreachable!(),
                };
                let edges = (instruction.opcode == 3).then(|| {
                    instruction
                        .records
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|record| u16::from_le_bytes([record[6], record[7]]))
                        .collect::<Vec<_>>()
                });
                let hr = self.emit_primitive(
                    this,
                    PrimitiveBatch {
                        primitive,
                        fvf: 0x1c4,
                        flags: if flags == 1 { 0 } else { 4 },
                        vertices: vertices.clone(),
                        indices: Some(indices),
                    },
                    edges.as_deref(),
                )?;
                if hr != 0 {
                    return Ok(hr);
                }
            }
        }
        if let Some(extent) = extent {
            status.drExtent = extent;
        }
        if let Some(Object::ExecuteBuffer(buffer)) = com.get_mut::<Object>(self.objects, pointer) {
            buffer.execute.dsStatus = status;
        }
        if pick.is_some() {
            device_mut(self.draw, self.objects, this)
                .unwrap()
                .legacy
                .picked = picked;
        }
        Ok(0)
    }

    pub(super) fn pick_records(&mut self, args: &[u32]) -> Result<u32, CallError> {
        let Some(device) = device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let records = &device.legacy.picked;
        if args[2] != 0 {
            let limit = self.memory.arg_u32(args[1])? as usize;
            if limit < records.len() {
                return Ok(0x887602b2);
            }
            self.memory
                .arg_write(args[2], bytemuck::cast_slice(records))?;
        }
        self.memory.arg_put_u32(args[1], records.len() as u32)?;
        Ok(0)
    }
}

pub(super) fn validate(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    use crate::ddraw::front;
    use adv_cpu::backend::Prot;
    let state = env.ext.get::<front::State>();
    let Some(Object::ExecuteBuffer(buffer)) = state
        .draw
        .com
        .unwrap()
        .get::<Object>(&env.sys.objects, args[0])
    else {
        return Outcome::Return(INVALID);
    };
    if buffer.locked {
        return Outcome::Return(EXECUTE_LOCKED);
    }
    let mut bytes = vec![0; buffer.size as usize];
    if env.cpu.read(buffer.data, &mut bytes).is_err() {
        return Outcome::Return(INVALID);
    }
    if args[4] != 0 || (args[2] != 0 && !env.cpu.accessible(args[2], 1, Prot::EXEC)) {
        return Outcome::Return(INVALID);
    }
    let Err(offset) = execute::decode(&bytes, buffer.execute) else {
        return Outcome::Return(0);
    };
    if args[1] != 0 && env.cpu.write(args[1], &offset.to_le_bytes()).is_err() {
        return Outcome::Return(INVALID);
    }
    if args[2] == 0 {
        Outcome::Return(EXECUTE_FAILED)
    } else {
        Outcome::call(args[2], vec![args[3], offset], |_, _| {
            Outcome::Return(EXECUTE_FAILED)
        })
    }
}

fn update_extent<'a>(extent: &mut Option<D3DRECT>, vertices: impl Iterator<Item = &'a [u8]>) {
    for raw in vertices {
        let v: D3DTLVERTEX = bytemuck::pod_read_unaligned(raw);
        if !v.sx.is_finite() || !v.sy.is_finite() {
            continue;
        }
        let next = D3DRECT {
            x1: (v.sx.floor() as i32).saturating_sub(1),
            y1: (v.sy.floor() as i32).saturating_sub(1),
            x2: (v.sx.ceil() as i32).saturating_add(1),
            y2: (v.sy.ceil() as i32).saturating_add(1),
        };
        *extent = Some(if let Some(old) = extent {
            D3DRECT {
                x1: old.x1.min(next.x1),
                y1: old.y1.min(next.y1),
                x2: old.x2.max(next.x2),
                y2: old.y2.max(next.y2),
            }
        } else {
            next
        });
    }
}

fn pick_record(opcode: u8, record: &[u8], vertices: &[u8], rect: D3DRECT) -> Option<f32> {
    let short =
        |i: usize| usize::from(u16::from_le_bytes(record[i * 2..][..2].try_into().unwrap()));
    let indices = match opcode {
        1 | 13 => short(1)..short(1) + short(0),
        2 => 0..2,
        3 => 0..3,
        _ => return None,
    };
    let position = |index| {
        let v =
            bytemuck::pod_read_unaligned::<D3DTLVERTEX>(&vertices[index * TL_SIZE..][..TL_SIZE]);
        [v.sx, v.sy, v.sz]
    };
    let inside = |[x, y, _]: [f32; 3]| {
        x >= rect.x1 as f32 && y >= rect.y1 as f32 && x < rect.x2 as f32 && y < rect.y2 as f32
    };
    if !matches!(opcode, 2 | 3) {
        return indices
            .map(position)
            .filter(|&p| inside(p))
            .map(|p| p[2])
            .reduce(f32::min);
    }
    let mut polygon: Vec<_> = indices.map(|i| position(short(i))).collect();
    let bounds = [
        (0, rect.x1 as f32, 1.),
        (0, rect.x2 as f32, -1.),
        (1, rect.y1 as f32, 1.),
        (1, rect.y2 as f32, -1.),
    ];
    for (axis, boundary, sign) in bounds {
        let mut clipped = Vec::with_capacity(polygon.len() + 1);
        let mut before = *polygon.last()?;
        for after in polygon.drain(..) {
            let a = (before[axis] - boundary) * sign;
            let b = (after[axis] - boundary) * sign;
            if (a < 0.) != (b < 0.) {
                let fraction = a / (a - b);
                clipped.push(std::array::from_fn(|n| {
                    before[n] + fraction * (after[n] - before[n])
                }));
            }
            if b >= 0. {
                clipped.push(after);
            }
            before = after;
        }
        polygon = clipped;
    }
    polygon.into_iter().map(|p| p[2]).reduce(f32::min)
}
