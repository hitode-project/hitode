//! One live native-entry boundary for host frontends. The fast path is legal
//! only while the guest-visible export still names its original implementation.
use std::collections::HashMap;

use crate::Win32;
use adv_abi::guest::check_range;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::THUNK_STRIDE;
use adv_machine::{Env, Outcome};
use adv_os::{LoadError, Loader, ModuleId, ModuleKind, Modules};
use adv_pe::{ExportTarget, ImportName};

const PAGE: u32 = 4096;

fn system_module(modules: &Modules, name: &str) -> Result<Option<ModuleId>, LoadError> {
    if let Some((id, _)) = modules.iter().find(|(_, module)| {
        module.kind == ModuleKind::Host
            && module.executable_visible()
            && module.name.eq_ignore_ascii_case("ntdll.dll")
    }) {
        return Ok(Some(id));
    }
    if modules.by_name("ntdll.dll").is_some() {
        return Err(LoadError::MissingExport {
            module: "ntdll.dll".into(),
            name: name.into(),
        });
    }
    Ok(None)
}

/// Entries found original while every page that decides the lookup was still
/// an unwritten copy: the headers, the export data and the entry itself. A
/// write to any of them makes the next call inspect the image again. While the
/// backend's copy epoch stays put, no unwritten page anywhere has changed.
#[derive(Default)]
struct Unmodified(HashMap<String, Original>);

struct Original {
    module: ModuleId,
    base: u32,
    entry: u32,
    pages: Vec<u32>,
    epoch: Option<u64>,
}

fn unwritten(cpu: &dyn Guest, pages: &[u32]) -> bool {
    pages
        .iter()
        .all(|&page| cpu.file_page_copy_on_write(page) == Some(true))
}

/// Pages a synthetic image's export lookup and entry comparison read.
fn deciding_pages(cpu: &dyn Guest, base: u32, entry: u32) -> Option<Vec<u32>> {
    let header = cpu.read_u32(base + 0x3c).ok()?;
    let directory = base.checked_add(cpu.read_u32(base.checked_add(header)? + 120).ok()?)?;
    let size = cpu.read_u32(base.checked_add(header)? + 124).ok()?;
    let span = |start: u32, len: u32| {
        let end = start.checked_add(len.max(1) - 1)?;
        Some((start / PAGE..=end / PAGE).map(|page| page * PAGE))
    };
    let mut pages: Vec<u32> = span(base, 1)?
        .chain(span(directory, size)?)
        .chain(span(entry, THUNK_STRIDE)?)
        .collect();
    pages.dedup();
    Some(pages)
}

/// Read-only proof for leaves: an uncertain or changed image takes the ordinary path.
pub(crate) fn known_unmodified(env: &Env<'_>, name: &str) -> bool {
    let Some(original) = env
        .ext
        .try_get::<Unmodified>()
        .and_then(|cache| cache.0.get(name))
    else {
        return false;
    };
    env.ext
        .get::<Modules>()
        .try_get(original.module)
        .is_some_and(|image| image.base == original.base && image.executable_visible())
        && (original.epoch.is_some() && original.epoch == env.cpu.private_copy_epoch()
            || unwritten(env.cpu, &original.pages))
        && check_range(env.cpu, original.entry, THUNK_STRIDE, Prot::READ).is_ok()
}

pub(crate) fn overridden(env: &mut Env<'_>, name: &str) -> Result<Option<u32>, LoadError> {
    // Standalone handler embedders may install no Windows personality. There
    // is then no guest system image whose entry could have been replaced.
    if env.ext.try_get::<Win32>().is_none() || env.ext.try_get::<Modules>().is_none() {
        return Ok(None);
    }
    let epoch = env.cpu.private_copy_epoch();
    if let Some(original) = env
        .ext
        .try_get::<Unmodified>()
        .and_then(|cache| cache.0.get(name))
        && env
            .ext
            .get::<Modules>()
            .try_get(original.module)
            .is_some_and(|image| image.base == original.base && image.executable_visible())
        && (epoch.is_some() && original.epoch == epoch || unwritten(env.cpu, &original.pages))
        && check_range(env.cpu, original.entry, THUNK_STRIDE, Prot::READ).is_ok()
    {
        if original.epoch != epoch
            && let Some(original) = env.ext.get_mut::<Unmodified>().0.get_mut(name)
        {
            original.epoch = epoch;
        }
        return Ok(None);
    }
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let Some(module) = system_module(modules, name)? else {
        return Ok(None);
    };
    let result = Loader::new(env.cpu, env.sys, modules, &win32.vfs).guest_override(module, name);
    if let Ok(None) = result {
        let image = modules.get(module);
        let base = image.base;
        let entry = match image.exports_by_name.get(name) {
            Some(ExportTarget::Rva(rva)) => base.checked_add(*rva),
            _ => None,
        };
        if let Some(entry) = entry
            && let Some(pages) = deciding_pages(env.cpu, base, entry)
            && unwritten(env.cpu, &pages)
        {
            env.ext.get_or_default::<Unmodified>().0.insert(
                name.to_owned(),
                Original {
                    module,
                    base,
                    entry,
                    pages,
                    epoch: env.cpu.private_copy_epoch(),
                },
            );
        }
    }
    result
}

