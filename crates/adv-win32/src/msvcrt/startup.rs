//! Legacy CRT main arguments share Win32's command line and environment owners.
use super::*;
use adv_abi::{AnsiStr, GuestPtr, WideStr, guest::check_range, win32};
use adv_cpu::backend::{MemFault, Prot};

const NARROW: &str = "_mainargs_a";
const WIDE: &str = "_mainargs_w";
const HEADER_BYTES: u32 = 20;
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn register(reg: &mut Registry) {
    // UCRT uses its configure/initialize startup exports, not these legacy APIs.
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for api in [getmainargs_ABI, wgetmainargs_ABI] {
            reg.register(adv_abi::Api { module, ..api });
        }
    }
}

/// Microsoft CRT rules: argv[0] treats quotes as grouping, later arguments
/// interpret backslashes before quotes and doubled quotes inside quoted text.
fn parse(line: &[u16]) -> Vec<Vec<u16>> {
    let mut at = 0;
    let mut program = Vec::new();
    let mut quoted = false;
    while at < line.len() {
        let unit = line[at];
        if unit == u16::from(b'"') {
            quoted = !quoted;
        } else if !quoted && matches!(unit, 9 | 32) {
            break;
        } else {
            program.push(unit);
        }
        at += 1;
    }
    let mut args = vec![program];
    loop {
        while at < line.len() && matches!(line[at], 9 | 32) {
            at += 1;
        }
        if at == line.len() {
            return args;
        }
        let mut arg = Vec::new();
        quoted = false;
        while at < line.len() && (quoted || !matches!(line[at], 9 | 32)) {
            let mut slashes = 0;
            while at < line.len() && line[at] == u16::from(b'\\') {
                slashes += 1;
                at += 1;
            }
            if at < line.len() && line[at] == u16::from(b'"') {
                arg.extend(std::iter::repeat_n(u16::from(b'\\'), slashes / 2));
                if slashes % 2 != 0 {
                    arg.push(u16::from(b'"'));
                } else if quoted && line.get(at + 1) == Some(&u16::from(b'"')) {
                    arg.push(u16::from(b'"'));
                    at += 1;
                } else {
                    quoted = !quoted;
                }
                at += 1;
            } else {
                arg.extend(std::iter::repeat_n(u16::from(b'\\'), slashes));
                if at == line.len() || (!quoted && matches!(line[at], 9 | 32)) {
                    break;
                }
                arg.push(line[at]);
                at += 1;
            }
        }
        args.push(arg);
    }
}

fn error(env: &mut Env<'_>, code: i32) -> Result<i32, MemFault> {
    let errno = floating::errno_address(env);
    errno.write(env.cpu, code)?;
    Ok(-1)
}

fn encoded(env: &Env<'_>, units: &[u16], wide: bool) -> Vec<u8> {
    if wide {
        units
            .iter()
            .copied()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut bytes = env.ext.get::<crate::Win32>().codec.from_utf16(0, units).0;
        bytes.push(0);
        bytes
    }
}

