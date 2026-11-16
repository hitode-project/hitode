//! Read-only Wine session observations for the shell's live debugger.
use super::*;
use adv_cpu::backend::GuestExt;

pub(super) fn counters(machine: &Machine<Cpu>) -> [u64; 8] {
    use crate::Engine;
    let cpu = machine.cpu.counters();
    [
        machine.sys.host_calls,
        cpu.native_entries,
        cpu.interpreter_entries,
        cpu.retired_forms.unwrap_or_default()[0],
        cpu.retired_forms.unwrap_or_default()[1],
        cpu.retired_forms.unwrap_or_default()[2],
        cpu.retired_forms.unwrap_or_default()[3],
        cpu.retired_forms.unwrap_or_default()[4],
    ]
}

fn infer_module(api: &str) -> Option<&'static str> {
    if api.starts_with("IDirect3D") {
        Some("d3d9.dll")
    } else if api.starts_with("IDirectDraw") {
        Some("ddraw.dll")
    } else if api.starts_with("IDirectSound") || api.starts_with("DirectSound") {
        Some("dsound.dll")
    } else if api.starts_with("IWM") {
        Some("wmvcore.dll")
    } else if api.starts_with("IGraphBuilder::")
        || api.starts_with("IMediaControl::")
        || api.starts_with("IMemInputPin::")
        || api.starts_with("IDMOWrapperFilter::")
        || api.starts_with("IBasicVideo")
        || api.starts_with("IVideoWindow")
        || api.starts_with("DirectShow")
        || api.starts_with("MPEG-")
    {
        Some("quartz.dll")
    } else {
        None
    }
}

fn normalize_api_gap(api: &str) -> Option<String> {
    let formatted = if let Some((module, export)) = api.split_once('!') {
        format!("{}!{export}", module.to_ascii_lowercase())
    } else {
        let module = infer_module(api)?;
        let compact: String = api
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '-')
            .collect();
        format!("{module}!{compact}")
    };
    (formatted.len() <= 160
        && formatted.bytes().all(|b| b.is_ascii_graphic())
        && formatted
            .split_once('!')
            .is_some_and(|(module, export)| !module.is_empty() && !export.is_empty()))
    .then_some(formatted)
}

fn is_capability_gap(api: &str, reason: &str) -> bool {
    let (_, name) = api.split_once('!').unwrap_or(("", api));
    !(name == "IDirect3DDevice9::GetAvailableTextureMem"
        || name.starts_with("IDirect3D9::Check")
        || (name.starts_with("EnumFont") && reason.starts_with("font unavailable; substituting")))
}

/// Merge host-side ABI stubs and capability rejections from `machine` into the
/// session gap set without changing `failure`.
pub(super) fn merge_api_diagnostics(
    machine: &Machine<Cpu>,
    gaps: &mut adv_nt::exception::StubGaps,
) {
    let Some(diagnostics) = machine.ext.try_get::<adv_abi::ApiDiagnostics>() else {
        return;
    };
    for api in diagnostics
        .gaps
        .keys()
        .chain(diagnostics.reviewed_calls.keys())
    {
        if let Some(normalized) = normalize_api_gap(api) {
            gaps.called.insert(normalized);
        }
    }
    for (api, reason) in diagnostics.warnings.keys() {
        if is_capability_gap(api, reason)
            && let Some(normalized) = normalize_api_gap(api)
        {
            gaps.called.insert(normalized);
        }
    }
}

impl Session {
    /// Cumulative family counters, including processes that have exited.
    pub fn debug_counters(&self) -> [u64; 8] {
        let mut totals = self.retired_debug;
        self.debug_processes(|machine| {
            for (total, count) in totals.iter_mut().zip(counters(machine)) {
                *total += count;
            }
        });
        totals
    }

    /// Cumulative boundary counters of the family, including processes
    /// that have exited.
    pub fn boundaries(&self) -> adv_nt::counters::Boundaries {
        let mut totals = self.retired_boundaries.clone();
        self.debug_processes(|machine| {
            totals.merge(&adv_nt::counters::Boundaries::of(&machine.ext));
        });
        totals
    }

    pub fn debug_modules(&self) -> Result<Vec<adv_nt::process::LoadedModule>, String> {
        adv_nt::process::loaded_modules(&self.machine.cpu, self.machine.sys.peb)
    }

    pub fn guest_place(&self, address: u32) -> String {
        if let Some(place) = adv_nt::identity::place(&self.machine.ext, address) {
            return format!("pid {:#x} {place}", self.machine.sys.pid);
        }
        let place = self
            .debug_modules()
            .ok()
            .and_then(|modules| {
                modules
                    .into_iter()
                    .find(|m| address.wrapping_sub(m.base) < m.size)
            })
            .map_or_else(
                || format!("{address:#010x}"),
                |m| format!("{}+{:#x}", m.name, address - m.base),
            );
        format!("pid {:#x} {place}", self.machine.sys.pid)
    }

