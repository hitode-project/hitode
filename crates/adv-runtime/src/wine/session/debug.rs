//! Read-only Wine session observations for the shell's live debugger.
use super::*;
use adv_cpu::backend::GuestExt;

/// Decodes only the single `iced_x86::Code` enum name at `pc`, never bytes or
/// operands, so reports identify the faulting instruction form safely.
pub fn decode_fault_insn(machine: &Machine<Cpu>, pc: u32) -> Option<String> {
    if pc == 0 {
        return None;
    }
    let mut bytes = [0u8; 15];
    let mut readable = bytes.len();
    while readable > 0 && machine.cpu.read(pc, &mut bytes[..readable]).is_err() {
        readable -= 1;
    }
    if readable == 0 {
        return None;
    }
    let mut decoder = iced_x86::Decoder::with_ip(
        32,
        &bytes[..readable],
        u64::from(pc),
        iced_x86::DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    (instruction.code() != iced_x86::Code::INVALID).then(|| format!("{:?}", instruction.code()))
}

fn after_call_in_images(
    machine: &Machine<Cpu>,
    address: u32,
    images: &adv_nt::identity::Images,
) -> bool {
    if address < 7 || images.containing(address).is_none() {
        return false;
    }
    let mut bytes = [0u8; 7];
    if machine.cpu.read(address - 7, &mut bytes).is_err() {
        return false;
    }
    let call = |modrm: u8| modrm & 0x38 == 0x10;
    bytes[2] == 0xe8
        || (bytes[5] == 0xff && call(bytes[6]) && bytes[6] >> 6 == 3)
        || (bytes[5] == 0xff
            && call(bytes[6])
            && bytes[6] >> 6 == 0
            && bytes[6] & 7 != 4
            && bytes[6] & 7 != 5)
        || (bytes[4] == 0xff && call(bytes[5]) && bytes[5] >> 6 == 1)
        || (bytes[4] == 0xff && call(bytes[5]) && bytes[5] >> 6 == 0 && bytes[5] & 7 == 4)
        || (bytes[3] == 0xff && call(bytes[4]) && bytes[4] >> 6 == 1 && bytes[4] & 7 == 4)
        || (bytes[1] == 0xff && call(bytes[2]) && (bytes[2] >> 6 == 2 || bytes[2] & 0xc7 == 5))
        || (bytes[0] == 0xff && call(bytes[1]) && bytes[1] >> 6 == 2 && bytes[1] & 7 == 4)
}

/// Collects up to `limit` guest code addresses from the current context's
/// registers and stack.
pub fn current_stack_addresses(machine: &Machine<Cpu>, limit: usize) -> Vec<u32> {
    use adv_cpu::backend::Reg;
    guest_stack_addresses(
        machine,
        machine.sys.current,
        machine.cpu.reg(Reg::Eip),
        machine.cpu.reg(Reg::Esp),
        machine.cpu.reg(Reg::Ebp),
        limit,
    )
}

/// Collects up to `limit` guest code addresses for a failure report: starts
/// with `pc`, walks the EBP frame chain inside the thread stack, then falls
/// back to scanning stack words that follow a CALL inside a mapped image.
pub fn guest_stack_addresses(
    machine: &Machine<Cpu>,
    context: u32,
    pc: u32,
    sp: u32,
    bp: u32,
    limit: usize,
) -> Vec<u32> {
    let mut frames = Vec::with_capacity(limit);
    if pc != 0 && limit > 0 {
        frames.push(pc);
    }
    let Some(stack) = machine.sys.contexts.get(context as usize) else {
        return frames;
    };
    let in_stack = |addr: u32, bytes: u32| {
        addr >= sp
            && addr >= stack.stack_lo
            && addr
                .checked_add(bytes)
                .is_some_and(|end| end <= stack.stack_hi)
    };
    let mut frame = bp;
    while frames.len() < limit && in_stack(frame, 8) {
        let Ok(next) = machine.cpu.read_u32(frame) else {
            break;
        };
        let Ok(ret) = machine.cpu.read_u32(frame + 4) else {
            break;
        };
        if ret != 0 && frames.last().copied() != Some(ret) {
            frames.push(ret);
        }
        if next <= frame {
            break;
        }
        frame = next;
    }
    if frames.len() < limit
        && let Some(images) = machine.ext.try_get::<adv_nt::identity::Images>()
        && sp >= stack.stack_lo
        && sp < stack.stack_hi
    {
        let end = stack.stack_hi.min(sp.saturating_add(4096));
        let mut cursor = sp & !3;
        while cursor + 4 <= end && frames.len() < limit {
            let Ok(word) = machine.cpu.read_u32(cursor) else {
                break;
            };
            if after_call_in_images(machine, word, images) && !frames.contains(&word) {
                frames.push(word);
            }
            cursor += 4;
        }
    }
    frames
}

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

/// Raw read-only thread state captured for hang and live-report snapshots.
pub struct RawThreadSnapshot {
    pub tid: u32,
    pub state: &'static str,
    pub blocked_api: Option<String>,
    pub stack: Vec<u32>,
}

/// Raw read-only session diagnostics collected at snapshot or exit time.
pub struct RawDiagnostics {
    pub warnings: Vec<(String, String, u64)>,
    pub rejected_probes: Vec<(String, String, u64)>,
    pub wine_errors: Vec<(String, String, String, u64)>,
    pub hooked_exports: Vec<(String, &'static str)>,
    pub exceptions_seen: Vec<(u32, u64, u32)>,
    pub graphics_fronts: Vec<&'static str>,
    pub audio_fronts: Vec<&'static str>,
    pub video_fronts: Vec<&'static str>,
    pub font_events: adv_abi::FontDiagnostics,
    pub windows: (u32, u32, u32),
    pub top_cliffs: Vec<(String, u64)>,
}

impl Session {
    /// Active guest threads across the session's processes, with bounded
    /// guest stack addresses for hang diagnosis.
    pub fn collect_thread_snapshots(&self, limit: usize) -> Vec<RawThreadSnapshot> {
        use adv_cpu::backend::Reg;
        let mut threads = Vec::new();
        self.debug_processes(|machine| {
            for context in &machine.sys.contexts {
                if threads.len() >= limit || context.is_done() {
                    continue;
                }
                let blocked_api = context.blocked_thunk().map(|thunk| {
                    let call = machine.sys.thunks.get(thunk);
                    format!("{}!{}", call.module.to_ascii_lowercase(), call.name)
                });
                let (pc, sp, bp) = if let Some(sp) = context.blocked_stack() {
                    let pc = machine.cpu.read_u32(sp).unwrap_or(0);
                    (pc, sp, 0)
                } else if machine.sys.current == context.id {
                    (
                        machine.cpu.reg(Reg::Eip),
                        machine.cpu.reg(Reg::Esp),
                        machine.cpu.reg(Reg::Ebp),
                    )
                } else {
                    (0, 0, 0)
                };
                let stack = if sp != 0 {
                    guest_stack_addresses(machine, context.id, pc, sp, bp, 6)
                } else {
                    Vec::new()
                };
                threads.push(RawThreadSnapshot {
                    tid: context.tid,
                    state: context.state_name(),
                    blocked_api,
                    stack,
                });
            }
        });
        threads
    }

    /// Collects bounded, read-only diagnostics across live processes.
    pub fn collect_diagnostics_raw(&self) -> RawDiagnostics {
        use crate::Engine;
        let mut warnings_map: BTreeMap<(String, String), u64> = BTreeMap::new();
        let mut probes_map: BTreeMap<(String, String), u64> = BTreeMap::new();
        let mut hooks_map: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut exceptions_map: BTreeMap<u32, (u64, u32)> = BTreeMap::new();
        let mut font_events = adv_abi::FontDiagnostics::default();
        let mut graphics_fronts = std::collections::BTreeSet::<&'static str>::new();
        let mut audio_fronts = std::collections::BTreeSet::<&'static str>::new();
        let mut video_fronts = std::collections::BTreeSet::<&'static str>::new();

        self.debug_processes(|machine| {
            if let Some(diag) = machine.ext.try_get::<adv_abi::ApiDiagnostics>() {
                for ((api, reason), &count) in &diag.warnings {
                    let api_norm = normalize_api_gap(api).unwrap_or_else(|| api.clone());
                    *warnings_map.entry((api_norm, reason.clone())).or_default() += count;
                }
                for ((api, detail), &count) in &diag.probe_rejections {
                    *probes_map.entry((api.clone(), detail.clone())).or_default() += count;
                }
                font_events.substitutions = font_events
                    .substitutions
                    .saturating_add(diag.font_events.substitutions);
                for face in &diag.font_events.missing_faces {
                    if font_events.missing_faces.len() < 32 {
                        font_events.missing_faces.insert(face.clone());
                    }
                }
                for &cs in &diag.font_events.charsets {
                    font_events.record_charset(cs);
                }
                for &cp in &diag.font_events.gdi_codepages {
                    font_events.record_gdi_codepage(cp);
                }
            }
            for (api, form) in adv_nt::identity::detect_hooks(&machine.cpu, &machine.ext) {
                hooks_map.entry(api).or_insert(form);
            }
            if let Some(counts) = machine.ext.try_get::<adv_nt::exception::ExceptionCounts>() {
                for (&code, &(count, last_pc)) in &counts.entries {
                    let entry = exceptions_map.entry(code).or_insert((0, last_pc));
                    entry.0 = entry.0.saturating_add(count);
                    entry.1 = last_pc;
                }
            }
            if let Some(images) = machine.ext.try_get::<adv_nt::identity::Images>() {
                for module in images.0.values() {
                    match module.name.as_str() {
                        "ddraw.dll" => {
                            graphics_fronts.insert("ddraw");
                        }
                        "d3d8.dll" => {
                            graphics_fronts.insert("d3d8");
                        }
                        "d3d9.dll" => {
                            graphics_fronts.insert("d3d9");
                        }
                        "d3d10.dll" | "d3d10_1.dll" => {
                            graphics_fronts.insert("d3d10");
                        }
                        "d3d11.dll" => {
                            graphics_fronts.insert("d3d11");
                        }
                        "dxgi.dll" => {
                            graphics_fronts.insert("dxgi");
                        }
                        "opengl32.dll" => {
                            graphics_fronts.insert("opengl");
                        }
                        "dsound.dll" => {
                            audio_fronts.insert("dsound");
                        }
                        "winmm.dll" => {
                            audio_fronts.insert("winmm");
                        }
                        "xaudio2_7.dll" | "xaudio2_8.dll" | "xaudio2_9.dll" => {
                            audio_fronts.insert("xaudio2");
                        }
                        "dmusic.dll" | "dmime.dll" | "dmsynth.dll" => {
                            audio_fronts.insert("dmusic");
                        }
                        "quartz.dll" => {
                            video_fronts.insert("dshow");
                        }
                        "amstream.dll" => {
                            video_fronts.insert("amstream");
                        }
                        "wmvcore.dll" => {
                            video_fronts.insert("wmvcore");
                        }
                        "msvfw32.dll" | "avifil32.dll" => {
                            video_fronts.insert("vfw");
                        }
                        _ => {}
                    }
                }
            }
        });

        let mut warnings: Vec<_> = warnings_map
            .into_iter()
            .map(|((api, reason), count)| (api, reason, count))
            .collect();
        warnings.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        warnings.truncate(24);

        let mut rejected_probes: Vec<_> = probes_map
            .into_iter()
            .map(|((api, detail), count)| (api, detail, count))
            .collect();
        rejected_probes.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        rejected_probes.truncate(16);

        let mut wine_errors: Vec<_> = self
            .wine_logs
            .borrow()
            .entries
            .iter()
            .map(|((level, channel, func), &count)| {
                (level.clone(), channel.clone(), func.clone(), count)
            })
            .collect();
        wine_errors.sort_unstable_by(|a, b| b.3.cmp(&a.3).then(a.1.cmp(&b.1)));
        wine_errors.truncate(24);

        let hooked_exports: Vec<_> = hooks_map.into_iter().take(24).collect();

        let mut exceptions_seen: Vec<_> = exceptions_map
            .into_iter()
            .map(|(code, (count, last_pc))| (code, count, last_pc))
            .collect();
        exceptions_seen.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        exceptions_seen.truncate(16);

        let view = project::view(&self.machine.ext);
        let top_level = view.windows.len() as u32;
        let visible = view.windows.iter().filter(|w| w.visible).count() as u32;
        let dialogs = self.dialogs().dialogs.len() as u32;
        let top_cliffs = self.machine.cpu.top_cliffs(8);

        RawDiagnostics {
            warnings,
            rejected_probes,
            wine_errors,
            hooked_exports,
            exceptions_seen,
            graphics_fronts: graphics_fronts.into_iter().collect(),
            audio_fronts: audio_fronts.into_iter().collect(),
            video_fronts: video_fronts.into_iter().collect(),
            font_events,
            windows: (top_level, visible, dialogs),
            top_cliffs,
        }
    }

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
