//! `.rsrc` tree: `IMAGE_RESOURCE_DIRECTORY` levels (type → name → language) and data entries.

use crate::{Pe, PeError, Result};

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

pub(crate) fn parse(pe: &Pe, root: u32) -> Result<ResourceDir> {
    parse_dir(pe, root, 0, 0)
}

fn parse_dir(pe: &Pe, root: u32, off: u32, depth: u32) -> Result<ResourceDir> {
    if depth > 8 {
        return Err(PeError::Malformed("resource tree too deep".into()));
    }
    let d = root + off;
    let n = u32::from(pe.u16_at(d + 12)?) + u32::from(pe.u16_at(d + 14)?);
    let mut entries = Vec::with_capacity(n as usize);
    for i in 0..n {
        let e = d + 16 + 8 * i;
        let (name, target) = (pe.u32_at(e)?, pe.u32_at(e + 4)?);
        let id = if name & 0x8000_0000 != 0 {
            let s = root + (name & 0x7fff_ffff);
            let len = u32::from(pe.u16_at(s)?);
            let units: Vec<u16> = pe
                .slice(s + 2, len * 2)?
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            ResId::Name(String::from_utf16_lossy(&units))
        } else {
            ResId::Id(name as u16)
        };
        let node = if target & 0x8000_0000 != 0 {
            ResourceNode::Dir(parse_dir(pe, root, target & 0x7fff_ffff, depth + 1)?)
        } else {
            let de = root + target;
            ResourceNode::Data(ResourceData {
                rva: pe.u32_at(de)?,
                size: pe.u32_at(de + 4)?,
                codepage: pe.u32_at(de + 8)?,
            })
        };
        entries.push(ResourceEntry { id, node });
    }
    Ok(ResourceDir { entries })
}
