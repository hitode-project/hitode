//! Whole-section and checksummed-structure queries share the ordinary INI reader.
use super::*;
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
pub(super) fn section_query(env: &mut Env<'_>, wide: bool, private: bool) -> Outcome {
    let (app, out, capacity, filename) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        if private { env.arg(3) } else { 0 },
    );
    let Some(length) = capacity.checked_mul(if wide { 2 } else { 1 }) else {
        return fail(env, 998);
    };
    if check_range(env.cpu, out, length, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let (Ok(Some(app)), Ok(filename)) = (text(env, app, wide), text(env, filename, wide)) else {
        return fail(env, 87);
    };
    let filename = match path(filename) {
        Ok(value) => value,
        Err(_) => return fail(env, 1113),
    };
    with_contents(
        env,
        filename,
        Box::new(move |env, result| {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(ReadError::Io(error)) => {
                    return output(
                        env,
                        Answer::List(Vec::new()),
                        capacity,
                        out,
                        wide,
                        Some(error),
                    );
                }
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(
                        env,
                        "kernel32.dll!GetPrivateProfileSection",
                        reason,
                        error,
                    );
                }
            };
            let units = match decode(env, &bytes) {
                Ok(units) => units,
                Err(ReadError::Io(error)) => return fail(env, error),
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(
                        env,
                        "kernel32.dll!GetPrivateProfileSection",
                        reason,
                        error,
                    );
                }
            };
            let ini = Ini::new(&units);
            let items = ini
                .bounds(&app)
                .map(|(begin, end)| {
                    ini.lines[begin + 1..end]
                        .iter()
                        .filter_map(|line| {
                            let line = trim(line);
                            (line.first() != Some(&59) && line.contains(&61)).then(|| line.to_vec())
                        })
                        .collect()
                })
                .unwrap_or_default();
            output(env, Answer::List(items), capacity, out, wide, None)
        }),
    )
}
fn nibble(unit: u16) -> Option<u8> {
    Some(match unit {
        48..=57 => (unit - 48) as u8,
        65..=70 => (unit - 65 + 10) as u8,
        97..=102 => (unit - 97 + 10) as u8,
        _ => return None,
    })
}
pub(super) fn structure(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (output, length) = (env.arg(2), env.arg(3));
    let Some(encoded_length) = length
        .checked_add(1)
        .and_then(|length| length.checked_mul(2))
    else {
        return fail(env, 87);
    };
    if check_range(env.cpu, output, length, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let names = [env.arg(0), env.arg(1), env.arg(4)];
    let [Ok(Some(app)), Ok(Some(key)), Ok(filename)] =
        names.map(|pointer| text(env, pointer, wide))
    else {
        return fail(env, 87);
    };
    let filename = match path(filename) {
        Ok(value) => value,
        Err(_) => return fail(env, 1113),
    };
    with_contents(
        env,
        filename,
        Box::new(move |env, result| {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(ReadError::Io(error)) => return fail(env, error),
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(env, "kernel32.dll!GetPrivateProfileStruct", reason, error);
                }
            };
            let units = match decode(env, &bytes) {
                Ok(units) => units,
                Err(ReadError::Io(error)) => return fail(env, error),
                Err(ReadError::Unsupported(reason, error)) => {
                    return unavailable(env, "kernel32.dll!GetPrivateProfileStruct", reason, error);
                }
            };
            let Some(Answer::Scalar(value)) = Ini::new(&units).query(Some(&app), Some(&key)) else {
                return fail(env, 2);
            };
            if value.len() != encoded_length as usize {
                return fail(env, 13);
            }
            let mut bytes = Vec::new();
            for pair in value.as_chunks::<2>().0 {
                let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
                    return fail(env, 13);
                };
                bytes.push(high * 16 + low);
            }
            let checksum = bytes.pop().unwrap();
            if bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) != checksum {
                return fail(env, 13);
            }
            if check_range(env.cpu, output, length, Prot::WRITE).is_err()
                || env.cpu.write(output, &bytes).is_err()
            {
                return fail(env, 998);
            }
            set_last_error(env, 0);
            Outcome::Return(1)
        }),
    )
}
pub(super) fn register(registry: &mut Registry) {
    for (name, handler) in [
        (
            "GetPrivateProfileSectionA",
            (|env: &mut Env<'_>| section_query(env, false, true)) as adv_machine::Handler,
        ),
        ("GetPrivateProfileSectionW", |env| {
            section_query(env, true, true)
        }),
        ("GetProfileSectionA", |env| section_query(env, false, false)),
        ("GetProfileSectionW", |env| section_query(env, true, false)),
        ("GetPrivateProfileStructA", |env| structure(env, false)),
        ("GetPrivateProfileStructW", |env| structure(env, true)),
        ("WriteProfileStringA", |env| {
            write::string(env, false, false)
        }),
        ("WriteProfileStringW", |env| write::string(env, true, false)),
        ("WritePrivateProfileSectionA", |env| {
            write::section_write(env, false, true)
        }),
        ("WritePrivateProfileSectionW", |env| {
            write::section_write(env, true, true)
        }),
        ("WriteProfileSectionA", |env| {
            write::section_write(env, false, false)
        }),
        ("WriteProfileSectionW", |env| {
            write::section_write(env, true, false)
        }),
        ("WritePrivateProfileStructA", |env| {
            write::structure(env, false)
        }),
        ("WritePrivateProfileStructW", |env| {
            write::structure(env, true)
        }),
    ] {
        registry.add(
            K32,
            name,
            adv_abi::signature::lookup(K32, name).unwrap().conv.unwrap(),
            handler,
        );
    }
}