fn mainargs(
    env: &mut Env<'_>,
    argc: GuestPtr<i32>,
    argv: GuestPtr<u32>,
    environment: GuestPtr<u32>,
    wildcard: i32,
    startup: GuestPtr<i32>,
    wide: bool,
) -> Result<i32, MemFault> {
    // Check every published output before allocating or changing CRT state.
    for output in [argc.addr(), argv.addr(), environment.addr()] {
        check_range(env.cpu, output, 4, Prot::WRITE)?;
    }
    let newmode = startup.read(env.cpu)?;
    env.ext.get_or_default::<CrtState>();
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let key = (module, if wide { WIDE } else { NARROW }.to_owned());
    let existing = env.ext.get_mut::<CrtState>().globals.get(&key).copied();
    let pointer = if let Some(pointer) = existing {
        pointer
    } else {
        let command = crate::kernel32::command_line(env, wide);
        if command == 0 {
            return error(env, 12); // ENOMEM
        }
        let line = if wide {
            WideStr(GuestPtr::new(command)).read(env.cpu, 32_768)?
        } else {
            let bytes = AnsiStr(GuestPtr::new(command)).read(env.cpu, 32_768 * 4)?;
            env.ext.get::<crate::Win32>().codec.to_utf16(0, &bytes)
        };
        let args = parse(&line);
        let has_glob = args
            .iter()
            .skip(1)
            .any(|arg| arg.iter().any(|unit| matches!(*unit, 42 | 63)));
        if wildcard != 0 && has_glob {
            // No VFS glob expansion is implemented. Do not advertise an
            // unexpanded vector as the requested expanded result.
            return error(env, 40); // ENOSYS
        }
        let entries = crate::kernel32::environment_entries(env);
        let count = args.len();
        let env_count = entries.len();
        let strings: Vec<_> = args
            .iter()
            .chain(&entries)
            .map(|v| encoded(env, v, wide))
            .collect();
        let argv_offset = HEADER_BYTES as usize;
        let env_offset = argv_offset + (count + 1) * 4;
        let string_offset = env_offset + (env_count + 1) * 4;
        let length = strings
            .iter()
            .try_fold(string_offset, |len, bytes| len.checked_add(bytes.len()));
        let Some(length) = length.filter(|&length| length <= MAX_BYTES) else {
            return error(env, 12);
        };
        let pointer = allocate(env, length as u32, true);
        if pointer == 0 {
            return error(env, 12);
        }
        let mut bytes = vec![0; length];
        let arg_array = pointer + argv_offset as u32;
        let env_array = pointer + env_offset as u32;
        for (index, value) in [
            count as u32,
            arg_array,
            env_array,
            newmode as u32,
            u32::from(has_glob),
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut cursor = string_offset;
        for (index, string) in strings.iter().enumerate() {
            let slot = if index < count {
                argv_offset + index * 4
            } else {
                env_offset + (index - count) * 4
            };
            bytes[slot..slot + 4].copy_from_slice(&(pointer + cursor as u32).to_le_bytes());
            bytes[cursor..cursor + string.len()].copy_from_slice(string);
            cursor += string.len();
        }
        if let Err(fault) = env.cpu.write(pointer, &bytes) {
            let heaps = env.ext.get_mut::<Heaps>();
            heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
            return Err(fault);
        }
        env.ext.get_mut::<CrtState>().globals.insert(key, pointer);
        pointer
    };
    let header = GuestPtr::<[u32; 5]>::new(pointer).read(env.cpu)?;
    if wildcard != 0 && header[4] != 0 {
        return error(env, 40);
    }
    argc.write(env.cpu, header[0] as i32)?;
    argv.write(env.cpu, header[1])?;
    environment.write(env.cpu, header[2])?;
    Ok(0)
}

#[win32(msvcr90, name = "__getmainargs", level = implement)]
fn getmainargs(
    env: &mut Env<'_>,
    argc: GuestPtr<i32>,
    argv: GuestPtr<u32>,
    environment: GuestPtr<u32>,
    wildcard: i32,
    startup: GuestPtr<i32>,
) -> Result<i32, MemFault> {
    mainargs(env, argc, argv, environment, wildcard, startup, false)
}
#[win32(msvcr90, name = "__wgetmainargs", level = implement)]
fn wgetmainargs(
    env: &mut Env<'_>,
    argc: GuestPtr<i32>,
    argv: GuestPtr<u32>,
    environment: GuestPtr<u32>,
    wildcard: i32,
    startup: GuestPtr<i32>,
) -> Result<i32, MemFault> {
    mainargs(env, argc, argv, environment, wildcard, startup, true)
}

pub(super) fn release_process_tables(env: &mut Env<'_>) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let tables: Vec<_> = state
        .globals
        .iter()
        .filter(|((_, name), _)| matches!(name.as_str(), NARROW | WIDE))
        .map(|(key, &address)| (key.clone(), address))
        .collect();
    for (key, pointer) in tables {
        let heaps = env.ext.get_mut::<Heaps>();
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer) {
            return Err(format!(
                "CRT startup detach: invalid heap block {pointer:#x}"
            ));
        }
        env.ext.get_mut::<CrtState>().globals.remove(&key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{
        Clock, Machine,
        layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
    };

    #[test]
    fn legacy_startup_snapshots_are_per_crt_and_released_idempotently() {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        super::super::register(&mut reg);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(
                "/nonexistent/args-release",
                "fixture.exe",
                "/nonexistent/args-release-data",
            ),
        )
        .unwrap();
        let data = 0x410000;
        machine.cpu.map(data, 4096, Prot::RW).unwrap();
        machine.cpu.write_u32(data + 16, 0).unwrap();
        machine
            .spawn("CRT startup cleanup", PARK_ADDR, &[], 65536)
            .unwrap();
        for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
            for wide in [false, true] {
                let entry = machine
                    .resolve_import(
                        module,
                        if wide {
                            "__wgetmainargs"
                        } else {
                            "__getmainargs"
                        },
                    )
                    .unwrap();
                let mut env = Env {
                    context: machine.sys.current,
                    cpu: &mut machine.cpu,
                    sys: &mut machine.sys,
                    ext: &mut machine.ext,
                    thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
                };
                assert_eq!(
                    mainargs(
                        &mut env,
                        GuestPtr::new(data),
                        GuestPtr::new(data + 4),
                        GuestPtr::new(data + 8),
                        0,
                        GuestPtr::new(data + 16),
                        wide
                    )
                    .unwrap(),
                    0
                );
            }
        }
        let allocations: Vec<_> = machine
            .ext
            .get::<CrtState>()
            .globals
            .iter()
            .filter(|((_, name), _)| matches!(name.as_str(), NARROW | WIDE))
            .map(|(_, &pointer)| pointer)
            .collect();
        assert_eq!(allocations.len(), 6);
        for &pointer in &allocations {
            assert!(machine.ext.get::<Heaps>().owner(pointer).is_some());
        }
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        super::super::detach_process(&mut env).unwrap();
        super::super::detach_process(&mut env).unwrap();
        for pointer in allocations {
            assert!(env.ext.get::<Heaps>().owner(pointer).is_none());
        }
        assert!(env.ext.get::<CrtState>().globals.is_empty());
    }

    #[test]
    fn microsoft_quote_and_backslash_rules_preserve_empty_and_utf16_arguments() {
        let cases = [
            (
                r#""C:\Game Path\fixture.exe" "a b c" d e"#,
                vec![r"C:\Game Path\fixture.exe", "a b c", "d", "e"],
            ),
            (
                r#"program "ab\"c" "\\" d"#,
                vec!["program", "ab\"c", "\\", "d"],
            ),
            (
                r#"program a\\\b d"e f"g h"#,
                vec!["program", r"a\\\b", "de fg", "h"],
            ),
            (r#"program a\\\"b c d"#, vec!["program", "a\\\"b", "c", "d"]),
            (
                r#"program a\\\\"b c" d e"#,
                vec!["program", r"a\\b c", "d", "e"],
            ),
            (r#"program a"b"" c d"#, vec!["program", "ab\" c d"]),
            ("program \"\" \"日本語\"", vec!["program", "", "日本語"]),
        ];
        for (line, expected) in cases {
            assert_eq!(
                parse(&line.encode_utf16().collect::<Vec<_>>()),
                expected
                    .iter()
                    .map(|v| v.encode_utf16().collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            );
        }
    }
}
