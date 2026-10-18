//! Bounded reader for the D3DX9 binary Effect container (0xfeff0901).
//! The format is verified against fxc output authored in tools/fixtures.
//! Wine is a format/semantic reference only; no Wine code is linked or copied.
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 4096;

#[derive(Clone, Debug)]
pub struct Parameter {
    pub name: String,
    pub semantic: String,
    pub full_name: String,
    pub kind: u32,
    pub class: u32,
    pub rows: u32,
    pub columns: u32,
    pub elements: u32,
    pub flags: u32,
    pub bytes: Vec<u8>,
    pub children: Vec<usize>,
    pub annotations: Vec<usize>,
    pub sampler: Vec<State>,
    pub object: Option<usize>,
}
#[derive(Clone, Debug)]
pub struct State {
    pub operation: u32,
    pub index: u32,
    pub parameter: usize,
    pub reference: Option<usize>,
}
#[derive(Clone, Debug)]
pub struct Pass {
    pub name: String,
    pub annotations: Vec<usize>,
    pub states: Vec<State>,
}
#[derive(Clone, Debug)]
pub struct Technique {
    pub name: String,
    pub annotations: Vec<usize>,
    pub passes: Vec<Pass>,
}
#[derive(Clone, Debug)]
pub struct Effect {
    pub parameters: Vec<Parameter>,
    pub roots: Vec<usize>,
    pub techniques: Vec<Technique>,
    pub objects: Vec<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unsupported(&'static str),
}
type Result<T> = std::result::Result<T, Error>;
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn slice(&self, at: usize, len: usize) -> Result<&'a [u8]> {
        self.data
            .get(
                at..at
                    .checked_add(len)
                    .ok_or(Error::Invalid("offset overflow"))?,
            )
            .ok_or(Error::Invalid("truncated Effect"))
    }
    fn u32(&mut self) -> Result<u32> {
        let v = u32::from_le_bytes(self.slice(self.at, 4)?.try_into().unwrap());
        self.at += 4;
        Ok(v)
    }
    fn count(&mut self) -> Result<usize> {
        let value = self.u32()? as usize;
        if value > MAX_ITEMS {
            return Err(Error::Invalid("Effect table limit"));
        }
        Ok(value)
    }
    fn name(&self, at: usize) -> Result<String> {
        let mut r = Reader {
            data: self.data,
            at,
        };
        let len = r.count()?;
        if len == 0 {
            return Ok(String::new());
        }
        let value = r.slice(r.at, len)?;
        if value.last() != Some(&0) {
            return Err(Error::Invalid("unterminated Effect name"));
        }
        Ok(String::from_utf8_lossy(&value[..len - 1]).into_owned())
    }
    fn blob(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        if len > MAX_BYTES {
            return Err(Error::Invalid("Effect object limit"));
        }
        let bytes = self.slice(self.at, len)?.to_vec();
        self.at = self
            .at
            .checked_add((len + 3) & !3)
            .ok_or(Error::Invalid("object overflow"))?;
        self.slice(self.at, 0)?;
        Ok(bytes)
    }
}

