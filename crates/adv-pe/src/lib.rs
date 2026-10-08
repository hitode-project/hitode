//! PE32 image parsing: headers, sections, base relocations, imports, delay imports, TLS,
//! exports, and the resource tree.
//!
//! [`Pe::parse`] builds the image as the Windows loader would lay it out in memory
//! (headers + section raw data at their RVAs, zero-filled to `SizeOfImage`), and all
//! directory parsing reads from that mapped buffer by RVA. The loader then relocates and
//! binds the same buffer and copies it into guest memory in one write.

mod resource;

use thiserror::Error;

pub use resource::{ResId, ResourceData, ResourceDir, ResourceEntry, ResourceLeaf, ResourceNode};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PeError {
    #[error("not a PE image")]
    NotPe,
    #[error("not an x86 PE32 image (machine {0:#x})")]
    NotI386(u16),
    #[error("truncated or out-of-image read at rva {rva:#x} (+{len:#x})")]
    OutOfImage { rva: u32, len: u32 },
    #[error("malformed: {0}")]
    Malformed(String),
}

pub type Result<T> = std::result::Result<T, PeError>;

/// Data directory indices.
pub mod dir {
    pub const EXPORT: usize = 0;
    pub const IMPORT: usize = 1;
    pub const RESOURCE: usize = 2;
    pub const EXCEPTION: usize = 3;
    pub const SECURITY: usize = 4;
    pub const BASERELOC: usize = 5;
    pub const DEBUG: usize = 6;
    pub const TLS: usize = 9;
    pub const LOAD_CONFIG: usize = 10;
    pub const BOUND_IMPORT: usize = 11;
    pub const IAT: usize = 12;
    pub const DELAY_IMPORT: usize = 13;
}

/// `IMAGE_FILE_*` characteristics.
pub mod file_flags {
    pub const RELOCS_STRIPPED: u16 = 0x0001;
    pub const DLL: u16 = 0x2000;
}

