//! `.rsrc` tree: `IMAGE_RESOURCE_DIRECTORY` levels (type → name → language) and data entries.

use crate::{DataDir, Pe, PeError, Result, add, reserve};

/// A resource type, name, or language key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResId {
    Id(u16),
    /// Named entries; compared case-insensitively like the Win32 API does.
    Name(String),
}

impl ResId {
    pub fn matches(&self, other: &ResId) -> bool {
        match (self, other) {
            (ResId::Id(a), ResId::Id(b)) => a == b,
            (ResId::Name(a), ResId::Name(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceData {
    /// RVA of IMAGE_RESOURCE_DATA_ENTRY, used as the image-owned HRSRC.
    pub entry_rva: u32,
    /// RVA of the data in the mapped image (LoadResource returns `base + rva`).
    pub rva: u32,
    pub size: u32,
    pub codepage: u32,
}

#[derive(Clone, Debug)]
pub enum ResourceNode {
    Dir(ResourceDir),
    Data(ResourceData),
}

#[derive(Clone, Debug)]
pub struct ResourceEntry {
    pub id: ResId,
    pub node: ResourceNode,
}

#[derive(Clone, Debug, Default)]
pub struct ResourceDir {
    pub entries: Vec<ResourceEntry>,
}

/// One leaf of the standard three-level tree.
#[derive(Clone, Debug)]
pub struct ResourceLeaf {
    pub ty: ResId,
    pub name: ResId,
    pub lang: u16,
    pub data: ResourceData,
}

impl ResourceDir {
    pub fn get(&self, id: &ResId) -> Option<&ResourceNode> {
        self.entries
            .iter()
            .find(|e| e.id.matches(id))
            .map(|e| &e.node)
    }

    fn subdir(&self, id: &ResId) -> Option<&ResourceDir> {
        match self.get(id)? {
            ResourceNode::Dir(d) => Some(d),
            ResourceNode::Data(_) => None,
        }
    }

    /// FindResourceEx-style lookup. With `lang` given, prefer that language, then
    /// language-neutral (0), then the first entry; with `None`, take the first entry.
    pub fn find(&self, ty: &ResId, name: &ResId, lang: Option<u16>) -> Option<(u16, ResourceData)> {
        let langs = self.subdir(ty)?.subdir(name)?;
        let data = |e: &ResourceEntry| match (&e.id, &e.node) {
            (ResId::Id(l), ResourceNode::Data(d)) => Some((*l, *d)),
            _ => None,
        };
        let pick = |want: u16| {
            langs
                .entries
                .iter()
                .find(|e| e.id == ResId::Id(want))
                .and_then(data)
        };
        lang.and_then(pick)
            .or_else(|| pick(0))
            .or_else(|| langs.entries.iter().find_map(data))
    }

    /// All type/name/language leaves, in directory order.
    pub fn leaves(&self) -> Vec<ResourceLeaf> {
        let mut out = Vec::new();
        for t in &self.entries {
            let ResourceNode::Dir(names) = &t.node else {
                continue;
            };
            for n in &names.entries {
                let ResourceNode::Dir(langs) = &n.node else {
                    continue;
                };
                for l in &langs.entries {
                    if let (ResId::Id(lang), ResourceNode::Data(d)) = (&l.id, &l.node) {
                        out.push(ResourceLeaf {
                            ty: t.id.clone(),
                            name: n.id.clone(),
                            lang: *lang,
                            data: *d,
                        });
                    }
                }
            }
        }
        out
    }
}

pub(crate) fn parse(pe: &Pe, dir: DataDir) -> Result<ResourceDir> {
    let bytes = pe.slice(dir.rva, dir.size)?;
    ResourceDir::read(dir.rva, |rva, output| {
        let offset = rva.checked_sub(dir.rva).ok_or(PeError::OutOfImage {
            rva,
            len: output.len() as u32,
        })?;
        let end = add(offset, output.len() as u32)?;
        let source = bytes
            .get(offset as usize..end as usize)
            .ok_or(PeError::OutOfImage {
                rva,
                len: output.len() as u32,
            })?;
        output.copy_from_slice(source);
        Ok(())
    })
}

impl ResourceDir {
    /// Decode the same resource tree from an image or a checked raw-file RVA
    /// reader. The reader supplies live bytes; no image-sized buffer is needed.
    pub fn read(root: u32, read: impl FnMut(u32, &mut [u8]) -> Result<()>) -> Result<Self> {
        Reader {
            read,
            nodes: 65536,
            bytes: 16 * 1024 * 1024,
        }
        .directory(root, 0, 0)
    }
}

struct Reader<F> {
    read: F,
    nodes: u32,
    bytes: usize,
}

impl<F: FnMut(u32, &mut [u8]) -> Result<()>> Reader<F> {
    fn read_bytes(&mut self, rva: u32, output: &mut [u8]) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_sub(output.len())
            .ok_or_else(|| PeError::Malformed("resource read budget exceeded".into()))?;
        (self.read)(rva, output)
    }

    fn short(&mut self, rva: u32) -> Result<u16> {
        let mut bytes = [0; 2];
        self.read_bytes(rva, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn directory(&mut self, root: u32, offset: u32, depth: u32) -> Result<ResourceDir> {
        if depth > 8 {
            return Err(PeError::Malformed("resource tree too deep".into()));
        }
        let directory = add(root, offset)?;
        let count = u32::from(self.short(add(directory, 12)?)?)
            + u32::from(self.short(add(directory, 14)?)?);
        self.nodes = self
            .nodes
            .checked_sub(count)
            .ok_or_else(|| PeError::Malformed("resource node budget exceeded".into()))?;
        let mut entries = Vec::new();
        // Read the complete bounded entry array before allocating decoded nodes.
        let mut encoded_entries = Vec::new();
        reserve(&mut encoded_entries, count as usize * 8)?;
        encoded_entries.resize(count as usize * 8, 0);
        self.read_bytes(add(directory, 16)?, &mut encoded_entries)?;
        reserve(&mut entries, count as usize)?;
        for entry in encoded_entries.chunks_exact(8) {
            let name = u32::from_le_bytes(entry[..4].try_into().unwrap());
            let target = u32::from_le_bytes(entry[4..].try_into().unwrap());
            let id = if name & 0x8000_0000 != 0 {
                let string = add(root, name & 0x7fff_ffff)?;
                let length = usize::from(self.short(string)?) * 2;
                let mut encoded = Vec::new();
                reserve(&mut encoded, length)?;
                encoded.resize(length, 0);
                self.read_bytes(add(string, 2)?, &mut encoded)?;
                let mut units = Vec::new();
                reserve(&mut units, length / 2)?;
                units.extend(
                    encoded
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|bytes| u16::from_le_bytes(*bytes)),
                );
                ResId::Name(String::from_utf16_lossy(&units))
            } else {
                ResId::Id(name as u16)
            };
            let node = if target & 0x8000_0000 != 0 {
                ResourceNode::Dir(self.directory(root, target & 0x7fff_ffff, depth + 1)?)
            } else {
                let data = add(root, target)?;
                let mut bytes = [0; 16];
                self.read_bytes(data, &mut bytes)?;
                ResourceNode::Data(ResourceData {
                    entry_rva: data,
                    rva: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    size: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                    codepage: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                })
            };
            entries.push(ResourceEntry { id, node });
        }
        Ok(ResourceDir { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyclic_directory_is_bounded_without_recursing_or_reading_forever() {
        let mut bytes = [0; 24];
        bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
        bytes[16..20].copy_from_slice(&10u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&0x80000000u32.to_le_bytes());
        let mut reads = 0;
        let result = ResourceDir::read(0, |rva, output| {
            reads += 1;
            output.copy_from_slice(&bytes[rva as usize..rva as usize + output.len()]);
            Ok(())
        });
        assert!(matches!(result, Err(PeError::Malformed(_))));
        assert_eq!(reads, 27);
    }

    #[test]
    fn overflowing_root_fails_before_calling_the_reader() {
        let mut reads = 0;
        let result = ResourceDir::read(u32::MAX - 8, |_, _| {
            reads += 1;
            Ok(())
        });
        assert!(matches!(result, Err(PeError::OutOfImage { .. })));
        assert_eq!(reads, 0);
    }

    #[test]
    fn oversized_directory_rejects_before_allocating_or_reading_entries() {
        let mut reads = 0;
        let result = ResourceDir::read(0, |_, output| {
            reads += 1;
            assert_eq!(output.len(), 2);
            output.copy_from_slice(&u16::MAX.to_le_bytes());
            Ok(())
        });
        assert!(matches!(result, Err(PeError::Malformed(_))));
        assert_eq!(reads, 2);
    }
}
