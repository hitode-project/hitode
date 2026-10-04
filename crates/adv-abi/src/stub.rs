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

#[derive(Debug, Default)]
pub struct ApiDiagnostics {
    pub gaps: BTreeMap<String, u64>,
    pub recent_stubs: VecDeque<StubCall>,
    pub stub_calls: u64,
    /// Explicit capability failures, not ordinary API errors such as missing files.
    pub warnings: BTreeMap<(String, String), u64>,
    pub unrecorded_warnings: u64,
}

/// Keep failures visible even when the caller ignores their return values.
/// This shares the existing per-machine diagnostics owner and bounds distinct
/// reasons; repeated glyphs or capability probes do not flood stderr.
pub fn warn_unavailable(env: &mut Env<'_>, api: &str, reason: &str) {
    let caller = env.caller();
    let diagnostics = env.ext.get_or_default::<ApiDiagnostics>();
    let key = (api.to_owned(), reason.to_owned());
    if let Some(count) = diagnostics.warnings.get_mut(&key) {
        *count = count.saturating_add(1);
    } else if diagnostics.warnings.len() < 64 {
        diagnostics.warnings.insert(key, 1);
        eprintln!(
            "warning: {api}: {reason} (guest caller {caller:#010x}; repetitions counted in report)"
        );
    } else {
        diagnostics.unrecorded_warnings = diagnostics.unrecorded_warnings.saturating_add(1);
        if diagnostics.unrecorded_warnings == 1 {
            eprintln!(
                "warning: additional distinct compatibility warnings omitted; see report count"
            );
        }
    }
}

impl ApiDiagnostics {
    pub fn acceptance_eligible(&self, mode: RunMode) -> bool {
        mode == RunMode::Strict && self.stub_calls == 0
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
