//! One live native-entry boundary for host frontends. The fast path is legal
//! only while the guest-visible export still names its original implementation.
use crate::Win32;
use adv_machine::{Env, Outcome};
use adv_os::{LoadError, Loader, ModuleId, ModuleKind, Modules};
use adv_pe::ImportName;

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

pub(crate) fn overridden(env: &mut Env<'_>, name: &str) -> Result<Option<u32>, LoadError> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let Some(module) = system_module(modules, name)? else {
        return Ok(None);
    };
    Loader::new(env.cpu, env.sys, modules, &win32.vfs).guest_override(module, name)
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