    pub fn modules_report(&self) -> String {
        let mut text = String::new();
        self.debug_processes(|machine| {
            text.push_str(&format!(
                "pid {:#x}{}:\n",
                machine.sys.pid,
                if machine.sys.pid == self.machine.sys.pid {
                    " (presented)"
                } else {
                    ""
                }
            ));
            match adv_nt::process::loaded_modules(&machine.cpu, machine.sys.peb) {
                Ok(mut modules) => {
                    modules.sort_by_key(|m| m.base);
                    for m in modules {
                        text.push_str(&format!(
                            "{:#010x} {:#010x} Image    {}\n",
                            m.base, m.size, m.name
                        ));
                    }
                }
                Err(error) => text.push_str(&format!("modules unavailable: {error}\n")),
            }
        });
        text
    }

    pub fn contexts_report(&self) -> String {
        let mut text =
            String::from("Wine guest contexts (machine waits; server objects are owned by Wine):");
        self.debug_processes(|machine| {
            for context in &machine.sys.contexts {
                text.push_str(&format!(
                    "\npid {:#x} [{}] tid {:#x}: {}",
                    machine.sys.pid,
                    context.id,
                    context.tid,
                    context.state_name()
                ));
                if let Some(wait) = context.waiting_on() {
                    text.push_str(&format!(" {wait:?}"));
                }
                if let Some(thunk) = context.blocked_thunk() {
                    let call = machine.sys.thunks.get(thunk);
                    text.push_str(&format!(" in {}!{}", call.module, call.name));
                }
                if let Some(args) = context.blocked_args() {
                    text.push_str(&format!(" args {args:x?}"));
                }
                if let Some(caller) = context
                    .blocked_stack()
                    .and_then(|sp| machine.cpu.read_u32(sp).ok())
                {
                    text.push_str(&format!(" caller {caller:#010x}"));
                }
                if let Some(exit) = context.exit_code() {
                    text.push_str(&format!(" exit {exit}"));
                }
            }
        });
        text
    }

    /// Merge host-module ABI stubs and capability rejections from all live
    /// processes into `self.stub_gaps`.
    pub fn sync_api_diagnostics(&self) {
        let mut gaps = self.stub_gaps.borrow_mut();
        self.debug_processes(|machine| merge_api_diagnostics(machine, &mut gaps));
    }

    /// Visit the foreground and parked processes without changing execution state.
    pub fn debug_processes(&self, mut visit: impl FnMut(&Machine<Cpu>)) {
        visit(&self.machine);
        for machine in self.shared.driver.borrow().parked.values() {
            visit(machine);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::{Clock, layout::PARK_ADDR};

    // spec: REPORT-GAP-1
    #[test]
    fn capability_warnings_and_abi_stubs_normalize_into_stub_gaps_without_setting_failure() {
        let mut machine = Machine::new(
            Cpu::new(PARK_ADDR, false).unwrap(),
            Resolver(Registry::default()),
            Clock::virtual_time(),
        )
        .unwrap();
        let diagnostics = machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        diagnostics
            .gaps
            .insert("DDRAW.dll!DirectDrawCreate".into(), 1);
        diagnostics
            .reviewed_calls
            .insert("dwrite.dll!DWriteCreateFactory".into(), 1);
        diagnostics.warnings.insert(
            (
                "IDirect3DDevice9::SetLight".into(),
                "unsupported light type".into(),
            ),
            1,
        );
        diagnostics.warnings.insert(
            (
                "IDMOWrapperFilter::Init".into(),
                "only Windows Media decoders".into(),
            ),
            1,
        );
        diagnostics
            .warnings
            .insert(("MPEG-1 Stream Splitter".into(), "unsupported".into()), 1);
        diagnostics.warnings.insert(
            (
                "wmvcore.dll!IWMSyncReader::GetNextSample".into(),
                "unsupported".into(),
            ),
            1,
        );
        // Benign format-probe and informational warnings must stay excluded.
        diagnostics.warnings.insert(
            (
                "d3d9.dll!IDirect3DDevice9::GetAvailableTextureMem".into(),
                "estimate is 0".into(),
            ),
            1,
        );
        diagnostics.warnings.insert(
            (
                "d3d9.dll!IDirect3D9::CheckDeviceFormat".into(),
                "D3DERR_NOTAVAILABLE".into(),
            ),
            1,
        );
        diagnostics.warnings.insert(
            (
                "gdi32.dll!EnumFontFamiliesExA".into(),
                "font unavailable; substituting".into(),
            ),
            1,
        );

        let mut gaps = adv_nt::exception::StubGaps::default();
        merge_api_diagnostics(&machine, &mut gaps);
        assert!(gaps.failure.is_none());
        let collected: Vec<_> = gaps.called.into_iter().collect();
        assert_eq!(
            collected,
            vec![
                "d3d9.dll!IDirect3DDevice9::SetLight",
                "ddraw.dll!DirectDrawCreate",
                "dwrite.dll!DWriteCreateFactory",
                "quartz.dll!IDMOWrapperFilter::Init",
                "quartz.dll!MPEG1StreamSplitter",
                "wmvcore.dll!IWMSyncReader::GetNextSample",
            ]
        );
    }
}
