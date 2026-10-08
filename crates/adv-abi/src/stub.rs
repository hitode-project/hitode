//! Stubs return only a reviewed failure contract. No fabricated success or output objects.
use crate::{Registry, signature, trace};
use adv_machine::{Env, Outcome, layout::teb};
use std::any::Any;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    #[default]
    Strict,
    Discovery,
}

#[derive(Debug, Clone)]
pub struct StubCall {
    pub module: String,
    pub name: String,
    pub caller: u32,
    pub context: u32,
    pub arguments: Vec<String>,
    pub failure_value: Option<u32>,
}

/// A failed feature probe is an observation, not proof that the export was required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicLookupKind {
    Unimplemented,
    MissingExport,
    InvalidModule,
    InitializationFailed,
    ResolutionFailed,
}

impl DynamicLookupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unimplemented => "unimplemented",
            Self::MissingExport => "missing_export",
            Self::InvalidModule => "invalid_module",
            Self::InitializationFailed => "initialization_failed",
            Self::ResolutionFailed => "resolution_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicLookupFailure {
    pub sequence: u64,
    /// Requested DLL/export; a forwarded failure can have a different target.
    pub module: String,
    pub name: String,
    pub failure_module: Option<String>,
    pub failure_name: Option<String>,
    pub caller: u32,
    pub context: u32,
    pub kind: DynamicLookupKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBoxCall {
    pub sequence: u64,
    /// Presentation serial at the call, to distinguish later recovered progress.
    pub frame: u32,
    pub api: String,
    pub caller: u32,
    pub context: u32,
    pub caption: String,
    pub text: String,
    pub style: u32,
    pub result: u32,
    /// Chronological evidence only: optional probes can precede unrelated messages.
    pub preceding_lookup_failure: Option<DynamicLookupFailure>,
}

#[derive(Debug, Default)]
pub struct ApiDiagnostics {
    pub gaps: BTreeMap<String, u64>,
    pub recent_stubs: VecDeque<StubCall>,
    pub stub_calls: u64,
    /// Observed personality contract violations, independent of optional probes.
    pub structural_mismatches: u64,
    /// Explicit capability failures, not ordinary API errors such as missing files.
    pub warnings: BTreeMap<(String, String), u64>,
    pub unrecorded_warnings: u64,
    pub dynamic_lookup_failures: VecDeque<DynamicLookupFailure>,
    pub dynamic_lookup_failure_count: u64,
    pub dropped_dynamic_lookup_failures: u64,
    pub message_boxes: VecDeque<MessageBoxCall>,
    pub message_box_count: u64,
    pub dropped_message_boxes: u64,
    pub truncated_diagnostic_fields: u64,
    sequence: u64,
}

fn bounded_text(text: String, characters: usize, truncated: &mut u64) -> String {
    if text.len() <= characters {
        text
    } else {
        let mut chars = text.chars();
        let bounded = chars.by_ref().take(characters).collect();
        if chars.next().is_some() {
            *truncated = truncated.saturating_add(1);
            bounded
        } else {
            text
        }
    }
}

/// Keep failures visible even when the caller ignores their return values.
/// This shares the existing per-machine diagnostics owner and bounds distinct
/// reasons; repeated glyphs or capability probes do not flood stderr.
pub fn warn_unavailable(env: &mut Env<'_>, api: &str, reason: &str) {
    if crate::capability::defer_warning(env, api, reason) {
        return;
    }
    let caller = env.caller();
    emit_warning(env, api, reason, caller, None);
}

/// Structural mismatches disqualify acceptance even if a host frontend returns
/// success. Ordinary optional capability warnings keep their existing policy.
pub fn warn_structural(env: &mut Env<'_>, api: &str, reason: &str) {
    let diagnostics = env.ext.get_or_default::<ApiDiagnostics>();
    diagnostics.structural_mismatches = diagnostics.structural_mismatches.saturating_add(1);
    warn_unavailable(env, api, reason);
}

pub(crate) fn emit_warning(
    env: &mut Env<'_>,
    api: &str,
    reason: &str,
    caller: u32,
    sample: Option<&str>,
) {
    let diagnostics = env.ext.get_or_default::<ApiDiagnostics>();
    let key = (api.to_owned(), reason.to_owned());
    if let Some(count) = diagnostics.warnings.get_mut(&key) {
        *count = count.saturating_add(1);
    } else if diagnostics.warnings.len() < 64 {
        diagnostics.warnings.insert(key, 1);
        eprintln!(
            "warning: {api}: {reason} (guest caller {caller:#010x}; repetitions counted in report)"
        );
        if let Some(sample) = sample {
            eprintln!("  first rejected call arguments: {sample}");
        }
        env.sys
            .trace
            .warning(env.context, env.sys.frame, caller, api, reason, sample);
    } else {
        diagnostics.unrecorded_warnings = diagnostics.unrecorded_warnings.saturating_add(1);
        if diagnostics.unrecorded_warnings == 1 {
            eprintln!(
                "warning: additional distinct compatibility warnings omitted; see report count"
            );
            env.sys.trace.warning(
                env.context,
                env.sys.frame,
                caller,
                api,
                "additional distinct compatibility warnings omitted; see report count",
                None,
            );
        }
    }
}