pub(crate) fn entry(env: &mut Env<'_>, name: &str) -> Result<u32, u32> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let module = system_module(modules, name)
        .map_err(|_| 0xc00000bbu32)?
        .ok_or(0xc00000bbu32)?;
    let mut loader = Loader::new(env.cpu, env.sys, modules, &win32.vfs);
    if let Some(target) = loader
        .guest_override(module, name)
        .map_err(|_| 0xc00000bbu32)?
    {
        return Ok(target);
    }
    loader
        .resolve_dynamic(
            module,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .map(|(target, _)| target)
        .map_err(|_| 0xc00000bbu32)
}

/// Equal-ABI adapters share this path. It never calls the front end again and
/// never consumes a guest override by calling its old host handler directly.
pub(crate) fn route(
    env: &mut Env<'_>,
    name: &'static str,
    args: impl Into<Vec<u32>>,
    fast: impl FnOnce(&mut Env<'_>) -> Outcome,
) -> Outcome {
    match overridden(env, name) {
        Ok(Some(target)) => Outcome::call(target, args, |_, value| Outcome::Return(value.value())),
        Ok(None) => fast(env),
        Err(_) => unavailable(env, name),
    }
}

pub(crate) fn unavailable(env: &mut Env<'_>, name: &str) -> Outcome {
    adv_abi::warn_structural(
        env,
        &format!("ntdll.dll!{name}"),
        "live native entry cannot be resolved; host frontend cannot bypass it",
    );
    Outcome::Fail(format!("native boundary unavailable: ntdll.dll!{name}"))
}

/// Explicitly record remaining structural gaps, independently of tracing.
pub(crate) fn diagnose_bypass(env: &mut Env<'_>, frontend: &str, names: &[&str]) {
    for name in names {
        if !matches!(overridden(env, name), Ok(None)) {
            adv_abi::warn_structural(
                env,
                frontend,
                &format!("host frontend bypasses the modified ntdll.dll!{name} entry"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_abi::{ApiDiagnostics, Registry, RunMode};
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Prot},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[test]
    fn bypass_diagnostic_requires_a_live_override_and_disqualifies_strict_acceptance() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            crate::Win32Config::new(
                "/nonexistent/native-boundary",
                "fixture.exe",
                "/nonexistent/native-boundary-data",
            ),
        )
        .unwrap();
        Win32::prepare_process_infrastructure(&mut machine).unwrap();
        machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        machine.cpu.write(0x400000, &[0xc3]).unwrap();
        machine
            .spawn("diagnostic fixture", 0x400000, &[], 65536)
            .unwrap();
        let mut env = Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        diagnose_bypass(&mut env, "fixture.dll!frontend", &["NtReadFile"]);
        assert!(
            env.ext
                .get_or_default::<ApiDiagnostics>()
                .acceptance_eligible(RunMode::Strict)
        );
        let target = entry(&mut env, "NtReadFile").unwrap();
        let mut jump = vec![0xe9];
        jump.extend(0x400000u32.wrapping_sub(target + 5).to_le_bytes());
        env.cpu.write(target, &jump).unwrap();
        diagnose_bypass(&mut env, "fixture.dll!frontend", &["NtReadFile"]);
        diagnose_bypass(&mut env, "fixture.dll!frontend", &["NtReadFile"]);
        let diagnostics = env.ext.get::<ApiDiagnostics>();
        assert!(!diagnostics.acceptance_eligible(RunMode::Strict));
        assert_eq!(diagnostics.warnings.len(), 1);
        assert_eq!(diagnostics.warnings.values().next(), Some(&2));
        let routed = route(&mut env, "NtReadFile", [9, 8, 7], |_| {
            panic!("modified entry must not use host fast path")
        });
        assert!(
            matches!(routed, Outcome::Call { target: actual, args, .. } if actual == target && args == [9, 8, 7])
        );
    }
}
