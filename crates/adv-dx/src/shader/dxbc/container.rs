use super::{Error, Program, checksum::checksum, lower};
use crate::shader::ir;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Header,
    Checksum,
    Chunk([u8; 4]),
    MissingProgram,
    Profile,
    Tokens(Error),
    Ir(ir::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DXBC: {self:?}")
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug)]
pub struct Container<'a> {
    bytes: &'a [u8],
    chunks: Vec<([u8; 4], Range<usize>)>,
}

impl<'a> Container<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, ParseError> {
        let invalid = ParseError::Header;
        if bytes.get(..4) != Some(b"DXBC")
            || word(bytes, 20) != Some(1)
            || word(bytes, 24).map(|size| size as usize) != Some(bytes.len())
        {
            return Err(invalid);
        }
        let count = word(bytes, 28).ok_or(invalid)? as usize;
        let directory = records(bytes, 32, count, 4).ok_or(invalid)?;
        if checksum(&bytes[20..]) != bytes[4..20] {
            return Err(ParseError::Checksum);
        }
        let mut chunks = Vec::new();
        for entry in directory {
            let start = u32::from_le_bytes(entry.try_into().unwrap()) as usize;
            let tag = bytes
                .get(start..start + 4)
                .ok_or(invalid)?
                .try_into()
                .unwrap();
            let error = ParseError::Chunk(tag);
            let length = word(bytes, start + 4).ok_or(error)? as usize;
            let end = start + 8 + length;
            if start < 32 + count * 4 || end > bytes.len() {
                return Err(error);
            }
            chunks.push((tag, start + 8..end));
        }
        Ok(Self { bytes, chunks })
    }

    pub fn chunks(&self) -> impl Iterator<Item = ([u8; 4], &'a [u8])> + '_ {
        self.chunks
            .iter()
            .map(|(tag, range)| (*tag, &self.bytes[range.clone()]))
    }

    /// Re-encode token chunks through the token model and preserve the other
    /// chunks, directory order, padding and offsets.
    pub fn encode(&self) -> Result<Vec<u8>, ParseError> {
        let mut bytes = self.bytes.to_vec();
        for (tag, range) in &self.chunks {
            if matches!(tag, b"SHDR" | b"SHEX") {
                let tokens = words(*tag, &self.bytes[range.clone()])?;
                let code = Program::decode(&tokens)
                    .map_err(ParseError::Tokens)?
                    .encode();
                for (field, token) in bytes[range.clone()]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(code)
                {
                    *field = token.to_le_bytes();
                }
            }
        }
        let hash = checksum(&bytes[20..]);
        bytes[4..20].copy_from_slice(&hash);
        Ok(bytes)
    }
}

pub fn parse(bytes: &[u8]) -> Result<ir::Module, ParseError> {
    let container = Container::decode(bytes)?;
    let mut tokens = None;
    let mut signatures = ir::Signatures::default();
    let mut feature_flags = 0;
    let mut statistics = Vec::new();
    let mut reflection = None;
    let mut downlevel = None;
    for (tag, data) in container.chunks() {
        let error = ParseError::Chunk(tag);
        match &tag {
            b"SHDR" | b"SHEX" => {
                if tokens.replace(words(tag, data)?).is_some() {
                    return Err(error);
                }
            }
            b"ISGN" | b"ISG1" => signatures.input = signature(tag, data)?,
            b"OSGN" | b"OSG5" | b"OSG1" => signatures.output = signature(tag, data)?,
            b"PCSG" | b"PSG1" => signatures.patch_constant = signature(tag, data)?,
            b"SFI0" => feature_flags = u64::from_le_bytes(data.try_into().map_err(|_| error)?),
            b"STAT" => statistics = words(tag, data)?,
            b"RDEF" => reflection = Some(rdef(data).ok_or(error)?),
            b"Aon9" => {
                let offset = word(data, 12).ok_or(error)? as usize;
                downlevel = Some(word(data, offset).ok_or(error)?);
            }
            _ => {}
        }
    }
    let tokens = tokens.ok_or(ParseError::MissingProgram)?;
    let program = Program::decode(&tokens).map_err(ParseError::Tokens)?;
    let stage = match program.version.stage {
        0 => ir::Stage::Pixel,
        1 => ir::Stage::Vertex,
        2 => ir::Stage::Geometry,
        3 => ir::Stage::Hull,
        4 => ir::Stage::Domain,
        5 => ir::Stage::Compute,
        _ => return Err(ParseError::Profile),
    };
    let model = match (program.version.major, program.version.minor, downlevel) {
        (4, 0, None) => ir::Model::Sm40,
        (4, 1, None) => ir::Model::Sm41,
        (5, 0, None) => ir::Model::Sm50,
        (4, 0, Some(version)) => match version & 0xffff {
            0x0200 => ir::Model::Level91,
            0x0201 | 0x0300 => ir::Model::Level93,
            _ => return Err(ParseError::Profile),
        },
        _ => return Err(ParseError::Profile),
    };
    let module = ir::Module {
        stage,
        model,
        signatures,
        feature_flags,
        statistics,
        reflection,
        body: lower::program(&program).map_err(ParseError::Ir)?,
    };
    ir::verify(&module).map_err(ParseError::Ir)?;
    Ok(module)
}

