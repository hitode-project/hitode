//! Boundary counters: how often guest code enters the host through each
//! system call, Unix call and server request, and the host time their
//! synchronous part takes. Time spent in guest continuations and waits is
//! not counted; a system call's time includes the requests it sends.

use std::collections::HashMap;
use std::time::Instant;

use adv_machine::Env;

use crate::protocol::REQUEST_NAMES;
use crate::syscalls::SYSCALLS;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub calls: u64,
    pub host_ns: u64,
}

impl Tally {
    pub(crate) fn add(&mut self, since: Instant) {
        self.calls += 1;
        self.host_ns += since.elapsed().as_nanos() as u64;
    }

    fn merge(&mut self, other: Tally) {
        self.calls += other.calls;
        self.host_ns += other.host_ns;
    }
}

/// One process's counters, or a family's sum.
#[derive(Clone, Debug, Default)]
pub struct Boundaries {
    /// By system call number, NT and win32k tables alike.
    pub syscalls: HashMap<u32, Tally>,
    /// By Unix library table and code; ntdll.so's own table is
    /// [`crate::dispatch::NTDLL_UNIXLIB`].
    pub unix: HashMap<(u32, u32), Tally>,
    /// By request number.
    pub server: HashMap<i32, Tally>,
    /// The Unix libraries of the tables in `unix`.
    pub libraries: HashMap<u32, String>,
}

impl Boundaries {
    /// The counters of the machine `ext` belongs to.
    pub fn of(ext: &adv_machine::Ext) -> Self {
        let mut counters = ext.try_get::<Self>().cloned().unwrap_or_default();
        if let Some(link) = ext.try_get::<crate::server::Link>() {
            counters.server = link.requests.clone();
        }
        if let Some(names) = ext.try_get::<crate::unixlib::Names>() {
            counters.libraries = names.0.clone();
        }
        counters
    }

    pub fn merge(&mut self, other: &Self) {
        for (&key, &tally) in &other.syscalls {
            self.syscalls.entry(key).or_default().merge(tally);
        }
        for (&key, &tally) in &other.unix {
            self.unix.entry(key).or_default().merge(tally);
        }
        for (&key, &tally) in &other.server {
            self.server.entry(key).or_default().merge(tally);
        }
        for (&table, name) in &other.libraries {
            self.libraries.entry(table).or_insert_with(|| name.clone());
        }
    }

    /// The `limit` busiest entries of each kind by host time, as JSON.
    pub fn json(&self, limit: usize) -> String {
        fn rows<K>(map: &HashMap<K, Tally>, limit: usize, name: impl Fn(&K) -> String) -> String {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(_, tally)| std::cmp::Reverse((tally.host_ns, tally.calls)));
            let rows: Vec<_> = entries
                .into_iter()
                .take(limit)
                .map(|(key, tally)| {
                    format!(
                        "{{\"name\": \"{}\", \"calls\": {}, \"host_us\": {}}}",
                        name(key),
                        tally.calls,
                        tally.host_ns / 1000
                    )
                })
                .collect();
            let calls: u64 = map.values().map(|tally| tally.calls).sum();
            let host_ns: u64 = map.values().map(|tally| tally.host_ns).sum();
            format!(
                "{{\"calls\": {calls}, \"host_us\": {}, \"top\": [{}]}}",
                host_ns / 1000,
                rows.join(", ")
            )
        }
        format!(
            "{{\"syscalls\": {}, \"unix\": {}, \"server\": {}}}",
            rows(&self.syscalls, limit, |&number| syscall_name(number)),
            rows(&self.unix, limit, |&(table, code)| {
                match self.libraries.get(&table) {
                    _ if table == crate::dispatch::NTDLL_UNIXLIB => format!("ntdll.so:{code}"),
                    Some(name) => format!("{name}:{code}"),
                    None => format!("{table:#x}:{code}"),
                }
            }),
            rows(&self.server, limit, |&number| usize::try_from(number)
                .ok()
                .and_then(|number| REQUEST_NAMES.get(number))
                .map_or_else(
                    || number.to_string(),
                    |name| (*name).to_owned()
                )),
        )
    }
}

fn syscall_name(number: u32) -> String {
    match SYSCALLS.get(number as usize) {
        Some(&(name, _)) => name.to_owned(),
        None => format!("{number:#x}"),
    }
}

pub(crate) fn syscall(env: &mut Env<'_>, number: u32, since: Option<Instant>) {
    let Some(since) = since else { return };
    let counters = env.ext.get_or_default::<Boundaries>();
    counters.syscalls.entry(number).or_default().add(since);
}

pub(crate) fn unix(env: &mut Env<'_>, table: u32, code: u32, since: Option<Instant>) {
    let Some(since) = since else { return };
    let counters = env.ext.get_or_default::<Boundaries>();
    counters.unix.entry((table, code)).or_default().add(since);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_families_rank_by_host_time() {
        let tally = |calls, host_ns| Tally { calls, host_ns };
        let mut family = Boundaries::default();
        family.syscalls.insert(0, tally(1, 5_000));
        let mut child = Boundaries::default();
        child.syscalls.insert(0, tally(2, 1_000));
        child.syscalls.insert(1, tally(9, 9_000));
        child
            .unix
            .insert((crate::dispatch::NTDLL_UNIXLIB, 3), tally(4, 2_000));
        child.unix.insert((0x5000, 1), tally(1, 1_000));
        child.libraries.insert(0x5000, "win32u.so".into());
        child.server.insert(0, tally(1, 3_000));
        family.merge(&child);
        assert_eq!(family.syscalls[&0], tally(3, 6_000));
        let first = SYSCALLS[1].0;
        assert_eq!(
            family.json(1),
            format!(
                "{{\"syscalls\": {{\"calls\": 12, \"host_us\": 15, \"top\": [{{\"name\": \"{first}\", \"calls\": 9, \"host_us\": 9}}]}}, \
\"unix\": {{\"calls\": 5, \"host_us\": 3, \"top\": [{{\"name\": \"ntdll.so:3\", \"calls\": 4, \"host_us\": 2}}]}}, \
\"server\": {{\"calls\": 1, \"host_us\": 3, \"top\": [{{\"name\": \"new_process\", \"calls\": 1, \"host_us\": 3}}]}}}}"
            )
        );
        assert!(family.json(2).contains("\"win32u.so:1\""));
    }
}
