//! PE images for host modules. Entries carry machine-owned transport tokens,
//! never host pointers, and use the registered convention's return instruction.
//! Wine-independent import calls capture an image's IAT slot at export entry,
//! then read that slot whenever the host implementation calls it. Entries pass
//! their relocated guest image base in volatile EDX for this initial capture.

use std::collections::HashMap;
use std::sync::Arc;

use adv_cpu::backend::{Backend, GuestExt, Reg};
use adv_machine::{Machine, outcome::Resume};
use adv_pe::{ExportTarget, ImportModule, PeError, scn, write::DllWriter};

use crate::{Conv, Env, Outcome};

const IMAGE_BASE: u32 = 0x1000_0000;

/// A public name and ordinal for a registered host function.
pub struct Export<'a> {
    pub name: &'a str,
    pub ordinal: u16,
}

/// A DLL built for the bindings of one machine. Rebuild for another machine.
pub struct ModuleImage {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Entries(HashMap<u32, Arc<Vec<ImportModule>>>);

impl ModuleImage {
    /// Publish the named host functions and write their PE entries. The input
    /// import modules declare names/ordinals; their `iat_rva` fields are ignored.
    pub fn build<B: Backend>(
        machine: &mut Machine<B>,
        module: &str,
        exports: &[Export<'_>],
        imports: &[ImportModule],
    ) -> Result<Self, String> {
        let name = module.to_ascii_lowercase();
        if !name.ends_with(".dll") || name.contains(['/', '\\', ':']) {
            return Err("a host module needs a DLL file name".into());
        }
        let mut writer = DllWriter::new(&name, IMAGE_BASE);
        let code_rva = writer.next_rva();
        let mut code = Vec::new();
        let mut pe_exports = Vec::new();
        let mut entries = Vec::new();
        let mut relocations = Vec::new();
        let mut rvas = HashMap::new();
        for export in exports {
            if machine
                .sys
                .thunks
                .resolver()
                .host_fn(&name, export.name)
                .is_none()
            {
                return Err(format!(
                    "host function not registered: {name}!{}",
                    export.name
                ));
            }
            machine
                .sys
                .thunks
                .publish(&mut machine.cpu, &name, export.name)
                .map_err(|e| e.to_string())?;
            let canonical = machine
                .sys
                .thunks
                .resolver()
                .canonical_name(&name, export.name);
            let index = machine
                .sys
                .thunks
                .published_index(&name, canonical.as_deref().unwrap_or(export.name))
                .unwrap();
            let rva = if let Some(&rva) = rvas.get(&index) {
                rva
            } else {
                let thunk = machine.sys.thunks.get(index);
                let rva = code_rva + u32::try_from(code.len()).map_err(|e| e.to_string())?;
                // Relocatable hotpatch prologue, then the same transport as the
                // machine's ordinary thunks. This entry enters the host body.
                code.extend_from_slice(&[0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d]);
                // EDX is volatile under every registered convention. A relocated
                // guest image base survives copying this entry into a detour.
                code.push(0xba); // mov edx, image_base
                relocations.push(code_rva + code.len() as u32);
                code.extend_from_slice(&IMAGE_BASE.to_le_bytes());
                code.push(0x68);
                code.extend_from_slice(&thunk.transport_token.unwrap().word().to_le_bytes());
                code.extend_from_slice(&[0xcd, adv_cpu::backend::asm::TRAP_VECTOR]);
                entries.push(index);
                match thunk.f.conv {
                    Conv::Cdecl { .. } | Conv::CdeclVariadic { .. } => code.push(0xc3),
                    Conv::Stdcall { argc } => {
                        code.push(0xc2);
                        code.extend_from_slice(&(u16::from(argc) * 4).to_le_bytes());
                    }
                }
                rvas.insert(index, rva);
                rva
            };
            pe_exports.push(adv_pe::Export {
                name: Some(export.name.into()),
                ordinal: export.ordinal,
                target: ExportTarget::Rva(rva),
            });
        }
        let result = (|| -> Result<_, PeError> {
            let len = code.len() as u32;
            writer.section(
                ".text",
                code,
                len,
                scn::CNT_CODE | scn::MEM_READ | scn::MEM_EXECUTE,
            )?;
            writer.exports(&pe_exports)?;
            let imports = writer.imports(imports)?;
            writer.relocations(&relocations)?;
            Ok((writer.finish()?, Arc::new(imports)))
        })()
        .map_err(|e| e.to_string())?;
        for index in entries {
            machine
                .ext
                .get_or_default::<Entries>()
                .0
                .insert(index, result.1.clone());
        }
        Ok(Self {
            name,
            bytes: result.0,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A guest IAT slot, captured at host export entry and reusable in continuations.
#[derive(Clone, Copy)]
pub struct Import {
    slot: u32,
}

impl Import {
    /// Capture before returning an Outcome or entering guest code. `name` is
    /// case-sensitive, or `#ordinal`; `module` is a DLL file name.
    pub fn bind(env: &Env<'_>, module: &str, name: &str) -> Option<Self> {
        let imports = env.ext.try_get::<Entries>()?.0.get(&env.thunk)?;
        let import = imports
            .iter()
            .find(|m| m.dll.eq_ignore_ascii_case(module))?
            .imports
            .iter()
            .find(|i| match &i.name {
                adv_pe::ImportName::Name { name: imported, .. } => imported == name,
                adv_pe::ImportName::Ordinal(ordinal) => name == format!("#{ordinal}"),
            })?;
        let base = env.cpu.reg(Reg::Edx);
        Some(Self {
            slot: base.wrapping_add(import.iat_rva),
        })
    }

    /// Read the live slot and enter its target as an ordinary guest call.
    /// Capture this handle in `then` to call the same import again later.
    pub fn call(
        self,
        env: &mut Env<'_>,
        args: impl Into<Vec<u32>>,
        then: impl FnOnce(&mut Env<'_>, Resume) -> Outcome + 'static,
    ) -> Outcome {
        match env.cpu.read_u32(self.slot) {
            Ok(target) => Outcome::call(target, args, then),
            Err(error) => Outcome::Fail(format!("DLL import: {error}")),
        }
    }
}