impl ApiDiagnostics {
    pub fn acceptance_eligible(&self, mode: RunMode) -> bool {
        mode == RunMode::Strict && self.stub_calls == 0 && self.structural_mismatches == 0
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    /// Record only failed dynamic lookups, without changing the API's result or
    /// making an optional probe into a called stub. Counters expose truncation.
    pub fn record_dynamic_lookup_failure(&mut self, mut failure: DynamicLookupFailure) {
        failure.sequence = self.next_sequence();
        let truncated = &mut self.truncated_diagnostic_fields;
        failure.module = bounded_text(failure.module, 256, truncated);
        failure.name = bounded_text(failure.name, 256, truncated);
        failure.failure_module = failure
            .failure_module
            .map(|name| bounded_text(name, 256, truncated));
        failure.failure_name = failure
            .failure_name
            .map(|name| bounded_text(name, 256, truncated));
        failure.reason = bounded_text(failure.reason, 1024, truncated);
        self.dynamic_lookup_failure_count = self.dynamic_lookup_failure_count.saturating_add(1);
        if self.dynamic_lookup_failures.len() == 64 {
            self.dynamic_lookup_failures.pop_front();
            self.dropped_dynamic_lookup_failures =
                self.dropped_dynamic_lookup_failures.saturating_add(1);
        }
        self.dynamic_lookup_failures.push_back(failure);
    }

    /// Capture the decoded message even if no host logger is installed. The
    /// same-thread preceding lookup remains available after later probes evict
    /// the lookup ring; consumers must independently establish any attribution.
    pub fn record_message_box(&mut self, mut message: MessageBoxCall) {
        message.sequence = self.next_sequence();
        let truncated = &mut self.truncated_diagnostic_fields;
        message.api = bounded_text(message.api, 256, truncated);
        message.caption = bounded_text(message.caption, 256, truncated);
        message.text = bounded_text(message.text, 4096, truncated);
        message.preceding_lookup_failure = self
            .dynamic_lookup_failures
            .iter()
            .rev()
            .find(|failure| failure.context == message.context)
            .cloned();
        self.message_box_count = self.message_box_count.saturating_add(1);
        if self.message_boxes.len() == 32 {
            self.message_boxes.pop_front();
            self.dropped_message_boxes = self.dropped_message_boxes.saturating_add(1);
        }
        self.message_boxes.push_back(message);
    }

    fn record(&mut self, call: StubCall) {
        *self
            .gaps
            .entry(format!("{}!{}", call.module, call.name))
            .or_default() += 1;
        self.stub_calls += 1;
        if self.recent_stubs.len() == 16 {
            self.recent_stubs.pop_front();
        }
        self.recent_stubs.push_back(call);
    }
}

pub fn failure_value(signature: &signature::Signature) -> Option<u32> {
    if signature.return_type == "Windows.Win32.Foundation.HRESULT" {
        return Some(0x8000_4001);
    }
    include_str!("../../../data/abi/failures.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .find_map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            if fields[0] == signature.module && fields[1] == signature.name {
                Some(u32::from_str_radix(fields[2], 16).expect("reviewed failure value"))
            } else {
                None
            }
        })
}

pub(crate) fn dispatch(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let signature = &signature::signatures()[thunk.f.data as usize];
    let mode = (env.sys.thunks.resolver() as &dyn Any)
        .downcast_ref::<Registry>()
        .expect("ABI registry resolved stub")
        .mode();
    let failure = failure_value(signature);
    let call = StubCall {
        module: signature.module.to_owned(),
        name: signature.name.to_owned(),
        caller: env.caller(),
        context: env.context,
        arguments: trace::format_arguments(env.cpu, signature),
        failure_value: failure,
    };
    env.ext.get_or_default::<ApiDiagnostics>().record(call);
    warn_unavailable(
        env,
        &format!("{}!{}", signature.module, signature.name),
        "API has no implemented frontend; using a generated ABI failure stub",
    );
    if mode == RunMode::Strict {
        return Outcome::Fail(format!(
            "unimplemented: {}!{} (strict ABI stub)",
            signature.module, signature.name
        ));
    }
    if !signature.callable() {
        return Outcome::Fail(format!(
            "unimplemented: {}!{} (unsupported x86 ABI; discovery cannot return)",
            signature.module, signature.name
        ));
    }
    let Some(value) = failure else {
        return Outcome::Fail(format!(
            "unimplemented: {}!{} (no reviewed failure contract; discovery cannot return)",
            signature.module, signature.name
        ));
    };
    let last_error = env.ctx().teb + teb::LAST_ERROR;
    if let Err(fault) = crate::GuestPtr::<u32>::new(last_error).write(env.cpu, 120) {
        return crate::guest::memory_failure(fault);
    }
    Outcome::Return(value)
}

/// Existence metadata cannot authorize a return through an unknown ABI.
pub(crate) fn fixed_dispatch(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let module = thunk.module.clone();
    let name = thunk.name.clone();
    let api = format!("{module}!{name}");
    let call = StubCall {
        module,
        name,
        caller: env.caller(),
        context: env.context,
        arguments: Vec::new(),
        failure_value: None,
    };
    env.ext.get_or_default::<ApiDiagnostics>().record(call);
    warn_unavailable(
        env,
        &api,
        "fixed system export has no implemented frontend or verified ABI",
    );
    Outcome::Fail(format!(
        "unimplemented: {api} (fixed system export; unknown ABI cannot return)"
    ))
}