fn word(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn words(tag: [u8; 4], data: &[u8]) -> Result<Vec<u32>, ParseError> {
    let (words, tail) = data.as_chunks::<4>();
    if !tail.is_empty() {
        return Err(ParseError::Chunk(tag));
    }
    Ok(words.iter().copied().map(u32::from_le_bytes).collect())
}

fn records(
    data: &[u8],
    offset: usize,
    count: usize,
    stride: usize,
) -> Option<std::slice::ChunksExact<'_, u8>> {
    Some(
        data.get(offset..offset + count * stride)?
            .chunks_exact(stride),
    )
}

fn string(data: &[u8], offset: u32) -> Option<String> {
    let tail = data.get(offset as usize..)?;
    let name = &tail[..tail.iter().position(|&byte| byte == 0)?];
    Some(std::str::from_utf8(name).ok()?.into())
}

fn signature(tag: [u8; 4], data: &[u8]) -> Result<Vec<ir::SignatureElement>, ParseError> {
    let error = ParseError::Chunk(tag);
    let count = word(data, 0).ok_or(error)? as usize;
    let offset = word(data, 4).ok_or(error)? as usize;
    let extended = matches!(&tag, b"ISG1" | b"OSG1" | b"PSG1");
    let stream = extended || &tag == b"OSG5";
    let stride = 24 + 4 * usize::from(stream) + 4 * usize::from(extended);
    let invert_used = matches!(&tag, b"OSGN" | b"OSG1" | b"OSG5");
    records(data, offset, count, stride)
        .ok_or(error)?
        .map(|record| {
            let first = 4 * usize::from(stream);
            let get = |index: usize| word(record, first + index * 4).unwrap();
            let semantic = string(data, get(0)).ok_or(error)?;
            let system_value = system_value(get(2), &semantic).ok_or(error)?;
            let value_type = match get(3) {
                0 => ir::ValueType::Bits,
                1 => ir::ValueType::Uint,
                2 => ir::ValueType::Int,
                3 => ir::ValueType::Float,
                _ => return Err(error),
            };
            let mask = get(5) as u8;
            let used_mask = (get(5) >> 8) as u8;
            Ok(ir::SignatureElement {
                semantic,
                system_value,
                value_type,
                mask,
                semantic_index: get(1),
                register: get(4),
                used_mask: mask & if invert_used { !used_mask } else { used_mask },
                stream: if stream { word(record, 0).unwrap() } else { 0 },
                min_precision: if extended { get(6) } else { 0 },
            })
        })
        .collect()
}

