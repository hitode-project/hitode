//! Relocatable fingerprints for a reviewed library ABI family.
//!
//! Code and immutable tables are hashed in full. Declared pointer/call fields
//! are normalized and their graph is checked separately, including shared
//! targets and jump-table addends. Slots identify mutable CRT state and imports;
//! their addresses participate in graph identity, their values do not.
use crate::code_signature::ExecutableRange;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

struct Link {
    offset: usize,
    relative: bool,
    target: String,
    addend: u32,
}
struct Node {
    code: bool,
    len: usize,
    hash: Vec<u8>,
    links: Vec<Link>,
}
pub struct Family {
    roots: Vec<(String, usize)>,
    hooks: BTreeMap<String, usize>,
    nodes: BTreeMap<String, Node>,
}
pub struct Library {
    pub symbols: BTreeMap<String, u32>,
    pub hooks: BTreeMap<String, (u32, usize)>,
    pub snapshots: Vec<(u32, Vec<u8>)>,
}
impl Family {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut result = Self {
            roots: Vec::new(),
            hooks: BTreeMap::new(),
            nodes: BTreeMap::new(),
        };
        let mut links = Vec::new();
        for line in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            let integer = |i: usize| {
                fields
                    .get(i)
                    .ok_or("missing integer")?
                    .parse::<usize>()
                    .map_err(|_| "bad integer")
            };
            match fields.as_slice() {
                [kind @ ("root" | "hook"), name, _] => {
                    let prefix = integer(2)?;
                    if !(5..=32).contains(&prefix) {
                        return Err("bad prologue size".into());
                    }
                    result.hooks.insert((*name).into(), prefix);
                    if *kind == "root" {
                        result.roots.push(((*name).into(), prefix));
                    }
                }
                [kind @ ("code" | "data"), name, _, hash] => {
                    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("bad SHA-256".into());
                    }
                    let hash = (0..32)
                        .map(|i| {
                            u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16)
                                .map_err(|_| "bad SHA-256")
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    result.nodes.insert(
                        (*name).into(),
                        Node {
                            code: *kind == "code",
                            len: integer(2)?,
                            hash,
                            links: Vec::new(),
                        },
                    );
                }
                ["slot", name] => {
                    result.nodes.insert(
                        (*name).into(),
                        Node {
                            code: false,
                            len: 0,
                            hash: Vec::new(),
                            links: Vec::new(),
                        },
                    );
                }
                ["link", name, _, kind @ ("abs" | "rel"), target, _] => links.push((
                    (*name).to_owned(),
                    Link {
                        offset: integer(2)?,
                        relative: *kind == "rel",
                        target: (*target).into(),
                        addend: integer(5)? as u32,
                    },
                )),
                _ => return Err("bad library signature directive".into()),
            }
        }
        for (name, link) in links {
            if !result.nodes.contains_key(&link.target) {
                return Err("unknown link target".into());
            }
            let node = result.nodes.get_mut(&name).ok_or("unknown link source")?;
            if link.offset.checked_add(4).is_none_or(|end| end > node.len) {
                return Err("link outside node".into());
            }
            node.links.push(link);
        }
        if result.roots.is_empty() || result.hooks.keys().any(|n| !result.nodes.contains_key(n)) {
            return Err("missing library roots".into());
        }
        Ok(result)
    }

    fn matches(node: &Node, bytes: &[u8]) -> bool {
        let mut normalized = bytes.to_vec();
        for link in &node.links {
            normalized[link.offset..link.offset + 4].fill(0);
        }
        Sha256::digest(normalized).as_slice() == node.hash
    }

    /// Scan executable mappings, then verify the complete code/data reference graph.
    /// No target address, image name or game identity is part of the descriptor.
    pub fn scan(
        &self,
        ranges: &[ExecutableRange],
        read: &mut impl FnMut(u32, usize) -> Option<Vec<u8>>,
    ) -> Option<Library> {
        let mut resolved = BTreeMap::new();
        for (root, _) in &self.roots {
            let node = &self.nodes[root];
            let mut found = None;
            for range in ranges {
                let bytes = read(range.start, range.len as usize)?;
                for (offset, prefix) in bytes.windows(3).enumerate() {
                    // Standard frame-pointer entry, decoded again before detouring.
                    if prefix != [0x55, 0x8b, 0xec] {
                        continue;
                    }
                    let Some(body) = bytes.get(offset..offset.checked_add(node.len)?) else {
                        continue;
                    };
                    if Self::matches(node, body) {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(range.start.checked_add(offset as u32)?);
                    }
                }
            }
            resolved.insert(root.clone(), found?);
        }
        let mut pending: Vec<_> = resolved.iter().map(|(n, &a)| (n.clone(), a)).collect();
        let mut visited = BTreeSet::new();
        let mut snapshots = Vec::new();
        while let Some((name, address)) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let node = &self.nodes[&name];
            if node.len == 0 {
                continue;
            }
            if node.code
                && !ranges.iter().any(|r| {
                    address >= r.start
                        && u64::from(address) + node.len as u64
                            <= u64::from(r.start) + u64::from(r.len)
                })
            {
                return None;
            }
            let bytes = read(address, node.len)?;
            if !Self::matches(node, &bytes) {
                return None;
            }
            for link in &node.links {
                let word = u32::from_le_bytes(bytes[link.offset..link.offset + 4].try_into().ok()?);
                let pointer = if link.relative {
                    address
                        .wrapping_add(link.offset as u32 + 4)
                        .wrapping_add(word)
                } else {
                    word
                };
                let target = pointer.checked_sub(link.addend)?;
                if resolved
                    .insert(link.target.clone(), target)
                    .is_some_and(|old| old != target)
                {
                    return None;
                }
                pending.push((link.target.clone(), target));
            }
            snapshots.push((address, bytes));
        }
        if visited.len() != self.nodes.len() {
            return None;
        }
        let mut hooks = BTreeMap::new();
        for (name, &prefix) in &self.hooks {
            let address = *resolved.get(name)?;
            let bytes = read(address, prefix)?;
            let mut decoder = iced_x86::Decoder::new(32, &bytes, iced_x86::DecoderOptions::NONE);
            while decoder.can_decode() {
                let i = decoder.decode();
                if i.is_invalid() || i.flow_control() != iced_x86::FlowControl::Next {
                    return None;
                }
            }
            if decoder.position() != prefix {
                return None;
            }
            hooks.insert(name.clone(), (address, prefix));
        }
        Some(Library {
            hooks,
            snapshots,
            symbols: resolved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(base: u32, helper: u32, table: u32) -> (Family, Vec<u8>) {
        let mut memory = vec![0u8; 1024];
        // Authored stack frame; call helper; load a table word; return.
        let code = [
            0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0xe8, 0, 0, 0, 0, 0xa1, 0, 0, 0, 0, 0xc9, 0xc3,
        ];
        memory[..code.len()].copy_from_slice(&code);
        memory[7..11].copy_from_slice(&helper.wrapping_sub(base + 11).to_le_bytes());
        memory[12..16].copy_from_slice(&table.to_le_bytes());
        let h = (helper - base) as usize;
        memory[h..h + 6].copy_from_slice(&[0xb8, 7, 0, 0, 0, 0xc3]);
        let t = (table - base) as usize;
        memory[t..t + 4].copy_from_slice(&42u32.to_le_bytes());
        let digest = |b: &[u8]| format!("{:x}", Sha256::digest(b));
        let text = format!(
            "root entry 6\ncode entry {} {}\nlink entry 7 rel helper 0\nlink entry 12 abs table 0\ncode helper 6 {}\ndata table 4 {}\n",
            code.len(),
            digest(&code),
            digest(&memory[h..h + 6]),
            digest(&memory[t..t + 4])
        );
        (Family::parse(&text).unwrap(), memory)
    }
    fn admit(family: &Family, memory: &[u8], base: u32) -> bool {
        family
            .scan(
                &[ExecutableRange {
                    start: base,
                    len: memory.len() as u32,
                }],
                &mut |a, n| {
                    let offset = a.checked_sub(base)? as usize;
                    memory
                        .get(offset..offset.checked_add(n)?)
                        .map(<[u8]>::to_vec)
                },
            )
            .is_some()
    }
    // spec: DEV-VIDEO-1
    #[test]
    fn relocation_and_relinking_preserve_only_the_reviewed_graph() {
        let (family, memory) = fixture(0x10000, 0x10100, 0x10200);
        assert!(admit(&family, &memory, 0x10000));
        let (_, mut moved) = fixture(0x50000, 0x50280, 0x50080);
        assert!(admit(&family, &moved, 0x50000));
        moved[0x80] ^= 1;
        assert!(!admit(&family, &moved, 0x50000));
        moved[0x80] ^= 1;
        moved[0x281] ^= 1;
        assert!(!admit(&family, &moved, 0x50000));
        moved[0x281] ^= 1;
        moved[7] ^= 1;
        assert!(!admit(&family, &moved, 0x50000));
    }
}
