//! Reproducible PE32 system images. Layout follows Microsoft's PE/COFF format:
//! https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
//!
//! SDK ordinal aliases are authoritative. Other named exports receive local EAT
//! slots, not claims about the ordinal numbering of a particular Windows build.
use std::collections::{BTreeMap, BTreeSet};

use adv_pe::{PeError, scn};

#[derive(Clone)]
pub enum Target {
    Code(Vec<u8>),
    Forwarder(String),
}

#[derive(Clone)]
pub struct Export {
    pub name: Option<String>,
    pub ordinal: Option<u16>,
    pub target: Target,
}

pub struct Image {
    /// File alignment equals section alignment, so this is also the mapped image.
    pub bytes: Vec<u8>,
    pub text_rva: u32,
    pub text_size: u32,
}

fn word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn short(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn page(value: usize) -> usize {
    (value + 4095) & !4095
}
fn string(bytes: &mut Vec<u8>, text: &str) -> u32 {
    let at = bytes.len() as u32;
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    at
}

pub fn build(base: u32, module: &str, exports: &[Export]) -> Result<Image, PeError> {
    let malformed = |message: &str| PeError::Malformed(message.into());
    if module.as_bytes().contains(&0) || exports.len() > u16::MAX as usize {
        return Err(malformed("invalid synthetic module or export count"));
    }
    let mut slots = BTreeMap::<u16, &Export>::new();
    let mut names = BTreeSet::new();
    for export in exports {
        if let Some(name) = &export.name
            && (name.is_empty() || name.as_bytes().contains(&0) || !names.insert(name))
        {
            return Err(malformed("invalid or duplicate synthetic export name"));
        }
        if let Some(ordinal) = export.ordinal
            && (ordinal == 0 || slots.insert(ordinal, export).is_some())
        {
            return Err(malformed("invalid or duplicate synthetic export ordinal"));
        }
    }
    let mut next = 1u32;
    for export in exports.iter().filter(|export| export.ordinal.is_none()) {
        while next <= u16::MAX as u32 && slots.contains_key(&(next as u16)) {
            next += 1;
        }
        if next > u16::MAX as u32 {
            return Err(malformed("synthetic export ordinal space exhausted"));
        }
        slots.insert(next as u16, export);
        next += 1;
    }
    let ordinal_base = slots.keys().next().copied().unwrap_or(1);
    let count = slots
        .keys()
        .next_back()
        .map_or(0, |end| usize::from(end - ordinal_base) + 1);
    let named = slots
        .values()
        .filter(|export| export.name.is_some())
        .count();
    let mut bytes = vec![0u8; 4096 + 40 + count * 4 + named * 6];
    let functions = 4096 + 40;
    let name_table = functions + count * 4;
    let ordinal_table = name_table + named * 4;
    let module_name = string(&mut bytes, module);
    let mut named_entries: Vec<_> = slots
        .iter()
        .filter_map(|(&ordinal, export)| export.name.as_deref().map(|name| (name, ordinal)))
        .collect();
    named_entries.sort_by_key(|(name, _)| *name);
    for (i, (name, ordinal)) in named_entries.into_iter().enumerate() {
        let rva = string(&mut bytes, name);
        word(&mut bytes, name_table + i * 4, rva);
        short(&mut bytes, ordinal_table + i * 2, ordinal - ordinal_base);
    }
    for (&ordinal, export) in &slots {
        if let Target::Forwarder(target) = &export.target {
            if target.as_bytes().contains(&0) || !target.contains('.') {
                return Err(malformed("invalid synthetic forwarder"));
            }
            let rva = string(&mut bytes, target);
            word(
                &mut bytes,
                functions + usize::from(ordinal - ordinal_base) * 4,
                rva,
            );
        }
    }
    let export_size = bytes.len() - 4096;
    let text_rva = page(bytes.len());
    bytes.resize(text_rva, 0);
    // Equal entries (SDK ordinal/name aliases) share the same hookable address.
    let mut code = BTreeMap::<Vec<u8>, u32>::new();
    for (&ordinal, export) in &slots {
        if let Target::Code(body) = &export.target {
            if body.is_empty() || body.len() > 32 {
                return Err(malformed("invalid synthetic thunk length"));
            }
            let rva = *code.entry(body.clone()).or_insert_with(|| {
                // Five NOPs before the entry also support hotpatch-above detours.
                let start = bytes.len();
                bytes.resize(start + 48, 0x90);
                bytes[start + 8..start + 8 + body.len()].copy_from_slice(body);
                (start + 8) as u32
            });
            word(
                &mut bytes,
                functions + usize::from(ordinal - ordinal_base) * 4,
                rva,
            );
        }
    }
    let image_size = page(bytes.len().max(text_rva + 1));
    bytes.resize(image_size, 0);
    bytes[..2].copy_from_slice(b"MZ");
    word(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    short(&mut bytes, 0x84, 0x14c);
    short(&mut bytes, 0x86, 2);
    short(&mut bytes, 0x94, 224);
    short(&mut bytes, 0x96, 0x2103); // executable, x86, DLL, no base fixups
    let optional = 0x98;
    short(&mut bytes, optional, 0x10b);
    word(&mut bytes, optional + 4, (image_size - text_rva) as u32);
    word(&mut bytes, optional + 8, (text_rva - 4096) as u32);
    word(&mut bytes, optional + 20, text_rva as u32);
    word(&mut bytes, optional + 24, 4096);
    word(&mut bytes, optional + 28, base);
    word(&mut bytes, optional + 32, 4096);
    word(&mut bytes, optional + 36, 4096);
    short(&mut bytes, optional + 40, 6);
    short(&mut bytes, optional + 48, 6);
    word(&mut bytes, optional + 56, image_size as u32);
    word(&mut bytes, optional + 60, 4096);
    short(&mut bytes, optional + 68, 3);
    word(&mut bytes, optional + 72, 0x100000);
    word(&mut bytes, optional + 76, 4096);
    word(&mut bytes, optional + 80, 0x100000);
    word(&mut bytes, optional + 84, 4096);
    word(&mut bytes, optional + 92, 16);
    word(&mut bytes, optional + 96, 4096);
    word(&mut bytes, optional + 100, export_size as u32);
    for (i, (name, rva, size, flags)) in [
        (b".edata\0\0", 4096, text_rva - 4096, scn::MEM_READ | 0x40),
        (
            b".text\0\0\0",
            text_rva,
            image_size - text_rva,
            scn::MEM_READ | scn::MEM_EXECUTE | scn::CNT_CODE,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let section = optional + 224 + i * 40;
        bytes[section..section + 8].copy_from_slice(name);
        word(&mut bytes, section + 8, size as u32);
        word(&mut bytes, section + 12, rva as u32);
        word(&mut bytes, section + 16, size as u32);
        word(&mut bytes, section + 20, rva as u32);
        word(&mut bytes, section + 36, flags);
    }
    word(&mut bytes, 4096 + 12, module_name);
    word(&mut bytes, 4096 + 16, ordinal_base as u32);
    word(&mut bytes, 4096 + 20, count as u32);
    word(&mut bytes, 4096 + 24, named as u32);
    word(&mut bytes, 4096 + 28, functions as u32);
    word(&mut bytes, 4096 + 32, name_table as u32);
    word(&mut bytes, 4096 + 36, ordinal_table as u32);
    Ok(Image {
        bytes,
        text_rva: text_rva as u32,
        text_size: (image_size - text_rva) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_pe::{ExportTarget, Pe};

    #[test]
    fn real_pe_exports_sparse_ordinals_forwarders_and_shared_hook_entries() {
        let image = build(
            0x70000000,
            "fixture.dll",
            &[
                Export {
                    name: Some("Zulu".into()),
                    ordinal: Some(57),
                    target: Target::Code(vec![0x8b, 0xff, 0xc3]),
                },
                Export {
                    name: Some("Alpha".into()),
                    ordinal: None,
                    target: Target::Code(vec![0x8b, 0xff, 0xc3]),
                },
                Export {
                    name: None,
                    ordinal: Some(9),
                    target: Target::Forwarder("other.#12".into()),
                },
            ],
        )
        .unwrap();
        let pe = Pe::parse(&image.bytes).unwrap();
        assert_eq!(pe.image, image.bytes);
        assert_eq!(pe.image_base, 0x70000000);
        let exports = pe.exports().unwrap();
        let alpha = exports
            .iter()
            .find(|e| e.name.as_deref() == Some("Alpha"))
            .unwrap();
        let zulu = exports
            .iter()
            .find(|e| e.name.as_deref() == Some("Zulu"))
            .unwrap();
        assert_eq!(zulu.ordinal, 57);
        assert_eq!(alpha.target, zulu.target);
        assert_eq!(
            exports.iter().find(|e| e.ordinal == 9).unwrap().target,
            ExportTarget::Forwarder("other.#12".into())
        );
        assert!(pe.sections[1].is_executable());
        assert_eq!(exports.len(), 3);
    }
}
