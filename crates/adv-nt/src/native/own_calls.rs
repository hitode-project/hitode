//! A module's calls to its own hookable exports.
//!
//! Windows creates dialogs, their controls, message boxes and combo box
//! lists without entering user32's exported CreateWindowExA/W, so a game's
//! inline hook on those exports sees only the game's own calls (measured on
//! Windows 11). Wine's user32 calls the exports itself. Those calls branch
//! instead to a private entry that runs the export's first instructions and
//! continues after them, which a hook written over the entry never reaches.

use adv_cpu::backend::Guest;
use adv_machine::{Ext, Sys};
use iced_x86::{Decoder, DecoderOptions, FlowControl, Mnemonic, OpKind};

use crate::identity::{self, Table};

/// Each module's exports that its own calls bypass.
const EXPORTS: &[(&str, &[&str])] = &[("user32.dll", &["CreateWindowExA", "CreateWindowExW"])];

/// The bytes an inline hook overwrites: a near jump.
const HOOK: usize = 5;

pub(super) fn covers(module: &str) -> bool {
    EXPORTS.iter().any(|&(name, _)| name == module)
}

/// Points the calls of the image at `base` to its own exports at private
/// entries, before any guest code can hook them.
pub(super) fn bind(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    base: u32,
    module: &str,
    table: &Table,
    relocated: impl Fn(u32) -> bool,
) {
    let Some(&(_, names)) = EXPORTS.iter().find(|&&(name, _)| name == module) else {
        return;
    };
    // (export entry, private entry)
    let mut entries = Vec::new();
    for name in names {
        let Some(function) = table.named(name) else {
            continue;
        };
        let entry = base + function.rva;
        let mut code = vec![0; function.size as usize];
        if !identity::intact(cpu, entry, function, &relocated)
            || cpu.read(entry, &mut code).is_err()
        {
            log::debug!("{module}!{name}: bytes differ from the distribution");
            continue;
        }
        let Some(length) = movable(&code, entry) else {
            log::warn!("{module}!{name}: the entry cannot move");
            continue;
        };
        match super::trampoline(
            cpu,
            sys,
            ext,
            code[..length].to_vec(),
            entry + length as u32,
        ) {
            Ok(private) => entries.push((entry, private)),
            Err(error) => log::warn!("{module}!{name}: no private entry: {error}"),
        }
    }
    if entries.is_empty() {
        return;
    }
    let mut code = Vec::new();
    for function in &table.functions {
        let at = base + function.rva;
        code.resize(function.size as usize, 0);
        if cpu.read(at, &mut code).is_err() {
            continue;
        }
        let sites = calls(&code, at, &entries);
        if sites.is_empty() || !identity::intact(cpu, at, function, &relocated) {
            continue;
        }
        for (field, private) in sites {
            let next = field + 4;
            if let Err(error) = cpu.write(field, &private.wrapping_sub(next).to_le_bytes()) {
                log::warn!("{module}!{}: call not redirected: {error:?}", function.name);
            }
        }
    }
}

/// The length of the whole instructions at `ip` that a hook's jump
/// overwrites, if they run the same anywhere.
fn movable(code: &[u8], ip: u32) -> Option<usize> {
    let mut decoder = Decoder::with_ip(32, code, u64::from(ip), DecoderOptions::NONE);
    let mut length = 0;
    while length < HOOK {
        let instruction = decoder.decode();
        // 32-bit code has no IP-relative operands; only branches depend on
        // where they run.
        if instruction.is_invalid() || instruction.flow_control() != FlowControl::Next {
            return None;
        }
        length += instruction.len();
    }
    Some(length)
}

/// The displacement fields of the calls in `code`, a function at `ip`, to
/// an export of `entries`, each with the private entry it should reach.
fn calls(code: &[u8], ip: u32, entries: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let private = |target: u32| {
        entries
            .iter()
            .find(|&&(entry, _)| entry == target)
            .map(|&(_, private)| private)
    };
    // Most functions call neither export: rule them out without decoding.
    let candidate = |at: usize| {
        let displacement = i32::from_le_bytes(code[at + 1..at + 5].try_into().unwrap());
        code[at] == 0xe8
            && private(
                ip.wrapping_add(at as u32 + 5)
                    .wrapping_add(displacement as u32),
            )
            .is_some()
    };
    if !(0..code.len().saturating_sub(4)).any(candidate) {
        return Vec::new();
    }
    let mut decoder = Decoder::with_ip(32, code, u64::from(ip), DecoderOptions::NONE);
    let mut sites = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.mnemonic() == Mnemonic::Call
            && instruction.op0_kind() == OpKind::NearBranch32
            && instruction.len() == HOOK
            && let Some(private) = private(instruction.near_branch32())
        {
            sites.push((instruction.ip32() + 1, private));
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hot_patch_prologue_moves_whole_instructions() {
        // mov edi,edi; push ebp; mov ebp,esp; push esi
        let code = [0x8b, 0xff, 0x55, 0x8b, 0xec, 0x56];
        assert_eq!(movable(&code, 0x1000), Some(5));
        // push ebp; mov ebp,esp; sub esp,0x248
        let code = [0x55, 0x8b, 0xec, 0x81, 0xec, 0x48, 0x02, 0, 0];
        assert_eq!(movable(&code, 0x1000), Some(9));
        // A leading branch depends on where it runs.
        assert_eq!(movable(&[0xe9, 0, 0, 0, 0], 0x1000), None);
        assert_eq!(movable(&[0xe8, 0, 0, 0, 0, 0x90], 0x1000), None);
    }

    #[test]
    fn only_decoded_calls_to_an_export_are_found() {
        let ip = 0x1000;
        let entries = [(0x2000, 0x9000), (0x3000, 0x9100)];
        let call = |at: u32, target: u32| {
            let mut bytes = vec![0xe8];
            bytes.extend_from_slice(&target.wrapping_sub(ip + at + 5).to_le_bytes());
            bytes
        };
        let mut code = call(0, 0x2000);
        // mov eax,0x001ff5e8; add al,al: from offset 6 the bytes read as a
        // call to 0x3000 that never runs.
        code.extend([0xb8, 0xe8, 0xf5, 0x1f, 0x00, 0x00, 0xc0]);
        assert_eq!(call(6, 0x3000), code[6..11]);
        code.extend(call(12, 0x3000));
        code.extend(call(17, 0x4000));
        assert_eq!(
            calls(&code, ip, &entries),
            [(ip + 1, 0x9000), (ip + 13, 0x9100)]
        );
        assert!(calls(&call(0, 0x4000), ip, &entries).is_empty());
    }
}