fn system_value(code: u32, name: &str) -> Option<ir::SystemValue> {
    use ir::SystemValue::*;
    Some(match code {
        0 => match name.to_ascii_lowercase().as_str() {
            "sv_target" => Target,
            "sv_depth" => Depth,
            "sv_coverage" => Coverage,
            "sv_depthgreaterequal" => DepthGreaterEqual,
            "sv_depthlessequal" => DepthLessEqual,
            "sv_stencilref" => StencilRef,
            "sv_innercoverage" => InnerCoverage,
            _ => None,
        },
        1 => Position,
        2 => ClipDistance,
        3 => CullDistance,
        4 => RenderTargetArrayIndex,
        5 => ViewportArrayIndex,
        6 => VertexId,
        7 => PrimitiveId,
        8 => InstanceId,
        9 => FrontFace,
        10 => SampleIndex,
        11 | 13 | 15 | 16 => TessFactor,
        12 | 14 => InsideTessFactor,
        64 => Target,
        65 => Depth,
        66 => Coverage,
        67 => DepthGreaterEqual,
        68 => DepthLessEqual,
        69 => StencilRef,
        70 => InnerCoverage,
        _ => return std::option::Option::None,
    })
}

fn rdef(data: &[u8]) -> Option<ir::ReflectionData> {
    let version = word(data, 16)?;
    let sm5 = version & 0xffff >= 0x500;
    let buffers = records(data, word(data, 4)? as usize, word(data, 0)? as usize, 24)?
        .map(|r| {
            let variables = records(
                data,
                word(r, 8)? as usize,
                word(r, 4)? as usize,
                if sm5 { 40 } else { 24 },
            )?
            .map(|v| {
                let bytes = word(v, 8)?;
                let default_offset = word(v, 20)? as usize;
                Some(ir::VariableDescription {
                    name: string(data, word(v, 0)?)?,
                    byte_offset: word(v, 4)?,
                    bytes,
                    flags: word(v, 12)?,
                    ty: variable_type(data, word(v, 16)?, sm5, 0)?,
                    default: if default_offset == 0 {
                        Vec::new()
                    } else {
                        data.get(default_offset..default_offset + bytes as usize)?
                            .into()
                    },
                })
            })
            .collect::<Option<_>>()?;
            Some(ir::BufferDescription {
                name: string(data, word(r, 0)?)?,
                bytes: word(r, 12)?,
                flags: word(r, 16)?,
                kind: word(r, 20)?,
                variables,
            })
        })
        .collect::<Option<_>>()?;
    let resources = records(data, word(data, 12)? as usize, word(data, 8)? as usize, 32)?
        .map(|r| {
            Some(ir::ResourceDescription {
                name: string(data, word(r, 0)?)?,
                kind: word(r, 4)?,
                value_type: word(r, 8)?,
                dimension: word(r, 12)?,
                samples: word(r, 16)?,
                slot: word(r, 20)?,
                count: word(r, 24)?,
                flags: word(r, 28)?,
            })
        })
        .collect::<Option<_>>()?;
    Some(ir::ReflectionData {
        compiler: string(data, word(data, 24)?)?,
        version,
        flags: word(data, 20)?,
        buffers,
        resources,
    })
}

fn variable_type(data: &[u8], offset: u32, sm5: bool, depth: usize) -> Option<ir::VariableType> {
    if depth == crate::shader::MAX_FLOW_DEPTH {
        return None;
    }
    let offset = offset as usize;
    let class = word(data, offset)?;
    let dimensions = word(data, offset + 4)?;
    let elements = word(data, offset + 8)?;
    let members = records(
        data,
        word(data, offset + 12)? as usize,
        (elements >> 16) as usize,
        12,
    )?
    .map(|r| {
        Some((
            string(data, word(r, 0)?)?,
            word(r, 8)?,
            variable_type(data, word(r, 4)?, sm5, depth + 1)?,
        ))
    })
    .collect::<Option<_>>()?;
    let name = if sm5 {
        match word(data, offset + 32)? {
            0 => None,
            name => Some(string(data, name)?),
        }
    } else {
        None
    };
    Some(ir::VariableType {
        class: class as u16,
        scalar_type: (class >> 16) as u16,
        rows: dimensions as u16,
        columns: (dimensions >> 16) as u16,
        elements: elements as u16,
        name,
        members,
    })
}