impl Effect {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 24 || bytes.len() > MAX_BYTES {
            return Err(Error::Invalid("Effect size"));
        }
        let mut header = Reader { data: bytes, at: 0 };
        if header.u32()? != 0xfeff0901 {
            return Err(Error::Unsupported("HLSL Effect needs a compiler"));
        }
        let start = header.u32()? as usize;
        let data = &bytes[8..];
        let mut reader = Reader { data, at: start };
        let roots = reader.count()?;
        let techniques = reader.count()?;
        let _shader_slots = reader.count()?;
        let objects = reader.count()?;
        let mut effect = Self {
            parameters: Vec::new(),
            roots: Vec::new(),
            techniques: Vec::new(),
            objects: vec![Vec::new(); objects],
        };
        for _ in 0..roots {
            let ty = reader.u32()? as usize;
            let value = reader.u32()? as usize;
            let flags = reader.u32()?;
            let annotations = reader.count()?;
            let id = effect.parameter(data, ty, value, flags, "", 0)?;
            effect.parameters[id].annotations = effect.annotations(&mut reader, annotations)?;
            effect.roots.push(id);
        }
        for _ in 0..techniques {
            let name_offset = reader.u32()? as usize;
            let name = reader.name(name_offset)?;
            let annotations = reader.count()?;
            let passes = reader.count()?;
            let mut technique = Technique {
                name,
                annotations: effect.annotations(&mut reader, annotations)?,
                passes: Vec::new(),
            };
            for _ in 0..passes {
                let name_offset = reader.u32()? as usize;
                let name = reader.name(name_offset)?;
                let annotations = reader.count()?;
                let states = reader.count()?;
                let annotations = effect.annotations(&mut reader, annotations)?;
                let mut pass = Pass {
                    name,
                    annotations,
                    states: Vec::new(),
                };
                for _ in 0..states {
                    pass.states.push(effect.state(&mut reader, 0)?);
                }
                technique.passes.push(pass);
            }
            effect.techniques.push(technique);
        }
        let strings = reader.count()?;
        let resources = reader.count()?;
        for _ in 0..strings {
            let id = reader.u32()? as usize;
            let blob = reader.blob()?;
            *effect
                .objects
                .get_mut(id)
                .ok_or(Error::Invalid("Effect object index"))? = blob;
        }
        let names: BTreeMap<_, _> = effect
            .parameters
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.full_name.is_empty())
            .map(|(i, p)| (p.full_name.clone(), i))
            .collect();
        for _ in 0..resources {
            let technique = reader.u32()?;
            let index = reader.u32()? as usize;
            let element = reader.u32()?;
            let state_index = reader.u32()? as usize;
            let usage = reader.u32()?;
            let blob = reader.blob()?;
            let mut sampler_id = None;
            let state = if technique == u32::MAX {
                let mut id = *effect
                    .roots
                    .get(index)
                    .ok_or(Error::Invalid("sampler parameter index"))?;
                if element != u32::MAX && effect.parameters[id].elements != 0 {
                    id = *effect.parameters[id]
                        .children
                        .get(element as usize)
                        .ok_or(Error::Invalid("sampler element index"))?;
                }
                sampler_id = Some(id);
                effect.parameters[id].sampler.get(state_index)
            } else {
                effect
                    .techniques
                    .get(technique as usize)
                    .and_then(|t| t.passes.get(index))
                    .and_then(|p| p.states.get(state_index))
            }
            .cloned()
            .ok_or(Error::Invalid("Effect state resource index"))?;
            let param = &effect.parameters[state.parameter];
            match usage {
                0 if matches!(param.kind, 15 | 16) => {
                    let id = param
                        .object
                        .ok_or(Error::Invalid("shader object missing"))?;
                    *effect
                        .objects
                        .get_mut(id)
                        .ok_or(Error::Invalid("shader object index"))? = blob;
                }
                1 => {
                    let name = blob.split(|v| *v == 0).next().unwrap_or_default();
                    let name = String::from_utf8_lossy(name);
                    let reference = Some(
                        *names
                            .get(name.as_ref())
                            .ok_or(Error::Invalid("Effect referenced parameter"))?,
                    );
                    if let Some(id) = sampler_id {
                        effect.parameters[id].sampler[state_index].reference = reference;
                    } else {
                        effect.techniques[technique as usize].passes[index].states[state_index]
                            .reference = reference;
                    }
                }
                0 => return Err(Error::Unsupported("Effect expression bytecode")),
                2 => return Err(Error::Unsupported("Effect array selector expression")),
                _ => return Err(Error::Invalid("Effect resource usage")),
            }
        }
        Ok(effect)
    }

    fn annotations(&mut self, reader: &mut Reader<'_>, count: usize) -> Result<Vec<usize>> {
        (0..count)
            .map(|_| {
                let ty = reader.u32()? as usize;
                let value = reader.u32()? as usize;
                self.parameter(reader.data, ty, value, 2, "", 0)
            })
            .collect()
    }

    fn state(&mut self, reader: &mut Reader<'_>, depth: u32) -> Result<State> {
        let operation = reader.u32()?;
        if operation > 178 {
            return Err(Error::Invalid("Effect operation"));
        }
        let index = reader.u32()?;
        let ty = reader.u32()? as usize;
        let value = reader.u32()? as usize;
        let parameter = self.parameter(reader.data, ty, value, 0, "", depth)?;
        Ok(State {
            operation,
            index,
            parameter,
            reference: None,
        })
    }

    fn parameter(
        &mut self,
        data: &[u8],
        ty: usize,
        value: usize,
        flags: u32,
        parent: &str,
        depth: u32,
    ) -> Result<usize> {
        if depth > 16 || self.parameters.len() >= MAX_ITEMS {
            return Err(Error::Invalid("Effect parameter tree limit"));
        }
        let mut reader = Reader { data, at: ty };
        let kind = reader.u32()?;
        let class = reader.u32()?;
        let name_offset = reader.u32()? as usize;
        let name = reader.name(name_offset)?;
        let semantic_offset = reader.u32()? as usize;
        let semantic = reader.name(semantic_offset)?;
        let elements = reader.count()? as u32;
        if kind > 19 || class > 5 {
            return Err(Error::Invalid("Effect parameter type"));
        }
        let full_name = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}.{name}")
        };
        let (rows, columns, members, size) = match class {
            0..=3 => {
                let a = reader.u32()?;
                let b = reader.u32()?;
                let (rows, columns) = if class == 1 { (b, a) } else { (a, b) };
                if rows == 0 || columns == 0 || rows > 4 || columns > 4 {
                    return Err(Error::Invalid("Effect numeric dimensions"));
                }
                (rows, columns, 0, rows as usize * columns as usize * 4)
            }
            4 => (0, 0, 0, if (10..=14).contains(&kind) { 0 } else { 4 }),
            5 => (0, 0, reader.count()?, 0),
            _ => unreachable!(),
        };
        let id = self.parameters.len();
        self.parameters.push(Parameter {
            name,
            semantic,
            full_name: full_name.clone(),
            kind,
            class,
            rows,
            columns,
            elements,
            flags,
            bytes: Vec::new(),
            children: Vec::new(),
            annotations: Vec::new(),
            sampler: Vec::new(),
            object: None,
        });
        if elements != 0 {
            if class == 5 {
                return Err(Error::Unsupported("Effect arrays of structures"));
            }
            if (10..=14).contains(&kind) {
                return Err(Error::Unsupported("Effect sampler arrays"));
            }
            if self.parameters.len() + elements as usize > MAX_ITEMS {
                return Err(Error::Invalid("Effect parameter array limit"));
            }
            let mut object_value = value;
            for i in 0..elements {
                let child = self.parameters.len();
                let mut p = self.parameters[id].clone();
                p.elements = 0;
                p.children.clear();
                p.full_name = format!("{full_name}[{i}]");
                self.parameters.push(p);
                self.initialize(child, data, object_value, size, depth + 1)?;
                object_value += if class == 4 { 4 } else { size };
                self.parameters[id].children.push(child);
            }
            self.parameters[id].bytes = self.parameters[id]
                .children
                .iter()
                .flat_map(|i| self.parameters[*i].bytes.clone())
                .collect();
        } else if members != 0 {
            let mut pos = value;
            for _ in 0..members {
                let child = self.parameter(data, reader.at, pos, flags, &full_name, depth + 1)?;
                // Recover consumed typedef length by following its dimensions/tree.
                reader.at = type_end(data, reader.at, depth + 1)?;
                pos += self.parameters[child].bytes.len();
                self.parameters[id].children.push(child);
            }
            self.parameters[id].bytes = self.parameters[id]
                .children
                .iter()
                .flat_map(|i| self.parameters[*i].bytes.clone())
                .collect();
        } else {
            self.initialize(id, data, value, size, depth)?;
        }
        Ok(id)
    }
    fn initialize(
        &mut self,
        id: usize,
        data: &[u8],
        value: usize,
        size: usize,
        depth: u32,
    ) -> Result<()> {
        let p = &self.parameters[id];
        let mut reader = Reader { data, at: value };
        if p.class != 4 {
            self.parameters[id].bytes = reader.slice(value, size)?.to_vec();
        } else if (10..=14).contains(&p.kind) {
            let count = reader.count()?;
            let mut states = Vec::new();
            for _ in 0..count {
                states.push(self.state(&mut reader, depth + 1)?);
            }
            self.parameters[id].sampler = states;
        } else {
            let object = reader.u32()? as usize;
            if object >= self.objects.len() {
                return Err(Error::Invalid("parameter object index"));
            }
            self.parameters[id].object = Some(object);
            self.parameters[id].bytes = vec![0; 4];
        }
        Ok(())
    }
}
fn type_end(data: &[u8], ty: usize, depth: u32) -> Result<usize> {
    if depth > 16 {
        return Err(Error::Invalid("Effect type nesting"));
    }
    let mut reader = Reader { data, at: ty };
    reader.u32()?;
    let class = reader.u32()?;
    reader.u32()?;
    reader.u32()?;
    reader.u32()?;
    if class < 4 {
        reader.u32()?;
        reader.u32()?;
    }
    if class == 5 {
        let count = reader.count()?;
        for _ in 0..count {
            reader.at = type_end(data, reader.at, depth + 1)?;
        }
    }
    Ok(reader.at)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn simple() -> Vec<u8> {
        // One float4 root and one empty technique, constructed from format
        // records rather than copied shader or Effect content.
        let mut data = vec![
            1u32, 1, 0, 1, 56, 84, 0, 0, 104, 0, 0, 0, 0, 0, 3, 1, 120, 128, 0, 4, 1, 0x3e800000,
            0x3f000000, 0x3f400000, 0x3f800000, 8, 0x6d697250, 0x00797261, 0, 4, 0x00656e6f, 0,
        ];
        // Payload string offsets: technique at100, root name at116,
        // and an empty semantic at128.
        data[8] = 100;
        data[16] = 116;
        data[17] = 112;
        data.pop();
        let mut output = vec![0xfeff0901u32, 0];
        output.extend(data);
        bytemuck::cast_slice(&output).to_vec()
    }
    #[test]
    // spec: GFX-EFFECT-1
    fn bounded_binary_reader_preserves_parameter_layout_and_rejects_all_prefixes() {
        let data = simple();
        let effect = Effect::parse(&data).unwrap();
        assert_eq!(effect.roots.len(), 1);
        assert_eq!(effect.techniques.len(), 1);
        let p = &effect.parameters[effect.roots[0]];
        assert_eq!((p.kind, p.class, p.rows, p.columns), (3, 1, 1, 4));
        assert_eq!(p.bytes.len(), 16);
        for len in 0..data.len() {
            assert!(Effect::parse(&data[..len]).is_err(), "prefix {len}");
        }
        for i in 0..data.len() / 4 {
            let mut corrupted = data.clone();
            corrupted[i * 4..i * 4 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let _ = Effect::parse(&corrupted);
        }
    }
    #[test]
    // spec: GFX-EFFECT-1
    fn configured_authored_fxc_effect_matches_native_description_and_preshader() {
        let Some(path) = std::env::var_os("ADV_TEST_AUTHORED_EFFECT_FXO") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let effect = Effect::parse(&data).unwrap();
        assert_eq!(effect.roots.len(), 1);
        assert_eq!(effect.techniques.len(), 1);
        assert_eq!(effect.techniques[0].passes.len(), 1);
        assert_eq!(effect.parameters[effect.roots[0]].name, "tone");
        let shaders: Vec<_> = effect
            .parameters
            .iter()
            .filter(|p| matches!(p.kind, 15 | 16))
            .filter_map(|p| p.object)
            .filter(|i| !effect.objects[*i].is_empty())
            .collect();
        assert_eq!(shaders.len(), 1);
        let words: Vec<_> = effect.objects[shaders[0]]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| u32::from_le_bytes(*v))
            .collect();
        assert!(
            super::super::preshader::comments(&words)
                .unwrap()
                .iter()
                .any(|(tag, _)| *tag == 0x53455250)
        );
    }
}
