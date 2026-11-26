//! Execute buffer records and forward branch targets.
use adv_abi::types::*;

pub(in crate::ddraw::d3d) struct Instruction<'a> {
    pub offset: usize,
    pub opcode: u8,
    pub size: usize,
    pub records: &'a [u8],
}

pub(in crate::ddraw::d3d) fn decode(
    bytes: &[u8],
    data: D3DEXECUTEDATA,
) -> Result<Vec<Instruction<'_>>, u32> {
    let start = data.dwInstructionOffset as usize;
    let bytes = bytes
        .get(start..start + data.dwInstructionLength as usize)
        .ok_or(0u32)?;
    let mut instructions = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let error = offset as u32;
        let header = bytes.get(offset..offset + 4).ok_or(error)?;
        let opcode = header[0];
        let size = header[1] as usize;
        let expected = match opcode {
            1 | 2 | 13 => 4,
            3 | 4 | 6..=8 | 10 => 8,
            5 => 12,
            9 | 12 => 16,
            11 => 0,
            14 => 24,
            _ => return Err(error),
        };
        let count = u16::from_le_bytes([header[2], header[3]]) as usize;
        if expected != size || (opcode == 11 && count != 0) {
            return Err(error);
        }
        let end = offset + 4 + size * count;
        let records = bytes.get(offset + 4..end).ok_or(error)?;
        instructions.push(Instruction {
            offset,
            opcode,
            size,
            records,
        });
        offset = end;
    }
    if instructions
        .last()
        .is_none_or(|instruction| instruction.opcode != 11)
    {
        return Err(offset as u32);
    }
    for instruction in &instructions {
        let fail = Err(instruction.offset as u32);
        let n = data.dwVertexCount as usize;
        match instruction.opcode {
            1 | 13 => {
                if instruction.records.as_chunks::<4>().0.iter().any(|record| {
                    let point: D3DPOINT = bytemuck::pod_read_unaligned(record);
                    usize::from(point.wFirst) + usize::from(point.wCount) > n
                }) {
                    return fail;
                }
            }
            2 | 3 => {
                if instruction
                    .records
                    .chunks_exact(instruction.size)
                    .any(|record| {
                        record
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .take(if instruction.opcode == 2 { 2 } else { 3 })
                            .any(|word| usize::from(u16::from_le_bytes(*word)) >= n)
                    })
                {
                    return fail;
                }
            }
            9 => {
                if instruction
                    .records
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .any(|record| {
                        let op: D3DPROCESSVERTICES = bytemuck::pod_read_unaligned(record);
                        (op.dwFlags & 7) > 2
                            || usize::from(op.wStart) + op.dwCount as usize > n
                            || usize::from(op.wDest) + op.dwCount as usize > n
                    })
                {
                    return fail;
                }
            }
            12 if instruction
                .records
                .as_chunks::<16>()
                .0
                .iter()
                .any(|record| {
                    let branch: D3DBRANCH = bytemuck::pod_read_unaligned(record);
                    let target = instruction.offset + branch.dwOffset as usize;
                    branch.dwOffset != 0
                        && (target <= instruction.offset
                            || instructions
                                .binary_search_by_key(&target, |op| op.offset)
                                .is_err())
                }) =>
            {
                return fail;
            }
            _ => {}
        }
    }
    Ok(instructions)
}
