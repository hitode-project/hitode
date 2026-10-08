//! Optional image-compilation hints. Only mapped native images reach this path;
//! synthetic system modules retain their ordinary host dispatch policy.
use std::collections::BTreeSet;

use adv_cpu::AotImage;
use adv_cpu::backend::Guest;
use adv_pe::{ExportTarget, Pe, scn};

const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEEDS: usize = 2048;

pub(super) fn register(cpu: &mut dyn Guest, pe: &mut Pe, base: u32, path: &str) {
    let aot = std::env::var("ADV_AOT").is_ok_and(|value| value == "1");
    let cache = std::env::var("ADV_CODE_CACHE").is_ok_and(|value| value == "1");
    if !(aot || cache) {
        return;
    }
    let mut image = AotImage {
        base,
        size: pe.size_of_image,
        primary: !pe.is_dll(),
        image: Vec::new(),
        executable: Vec::new(),
        seeds: Vec::new(),
    };
    // Large or system-path images still supply persistent-cache module identity.
    if aot && !super::is_system_path(path) && pe.image.len() <= MAX_IMAGE_BYTES {
        image.executable = executable(pe);
        image.seeds = seeds(pe, base, &image.executable);
        // The loader is finished with this mapped buffer; transfer ownership
        // instead of copying an image on the guest's load path.
        image.image = std::mem::take(&mut pe.image);
    }
    cpu.register_code_image(image);
}

fn executable(pe: &Pe) -> Vec<(u32, u32)> {
    pe.sections
        .iter()
        .filter(|section| section.characteristics & scn::MEM_EXECUTE != 0)
        .filter_map(|section| {
            let start = section.virtual_address;
            let end = start.checked_add(section.mem_size())?.min(pe.size_of_image);
            (start < end).then_some((start, end))
        })
        .collect()
}

fn seeds(pe: &Pe, base: u32, ranges: &[(u32, u32)]) -> Vec<u32> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut add = |rva| {
        if out.len() < MAX_SEEDS
            && ranges.iter().any(|&(start, end)| rva >= start && rva < end)
            && seen.insert(rva)
        {
            out.push(rva);
        }
    };
    if pe.entry_rva != 0 {
        add(pe.entry_rva);
    }
    if let Ok(Some(tls)) = pe.tls() {
        for callback in pe.tls_callbacks(&tls, base).unwrap_or_default() {
            if let Some(rva) = callback.checked_sub(base) {
                add(rva);
            }
        }
    }
    for export in pe.exports().unwrap_or_default() {
        if let ExportTarget::Rva(rva) = export.target {
            add(rva);
        }
    }
    for location in pe.relocations().unwrap_or_default() {
        if let Ok(target) = pe.u32_at(location)
            && let Some(rva) = target.checked_sub(base)
        {
            add(rva);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_pe::{DataDir, Section, dir};

    fn image() -> Pe {
        let mut pe = Pe {
            machine: 0x14c,
            characteristics: 0,
            timestamp: 0,
            image_base: 0,
            entry_rva: 16,
            size_of_image: 128,
            size_of_headers: 0,
            section_alignment: 1,
            file_alignment: 1,
            subsystem: 3,
            dll_characteristics: 0,
            stack_reserve: 0,
            stack_commit: 0,
            heap_reserve: 0,
            heap_commit: 0,
            dirs: [DataDir::default(); 16],
            sections: vec![Section {
                name: ".text".into(),
                virtual_address: 16,
                virtual_size: 16,
                raw_offset: 0,
                raw_size: 16,
                characteristics: scn::MEM_EXECUTE,
            }],
            image: vec![0; 128],
        };
        // One relocated pointer into executable code and one into data.
        pe.dirs[dir::BASERELOC] = DataDir { rva: 64, size: 12 };
        pe.set_u32(64, 0).unwrap();
        pe.set_u32(68, 12).unwrap();
        pe.image[72..76].copy_from_slice(&[40, 0x30, 44, 0x30]);
        pe.set_u32(40, 24).unwrap();
        pe.set_u32(44, 48).unwrap();
        pe
    }

    #[test]
    fn entry_and_relocated_code_pointers_are_seeds() {
        let pe = image();
        assert_eq!(seeds(&pe, 0, &executable(&pe)), vec![16, 24]);
    }

    #[test]
    fn relocated_values_use_the_actual_module_base() {
        let mut pe = image();
        let base = 4096;
        pe.relocate(base).unwrap();
        assert_eq!(seeds(&pe, base, &executable(&pe)), vec![16, 24]);
    }

    #[test]
    fn malformed_optional_metadata_keeps_the_entry_seed() {
        let mut pe = image();
        pe.dirs[dir::BASERELOC] = DataDir {
            rva: pe.size_of_image,
            size: 16,
        };
        assert_eq!(seeds(&pe, 0, &executable(&pe)), vec![16]);
    }
}
