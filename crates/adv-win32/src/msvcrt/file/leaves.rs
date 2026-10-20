//! Buffer-only stdio calls decline before memory or stream changes.
use super::*;

pub(in crate::msvcrt) fn register(registry: &mut Registry) {
    for module in [
        "msvcrt.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "ucrtbase.dll",
        "api-ms-win-crt-stdio-l1-1-0.dll",
    ] {
        for name in ["fgetc", "getc", "fread", "ftell", "fgetpos", "feof"] {
            if registry.is_implemented(module, name) {
                registry.set_env_leaf(module, name, call);
            }
        }
    }
}

fn call(env: &mut Env<'_>) -> Option<u64> {
    let thunk = env.sys.thunks.get(env.thunk);
    let name = thunk.name.as_str();
    let modern = matches!(
        thunk.module.as_str(),
        "ucrtbase.dll" | "api-ms-win-crt-stdio-l1-1-0.dll"
    );
    let reading = name == "fread";
    let (size, count) = if reading {
        (env.arg(1), env.arg(2))
    } else {
        (1, 1)
    };
    if size == 0 || count == 0 {
        return Some(0);
    }
    let length = size.checked_mul(count)?;
    let stream = env.arg(if reading { 3 } else { 0 });
    let file = env.ext.try_get::<CrtState>()?.files.get(&stream)?;
    match name {
        "feof" => Some(u64::from(file.eof)),
        "ftell" | "fgetpos" => {
            if !crate::native::known_unmodified(env, "NtReadFile") {
                return None;
            }
            let position = file.position;
            if name == "ftell" {
                if modern && (!file.binary || position > i32::MAX as u64) {
                    return None;
                }
                return Some(u64::from(position as u32));
            }
            if stream == 0 || position > i64::MAX as u64 {
                return None;
            }
            let output = env.arg(1);
            if output == 0 || check_range(env.cpu, output, 8, Prot::WRITE).is_err() {
                return None;
            }
            env.cpu.write(output, &position.to_le_bytes()).unwrap();
            Some(0)
        }
        "fread" | "fgetc" | "getc" => {
            let character = !reading;
            if length as usize > file.read_end - file.read_cursor
                || (character || modern) && !file.binary
                || modern && length > 16 * 1024 * 1024
                || !crate::native::known_unmodified(env, "NtReadFile")
            {
                return None;
            }
            let output = if reading { env.arg(0) } else { stream };
            if check_range(
                env.cpu,
                output,
                if character { 32 } else { length },
                if character { Prot::RW } else { Prot::WRITE },
            )
            .is_err()
            {
                return None;
            }
            let result = if character {
                u64::from(file.read_buffer[file.read_cursor])
            } else {
                u64::from(count)
            };
            env.cpu
                .write(
                    output,
                    &file.read_buffer[file.read_cursor..file.read_cursor + length as usize],
                )
                .unwrap();
            let file = env
                .ext
                .get_mut::<CrtState>()
                .files
                .get_mut(&stream)
                .unwrap();
            file.read_cursor += length as usize;
            file.position += u64::from(length);
            Some(result)
        }
        _ => unreachable!(),
    }
}
