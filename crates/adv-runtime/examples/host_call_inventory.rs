//! Export the assembled API registry as a conservative call-boundary audit.
//! Usage: cargo run -p adv-runtime --features gpu --example host_call_inventory
//!        -- --write docs/host-call-boundary.tsv
//! Input: runtime registrations, including COM methods. Output: sorted TSV.
//! --check PATH rejects stale rows. Unproven effects stay ordinary; a Windows
//! potential-kernel label is an upper bound, never proof that every call traps.
use adv_abi::HostFn;

fn row(module: &str, name: &str, f: HostFn) -> String {
    let api = format!("{module}!{name}");
    let route = if f.guest.is_some() {
        "guest"
    } else if f.leaf.is_some() || f.env_leaf.is_some() {
        "leaf"
    } else {
        "ordinary"
    };
    let lower = name.to_ascii_lowercase();
    let refs = name.ends_with("::AddRef") || name.ends_with("::Release");
    let sync = name.contains("CriticalSection");
    let heap = name.contains("Heap");
    let clock = [
        "timeGetTime",
        "GetTickCount",
        "QueryPerformanceCounter",
        "QueryPerformanceFrequency",
        "GetLocalTime",
        "GetSystemTime",
        "GetSystemTimeAsFileTime",
    ]
    .contains(&name);
    let live = clock
        || [
            "Cursor",
            "KeyState",
            "KeyboardState",
            "Focus",
            "ActiveWindow",
            "ForegroundWindow",
            "Capture",
            "SystemMetrics",
        ]
        .iter()
        .any(|s| name.contains(s));
    let memory_only = name.starts_with("Interlocked")
        || name.starts_with("lstr")
        || name == "RtlCompareMemory"
        || [
            "GetLastError",
            "SetLastError",
            "TlsGetValue",
            "SetRectEmpty",
            "IsRectEmpty",
            "IntersectRect",
            "UnionRect",
        ]
        .contains(&name);
    let windows = if memory_only {
        "user"
    } else if refs || sync || heap {
        "user; kernel on slow/final edge"
    } else if module == "user32.dll" {
        "user cache or win32k"
    } else if clock {
        "user clock; kernel fallback possible"
    } else {
        "potential kernel; not excluded"
    };
    let (block, callback, mapping, state, reason) = if refs {
        (
            "final/unsupported edge",
            "aggregation rejected or controlling handler",
            "final cleanup",
            "shared COM count/internal owners",
            "CAS above zero; original zero/overflow handler; patched vtables remain live",
        )
    } else if f.guest.is_some() && sync {
        (
            "contention fallback",
            "no",
            "no",
            "shared lock/waiters",
            "guest fast path; existing wait/wake links",
        )
    } else if f.guest.is_some() && heap {
        (
            "slow edge not excluded",
            "live native hooks on fallback",
            "growth/final fallback",
            "shared heap",
            "guest heap header/list path; original slow edge",
        )
    } else if f.guest.is_some() && name == "DebugBreak" {
        (
            "exception dispatch",
            "exception handlers",
            "no",
            "exception context",
            "guest INT3 retains exception dispatch",
        )
    } else if f.guest.is_some() && module == "<win32>" {
        (
            "guest execution",
            "DLL entry/exception continuation",
            "ordinary continuation",
            "CPU context",
            "guest lifecycle/exception body; not a host leaf",
        )
    } else if f.guest.is_some() {
        (
            "no",
            "no",
            "no",
            if name.starts_with("Interlocked") {
                "shared guest word"
            } else {
                "guest memory/context"
            },
            "authored guest body; ordinary implementation retained",
        )
    } else if route == "leaf" {
        (
            if ["GetTickCount", "timeGetTime", "QueryPerformanceCounter"].contains(&name) {
                "poll fallback only"
            } else {
                "no"
            },
            "no",
            "no",
            "handler-owned state; same source",
            "explicit nonblocking registration; decline before effects",
        )
    } else {
        // Conservative admission is intentional: source/name similarity cannot
        // establish nonblocking behavior or rule out indirect hooks and maps.
        let waits = [
            "wait",
            "sleep",
            "messagebox",
            "dialogbox",
            "getmessage",
            "readfile",
            "writefile",
            "recv",
            "connect",
        ]
        .iter()
        .any(|s| lower.contains(s));
        let hooks = [
            "enum",
            "sendmessage",
            "dispatchmessage",
            "callwindow",
            "loadlibrary",
            "callback",
        ]
        .iter()
        .any(|s| lower.contains(s));
        (
            if waits {
                "yes/conditional"
            } else {
                "not excluded"
            },
            if hooks {
                "yes/conditional"
            } else {
                "not excluded"
            },
            if heap
                || ["alloc", "free", "virtual", "mapview", "loadlibrary"]
                    .iter()
                    .any(|s| lower.contains(s))
            {
                "yes/conditional"
            } else {
                "not excluded"
            },
            "not excluded",
            "ordinary required until all effects are proven safe",
        )
    };
    format!(
        "{api}\t{windows}\t{block}\t{callback}\t{mapping}\t{state}\t{}\t{route}\t{reason}",
        if live {
            "live clock/input/window source"
        } else {
            "not required by fast path; ordinary not excluded"
        }
    )
}
fn inventory() -> String {
    let registry = adv_runtime::boot::api_registry(adv_runtime::vorbis::Policy::Native);
    let mut rows: Vec<_> = registry
        .registered()
        .map(|(module, name, f)| row(module, name, f))
        .collect();
    rows.sort();
    format!(
        "api\twindows_boundary\tblocking\tguest_callbacks\tmapping_changes\tcross_context_state\trealtime_state\troute\treason\n{}\n",
        rows.join("\n")
    )
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let table = inventory();
    match args.as_slice() {
        [mode, path] if mode == "--write" => std::fs::write(path, &table).unwrap(),
        [mode, path] if mode == "--check" => assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            table,
            "stale API inventory"
        ),
        [] => print!("{table}"),
        _ => panic!("use --write PATH or --check PATH"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // spec: WIN32-API-15, PROC-MACH-8
    #[test]
    fn committed_inventory_matches_every_assembled_registration() {
        let expected = include_str!("../../../docs/host-call-boundary.tsv")
            .lines()
            .filter(|line| cfg!(feature = "gpu") || !line.starts_with("d3d9.dll!"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert_eq!(inventory(), expected);
    }
    // spec: A64-CTL-12
    #[test]
    fn eligibility_never_infers_a_leaf_from_a_name() {
        let mut registry = adv_abi::Registry::default();
        registry.add(
            "kernel32.dll",
            "GetTickCount",
            adv_abi::Conv::Stdcall { argc: 0 },
            |_| adv_abi::Outcome::Return(0),
        );
        assert!(
            row(
                "kernel32.dll",
                "GetTickCount",
                registry.get("kernel32.dll", "GetTickCount").unwrap()
            )
            .contains("\tordinary\t")
        );
    }
}
