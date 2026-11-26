//! Class types, named instances and interface-slot tables from DXBC IFCE.

use super::{Container, ParseError};

pub struct ClassType {
    pub name: Vec<u8>,
    pub constants: u16,
    pub textures: u16,
    pub samplers: u16,
}

pub struct ClassInstance {
    pub name: Vec<u8>,
    pub class: u16,
    pub offsets: [u32; 4],
}

pub struct Linkage {
    pub classes: Vec<ClassType>,
    pub instances: Vec<ClassInstance>,
    slots: Vec<(usize, Vec<(u16, u32)>)>,
}

impl Linkage {
    pub fn decode(bytecode: &[u8]) -> Result<Option<Self>, ParseError> {
        Container::decode(bytecode)?
            .chunks()
            .find(|(tag, _)| tag == b"IFCE")
            .map(|(_, bytes)| Self::parse(bytes).ok_or(ParseError::Chunk(*b"IFCE")))
            .transpose()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        fn word(bytes: &[u8], at: usize) -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        }
        fn half(bytes: &[u8], at: usize) -> Option<u16> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        }
        fn name(bytes: &[u8], at: usize) -> Option<Vec<u8>> {
            let data = bytes.get(word(bytes, at)? as usize..)?;
            Some(
                data.get(..data.iter().position(|&byte| byte == 0)?)?
                    .to_vec(),
            )
        }

        let instance_count = word(bytes, 0)? as usize;
        let class_count = word(bytes, 4)? as usize;
        let slot_records = word(bytes, 8)? as usize;
        let slot_count = word(bytes, 12)? as usize;
        let classes_at = word(bytes, 20)? as usize;
        let slots_at = word(bytes, 24)? as usize;
        bytes.get(classes_at..classes_at + class_count * 12 + instance_count * 16)?;
        bytes.get(slots_at..slots_at + slot_records * 16)?;

        let mut classes = Vec::new();
        for at in (classes_at..).step_by(12).take(class_count) {
            classes.push(ClassType {
                name: name(bytes, at)?,
                constants: half(bytes, at + 6)?,
                textures: half(bytes, at + 8)?,
                samplers: half(bytes, at + 10)?,
            });
        }
        let mut instances = Vec::new();
        for at in (classes_at + class_count * 12..)
            .step_by(16)
            .take(instance_count)
        {
            instances.push(ClassInstance {
                name: name(bytes, at)?,
                class: half(bytes, at + 4)?,
                offsets: [
                    u32::from(half(bytes, at + 8)?),
                    u32::from(half(bytes, at + 10)?),
                    u32::from(half(bytes, at + 12)?),
                    u32::from(half(bytes, at + 14)?),
                ],
            });
        }
        let mut slots = Vec::new();
        for at in (slots_at..).step_by(16).take(slot_records) {
            let count = word(bytes, at)? as usize;
            let choices = word(bytes, at + 4)? as usize;
            let types_at = word(bytes, at + 8)? as usize;
            let tables_at = word(bytes, at + 12)? as usize;
            bytes.get(types_at..types_at + choices * 2)?;
            bytes.get(tables_at..tables_at + choices * 4)?;
            let entries = (0..choices)
                .map(|index| {
                    Some((
                        half(bytes, types_at + index * 2)?,
                        word(bytes, tables_at + index * 4)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            slots.push((count, entries));
        }
        if slots.iter().map(|(count, _)| count).sum::<usize>() != slot_count || slot_count > 256 {
            return None;
        }
        Some(Self {
            classes,
            instances,
            slots,
        })
    }

    pub fn tables(&self, instances: &[&[u8]]) -> Option<Vec<u32>> {
        let mut instances = instances.iter();
        let mut tables = Vec::new();
        for (count, entries) in &self.slots {
            for _ in 0..*count {
                let class = instances.next()?;
                let (_, table) = entries.iter().find(|(id, _)| {
                    self.classes
                        .get(usize::from(*id))
                        .is_some_and(|ty| &ty.name.as_slice() == class)
                })?;
                tables.push(*table);
            }
        }
        instances.next().is_none().then_some(tables)
    }
}