/// `IMAGE_SCN_*` section characteristics.
pub mod scn {
    pub const CNT_CODE: u32 = 0x20;
    pub const MEM_EXECUTE: u32 = 0x2000_0000;
    pub const MEM_READ: u32 = 0x4000_0000;
    pub const MEM_WRITE: u32 = 0x8000_0000;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DataDir {
    pub rva: u32,
    pub size: u32,
}

impl DataDir {
    pub fn is_present(self) -> bool {
        self.rva != 0 && self.size != 0
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    /// Size in memory (virtual size, or raw size for linkers that leave it 0).
    pub fn mem_size(&self) -> u32 {
        if self.virtual_size != 0 {
            self.virtual_size
        } else {
            self.raw_size
        }
    }

    pub fn is_executable(&self) -> bool {
        self.characteristics & (scn::MEM_EXECUTE | scn::CNT_CODE) != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImportName {
    Name { hint: u16, name: String },
    Ordinal(u16),
}

impl std::fmt::Display for ImportName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportName::Name { name, .. } => f.write_str(name),
            ImportName::Ordinal(o) => write!(f, "#{o}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Import {
    pub name: ImportName,
    /// RVA of the IAT slot the loader fills with the function's address.
    pub iat_rva: u32,
}

#[derive(Clone, Debug)]
pub struct ImportModule {
    /// Module name exactly as stored (may contain a relative path).
    pub dll: String,
    pub imports: Vec<Import>,
}

#[derive(Clone, Debug)]
pub struct DelayImportModule {
    pub dll: String,
    /// RVA of the HMODULE slot the delay helper would fill.
    pub module_handle_rva: u32,
    pub imports: Vec<Import>,
}

#[derive(Clone, Debug)]
pub struct Tls {
    /// Template range as VAs (relative to the preferred base; rebase before use).
    pub raw_start_va: u32,
    pub raw_end_va: u32,
    pub index_va: u32,
    pub callbacks_va: u32,
    pub zero_fill: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportTarget {
    Rva(u32),
    /// `"OTHERDLL.Func"` or `"OTHERDLL.#12"`.
    Forwarder(String),
}

#[derive(Clone, Debug)]
pub struct Export {
    pub ordinal: u16,
    pub name: Option<String>,
    pub target: ExportTarget,
}

#[derive(Clone, Debug)]
pub struct Pe {
    pub machine: u16,
    pub characteristics: u16,
    pub timestamp: u32,
    pub image_base: u32,
    pub entry_rva: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub stack_reserve: u32,
    pub stack_commit: u32,
    pub heap_reserve: u32,
    pub heap_commit: u32,
    /// Offset (and RVA) of the `PE\0\0` signature.
    pub nt_headers: u32,
    pub dirs: [DataDir; 16],
    pub sections: Vec<Section>,
    /// The mapped image, `size_of_image` bytes.
    pub image: Vec<u8>,
}

fn le16(b: &[u8], off: usize) -> Result<u16> {
    off.checked_add(2)
        .and_then(|end| b.get(off..end))
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(PeError::OutOfImage {
            rva: off as u32,
            len: 2,
        })
}

fn le32(b: &[u8], off: usize) -> Result<u32> {
    off.checked_add(4)
        .and_then(|end| b.get(off..end))
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(PeError::OutOfImage {
            rva: off as u32,
            len: 4,
        })
}

fn add(rva: u32, offset: u32) -> Result<u32> {
    rva.checked_add(offset)
        .ok_or(PeError::OutOfImage { rva, len: offset })
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<()> {
    values
        .try_reserve(additional)
        .map_err(|_| PeError::Malformed("PE allocation failed".into()))
}

impl Pe {
    /// Parse headers and lay out the full image.
    pub fn parse(file: &[u8]) -> Result<Pe> {
        let mut pe = Self::parse_headers(file)?;
        pe.map_image(file)?;
        Ok(pe)
    }

    /// Parse headers and the section table only; `image` stays empty, so the directory
    /// accessors are unavailable. Cheap even for PAL.dll's 60+ MB SizeOfImage.
    pub fn parse_headers(file: &[u8]) -> Result<Pe> {
        if file.get(0..2) != Some(b"MZ") {
            return Err(PeError::NotPe);
        }
        let nt = le32(file, 0x3c).map_err(|_| PeError::NotPe)? as usize;
        let nt_end = nt.checked_add(24).ok_or(PeError::NotPe)?;
        let header = file.get(nt..nt_end).ok_or(PeError::NotPe)?;
        if &header[..4] != b"PE\0\0" {
            return Err(PeError::NotPe);
        }
        let machine = le16(header, 4)?;
        if machine != 0x14c {
            return Err(PeError::NotI386(machine));
        }
        let nsections = le16(header, 6)? as usize;
        let timestamp = le32(header, 8)?;
        let opt_size = le16(header, 20)? as usize;
        let characteristics = le16(header, 22)?;
        let opt_end = nt_end
            .checked_add(opt_size)
            .ok_or_else(|| PeError::Malformed("optional header range overflows".into()))?;
        let oh = file
            .get(nt_end..)
            .filter(|header| header.len() >= 96)
            .ok_or_else(|| PeError::Malformed("optional header truncated".into()))?;
        if le16(oh, 0)? != 0x10b {
            return Err(PeError::Malformed("optional header is not PE32".into()));
        }
        // Windows clamps the advertised count to complete entries in the header.
        let available_dirs = opt_size.saturating_sub(96).min(oh.len() - 96) / 8;
        let ndirs = (le32(oh, 92)? as usize).min(available_dirs).min(16);
        let mut dirs = [DataDir::default(); 16];
        for (i, d) in dirs.iter_mut().enumerate().take(ndirs) {
            *d = DataDir {
                rva: le32(oh, 96 + 8 * i)?,
                size: le32(oh, 100 + 8 * i)?,
            };
        }
        let mut pe = Pe {
            machine,
            characteristics,
            timestamp,
            entry_rva: le32(oh, 16)?,
            image_base: le32(oh, 28)?,
            section_alignment: le32(oh, 32)?,
            file_alignment: le32(oh, 36)?,
            size_of_image: le32(oh, 56)?,
            size_of_headers: le32(oh, 60)?,
            subsystem: le16(oh, 68)?,
            dll_characteristics: le16(oh, 70)?,
            stack_reserve: le32(oh, 72)?,
            stack_commit: le32(oh, 76)?,
            heap_reserve: le32(oh, 80)?,
            heap_commit: le32(oh, 84)?,
            nt_headers: nt as u32,
            dirs,
            sections: Vec::new(),
            image: Vec::new(),
        };
        if pe.size_of_image == 0 || pe.size_of_image > 0x4000_0000 {
            return Err(PeError::Malformed(format!(
                "SizeOfImage {:#x}",
                pe.size_of_image
            )));
        }

        // Unused directories and advisory sizes do not prevent image mapping.
        // Each consumer checks the actual records it dereferences instead.
        let sections_end = nsections
            .checked_mul(40)
            .and_then(|size| opt_end.checked_add(size))
            .ok_or_else(|| PeError::Malformed("section table range overflows".into()))?;
        let sections = if nsections == 0 {
            &[][..]
        } else {
            file.get(opt_end..sections_end)
                .ok_or_else(|| PeError::Malformed("section table truncated".into()))?
        };
        reserve(&mut pe.sections, nsections)?;
        for s in sections.chunks_exact(40) {
            let raw_name = &s[..8];
            let name_len = raw_name.iter().position(|&c| c == 0).unwrap_or(8);
            pe.sections.push(Section {
                name: String::from_utf8_lossy(&raw_name[..name_len]).into_owned(),
                virtual_size: le32(s, 8)?,
                virtual_address: le32(s, 12)?,
                raw_size: le32(s, 16)?,
                raw_offset: le32(s, 20)?,
                characteristics: le32(s, 36)?,
            });
        }

        Ok(pe)
    }

    fn mapped_size(&self) -> Result<u32> {
        // SEC_IMAGE rounds the reserved extent to a page even when a linker
        // leaves SizeOfImage unaligned. Preserve the header value, but include
        // the final page's raw padding in the mapped buffer and bounds checks.
        if self.section_alignment >= 0x1000 {
            self.size_of_image
                .checked_add(0xfff)
                .map(|size| size & !0xfff)
                .ok_or_else(|| PeError::Malformed("SizeOfImage rounding overflows".into()))
        } else {
            Ok(self.size_of_image)
        }
    }

    fn map_image(&mut self, file: &[u8]) -> Result<()> {
        let pe = self;
        let mapped_size = pe.mapped_size()?;
        let section_size = |s: &Section| {
            // SEC_IMAGE preserves raw padding inside the section's final page.
            // Extra raw pages do not enlarge a nonzero VirtualSize or overwrite
            // the next section (some packed images have oversized raw data).
            if pe.section_alignment >= 0x1000 {
                s.mem_size()
                    .checked_add(0xfff)
                    .map(|size| size & !0xfff)
                    .ok_or_else(|| {
                        PeError::Malformed(format!("section {} mapped size overflows", s.name))
                    })
            } else {
                Ok(s.mem_size())
            }
        };
        // Reject invalid virtual extents before requesting the image buffer.
        for s in &pe.sections {
            let mapped = section_size(s)?;
            if s.virtual_address
                .checked_add(mapped)
                .is_none_or(|end| end > mapped_size)
            {
                return Err(PeError::Malformed(format!(
                    "section {} exceeds SizeOfImage",
                    s.name
                )));
            }
        }
        let mut image = Vec::new();
        reserve(&mut image, mapped_size as usize)?;
        image.resize(mapped_size as usize, 0);
        let hdr = (pe.size_of_headers as usize)
            .min(file.len())
            .min(image.len());
        image[..hdr].copy_from_slice(&file[..hdr]);
        for s in &pe.sections {
            let mapped = section_size(s)?;
            // Windows rounds the raw pointer down to 512 when FileAlignment is at least that.
            let off = if pe.file_alignment >= 0x200 {
                s.raw_offset & !0x1ff
            } else {
                s.raw_offset
            } as usize;
            let len = (s.raw_size.min(mapped) as usize).min(file.len().saturating_sub(off));
            let va = s.virtual_address as usize;
            if va + len > image.len() {
                return Err(PeError::Malformed(format!(
                    "section {} exceeds SizeOfImage",
                    s.name
                )));
            }
            if len != 0 {
                image[va..va + len].copy_from_slice(&file[off..off + len]);
            }
        }
        pe.image = image;
        Ok(())
    }

    pub fn is_dll(&self) -> bool {
        self.characteristics & file_flags::DLL != 0
    }

    pub fn dir(&self, index: usize) -> DataDir {
        self.dirs[index]
    }

    pub fn slice(&self, rva: u32, len: u32) -> Result<&[u8]> {
        let (s, e) = (rva as usize, add(rva, len)? as usize);
        self.image.get(s..e).ok_or(PeError::OutOfImage { rva, len })
    }

    pub fn u16_at(&self, rva: u32) -> Result<u16> {
        le16(&self.image, rva as usize).map_err(|_| PeError::OutOfImage { rva, len: 2 })
    }

    pub fn u32_at(&self, rva: u32) -> Result<u32> {
        le32(&self.image, rva as usize).map_err(|_| PeError::OutOfImage { rva, len: 4 })
    }

    pub fn set_u32(&mut self, rva: u32, v: u32) -> Result<()> {
        let s = self
            .image
            .get_mut(rva as usize..add(rva, 4)? as usize)
            .ok_or(PeError::OutOfImage { rva, len: 4 })?;
        s.copy_from_slice(&v.to_le_bytes());
        Ok(())
    }

    /// NUL-terminated byte string at `rva`.
    pub fn cstr_at(&self, rva: u32) -> Result<&[u8]> {
        let tail = self
            .image
            .get(rva as usize..)
            .ok_or(PeError::OutOfImage { rva, len: 1 })?;
        let n = tail
            .iter()
            .position(|&c| c == 0)
            .ok_or(PeError::OutOfImage {
                rva,
                len: tail.len() as u32,
            })?;
        Ok(&tail[..n])
    }

    fn string_at(&self, rva: u32) -> Result<String> {
        // Import/export names are ASCII in practice; keep anything else lossless-ish.
        Ok(String::from_utf8_lossy(self.cstr_at(rva)?).into_owned())
    }

    /// Convert a VA stored in the image (relative to the preferred base) to an RVA.
    pub fn va_to_rva(&self, va: u32) -> Result<u32> {
        self.va_to_rva_at(va, self.image_base)
    }

    fn va_to_rva_at(&self, va: u32, base: u32) -> Result<u32> {
        va.checked_sub(base)
            .filter(|&r| r < self.size_of_image)
            .ok_or_else(|| PeError::Malformed(format!("VA {va:#x} outside image")))
    }

    /// RVAs of all 32-bit (HIGHLOW) fixups. Absolute (padding) entries are skipped.
    pub fn relocations(&self) -> Result<Vec<u32>> {
        let d = self.dir(dir::BASERELOC);
        let mut out = Vec::new();
        if !d.is_present() {
            return Ok(out);
        }
        let mut blocks = self.slice(d.rva, d.size)?;
        while blocks.len() >= 8 {
            let page = le32(blocks, 0)?;
            let size = le32(blocks, 4)?;
            if size == 0 {
                break;
            }
            if size < 8 {
                return Err(PeError::Malformed("relocation block is too short".into()));
            }
            if size as usize > blocks.len() {
                return Err(PeError::Malformed("relocation block truncated".into()));
            }
            let (block, remaining) = blocks.split_at(size as usize);
            for entry in block[8..].chunks_exact(2) {
                let v = le16(entry, 0)?;
                match v >> 12 {
                    0 => {}
                    3 => {
                        let target = add(page, u32::from(v & 0xfff))?;
                        self.slice(target, 4)?;
                        reserve(&mut out, 1)?;
                        out.push(target);
                    }
                    t => {
                        return Err(PeError::Malformed(format!(
                            "relocation type {t} at page {page:#x}"
                        )));
                    }
                }
            }
            blocks = remaining;
        }
        Ok(out)
    }

    /// Apply `delta` to every fixup (for loading at `image_base + delta`).
    pub fn relocate(&mut self, delta: u32) -> Result<usize> {
        let relocs = self.relocations()?;
        for &r in &relocs {
            let v = self.u32_at(r)?;
            self.set_u32(r, v.wrapping_add(delta))?;
        }
        Ok(relocs.len())
    }

    /// Rebase the mapped image and publish its actual OptionalHeader.ImageBase.
    /// A missing relocation directory leaves embedded VAs unchanged, but does
    /// not suppress the mapped header update.
    pub fn rebase(&mut self, base: u32) -> Result<usize> {
        let field = self.image_base_field()?;
        self.slice(field, 4)?;
        let n = self.relocate(base.wrapping_sub(self.image_base))?;
        self.set_u32(field, base)?;
        Ok(n)
    }

    fn image_base_field(&self) -> Result<u32> {
        // Signature, file header, then ImageBase at optional-header offset 28.
        add(self.nt_headers, 4 + 20 + 28)
    }

    /// Walk a thunk array (names at `names_rva`, slots at `iat_rva`).
    fn thunks(
        &self,
        names_rva: u32,
        iat_rva: u32,
        name_va_base: Option<u32>,
    ) -> Result<Vec<Import>> {
        let mut out = Vec::new();
        let tail = self
            .image
            .get(names_rva as usize..)
            .ok_or(PeError::OutOfImage {
                rva: names_rva,
                len: 4,
            })?;
        let count = tail
            .chunks_exact(4)
            .position(|word| word == [0; 4])
            .ok_or_else(|| PeError::Malformed("import thunk array lacks a terminator".into()))?;
        let len = u32::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| PeError::Malformed("import thunk array size overflows".into()))?;
        let names = self.slice(names_rva, len)?;
        if len != 0 {
            self.slice(iat_rva, len)?;
        }
        reserve(&mut out, count)?;
        let mut iat_rva = iat_rva;
        for word in names.chunks_exact(4) {
            let t = le32(word, 0)?;
            let name = if t & 0x8000_0000 != 0 {
                ImportName::Ordinal(t as u16)
            } else {
                let rva = match name_va_base {
                    Some(base) => self.va_to_rva_at(t, base)?,
                    None => t,
                };
                ImportName::Name {
                    hint: self.u16_at(rva)?,
                    name: self.string_at(add(rva, 2)?)?,
                }
            };
            out.push(Import { name, iat_rva });
            iat_rva = add(iat_rva, 4)?;
        }
        Ok(out)
    }

    pub fn imports(&self) -> Result<Vec<ImportModule>> {
        let d = self.dir(dir::IMPORT);
        let mut out = Vec::new();
        if !d.is_present() {
            return Ok(out);
        }
        // The loader follows the terminator, even when Size excludes it.
        let descriptors = self
            .image
            .get(d.rva as usize..)
            .ok_or(PeError::OutOfImage {
                rva: d.rva,
                len: 20,
            })?;
        for desc in descriptors.chunks_exact(20) {
            let (int, name, iat) = (le32(desc, 0)?, le32(desc, 12)?, le32(desc, 16)?);
            if name == 0 && iat == 0 {
                return Ok(out);
            }
            // Old linkers leave OriginalFirstThunk 0; the IAT then holds the names.
            let names = if int != 0 { int } else { iat };
            reserve(&mut out, 1)?;
            out.push(ImportModule {
                dll: self.string_at(name)?,
                imports: self.thunks(names, iat, None)?,
            });
        }
        Err(PeError::Malformed(
            "import directory lacks a terminator".into(),
        ))
    }

    /// The import directory an image had before Detours' `setdll` added DLLs to it, read from
    /// the `DETOUR_SECTION_HEADER` at the start of the `.detour` section (signature `"Dtr\0"`,
    /// original import table at +0x10). `None` when the image wasn't rewritten by Detours.
    pub fn detours_original_imports(&self) -> Result<Option<DataDir>> {
        const SIGNATURE: u32 = u32::from_le_bytes(*b"Dtr\0");
        let Some(s) = self.sections.iter().find(|s| s.name == ".detour") else {
            return Ok(None);
        };
        if self.u32_at(add(s.virtual_address, 4)?)? != SIGNATURE {
            return Ok(None);
        }
        let header = self.slice(s.virtual_address, 0x18)?;
        let d = DataDir {
            rva: le32(header, 0x10)?,
            size: le32(header, 0x14)?,
        };
        Ok(Some(d))
    }

    pub fn delay_imports(&self) -> Result<Vec<DelayImportModule>> {
        self.delay_imports_at(self.image_base)
    }

    /// Delay imports from an image whose absolute pointers have already been
    /// relocated to `base`. RVA-based descriptors are independent of this base.
    pub fn delay_imports_at(&self, base: u32) -> Result<Vec<DelayImportModule>> {
        let d = self.dir(dir::DELAY_IMPORT);
        let mut out = Vec::new();
        if !d.is_present() {
            return Ok(out);
        }
        let descriptors = self
            .image
            .get(d.rva as usize..)
            .ok_or(PeError::OutOfImage {
                rva: d.rva,
                len: 32,
            })?;
        for desc in descriptors.chunks_exact(32) {
            let attrs = le32(desc, 0)?;
            let name = le32(desc, 4)?;
            if name == 0 {
                return Ok(out);
            }
            if attrs & !1 != 0 {
                return Err(PeError::Malformed(format!(
                    "delay import attributes {attrs:#x}"
                )));
            }
            // grAttrs bit 0 set: fields are RVAs (VC7+); clear: VAs (VC6 delay loads).
            let fix = |v: u32| {
                if attrs & 1 != 0 || v == 0 {
                    Ok(v)
                } else {
                    self.va_to_rva_at(v, base)
                }
            };
            let (hmod, iat, int) = (
                fix(le32(desc, 8)?)?,
                fix(le32(desc, 12)?)?,
                fix(le32(desc, 16)?)?,
            );
            // Translate old-format name pointers before dereferencing the hint/name.
            // Ordinal thunks still use their high-bit encoding in either format.
            let imports = self.thunks(int, iat, (attrs & 1 == 0).then_some(base))?;
            reserve(&mut out, 1)?;
            out.push(DelayImportModule {
                dll: self.string_at(fix(name)?)?,
                module_handle_rva: hmod,
                imports,
            });
        }
        Err(PeError::Malformed(
            "delay import directory lacks a terminator".into(),
        ))
    }

    pub fn tls(&self) -> Result<Option<Tls>> {
        let d = self.dir(dir::TLS);
        if !d.is_present() {
            return Ok(None);
        }
        let header = self.slice(d.rva, 20)?;
        Ok(Some(Tls {
            raw_start_va: le32(header, 0)?,
            raw_end_va: le32(header, 4)?,
            index_va: le32(header, 8)?,
            callbacks_va: le32(header, 12)?,
            zero_fill: le32(header, 16)?,
        }))
    }

    /// TLS callback VAs, read from the (possibly relocated) image at `callbacks_va`.
    /// `base` is where the image is loaded; VAs in the image are already rebased to it.
    pub fn tls_callbacks(&self, tls: &Tls, base: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        if tls.callbacks_va == 0 {
            return Ok(out);
        }
        // VA rebasing wraps as before; reads are bounded by the mapped image,
        // which can include padding beyond the logical SizeOfImage.
        let mut rva = tls.callbacks_va.wrapping_sub(base);
        loop {
            let cb = self.u32_at(rva)?;
            if cb == 0 {
                break;
            }
            reserve(&mut out, 1)?;
            out.push(cb);
            rva = add(rva, 4)?;
        }
        Ok(out)
    }

    pub fn exports(&self) -> Result<Vec<Export>> {
        let d = self.dir(dir::EXPORT);
        let mut out = Vec::new();
        if !d.is_present() {
            return Ok(out);
        }
        // Export metadata is consulted lazily by Windows; unusable entries are
        // absent lookups rather than a reason to reject the entire image.
        let Ok(header) = self.slice(d.rva, 40) else {
            return Ok(out);
        };
        let ordinal_base = le32(header, 16)?;
        let nfuncs = le32(header, 20)?;
        let nnames = le32(header, 24)?;
        let (funcs, names, ords) = (le32(header, 28)?, le32(header, 32)?, le32(header, 36)?);
        let array = |rva, count: u32, width| {
            let len = count
                .checked_mul(width)
                .ok_or_else(|| PeError::Malformed("export array size overflows".into()))?;
            self.slice(rva, len)
        };
        // Validate the EAT before allocation, then keep only ordinals exposed
        // by the existing u16 interface. Never truncate them into aliases.
        let Ok(funcs) = array(funcs, nfuncs, 4) else {
            return Ok(out);
        };
        let count = (u32::from(u16::MAX) + 1)
            .saturating_sub(ordinal_base)
            .min(nfuncs) as usize;
        let mut name_of = Vec::new();
        reserve(&mut name_of, count)?;
        name_of.resize_with(count, || None);
        let ords = self.image.get(ords as usize..).unwrap_or_default();
        for (i, ord) in ords.chunks_exact(2).take(nnames as usize).enumerate() {
            let idx = le16(ord, 0)? as usize;
            let Some(slot) = name_of.get_mut(idx) else {
                continue;
            };
            let Some(name_rva) = (i as u32)
                .checked_mul(4)
                .and_then(|offset| names.checked_add(offset))
            else {
                continue;
            };
            if let Ok(name) = self.u32_at(name_rva).and_then(|rva| self.string_at(rva)) {
                *slot = Some(name);
            }
        }
        reserve(&mut out, count)?;
        for (i, (name, func)) in name_of.into_iter().zip(funcs.chunks_exact(4)).enumerate() {
            let rva = le32(func, 0)?;
            if rva == 0 {
                continue;
            }
            let target = if rva.checked_sub(d.rva).is_some_and(|offset| offset < d.size) {
                let Ok(name) = self.string_at(rva) else {
                    continue;
                };
                ExportTarget::Forwarder(name)
            } else {
                ExportTarget::Rva(rva)
            };
            let ordinal = u16::try_from(add(ordinal_base, i as u32)?)
                .map_err(|_| PeError::Malformed("export ordinal exceeds u16".into()))?;
            out.push(Export {
                ordinal,
                name,
                target,
            });
        }
        Ok(out)
    }

    /// The resource tree (type → name → language).
    pub fn resources(&self) -> Result<Option<ResourceDir>> {
        let d = self.dir(dir::RESOURCE);
        if !d.is_present() {
            return Ok(None);
        }
        resource::parse(self, d).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_capacity_overflow_is_a_pe_error() {
        let mut bytes = Vec::<u8>::new();
        assert!(matches!(
            reserve(&mut bytes, usize::MAX),
            Err(PeError::Malformed(_))
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn little_endian_reads_reject_overflowing_host_offsets() {
        assert!(le16(&[], usize::MAX).is_err());
        assert!(le32(&[], usize::MAX).is_err());
    }
}
