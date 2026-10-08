//! Metadata fronts translate native structures without cloning file ownership.
use super::*;
use crate::util::set_last_error;

fn failed(env: &mut Env<'_>, status: u32) -> Outcome {
    set_last_error(env, win32_error(status));
    Outcome::Return(0)
}

pub(in crate::kernel32) fn flush(env: &mut Env<'_>, handle: u32) -> Outcome {
    operations::flush(
        env,
        handle,
        Box::new(|env, result| match result {
            Ok(()) => Outcome::Return(1),
            Err(status) => failed(env, status),
        }),
    )
}

pub(in crate::kernel32) fn set_times(env: &mut Env<'_>, handle: u32, inputs: [u32; 3]) -> Outcome {
    let mut bytes = vec![0; 40];
    for (index, input) in inputs.into_iter().enumerate() {
        if input != 0 {
            if check_range(env.cpu, input, 8, Prot::READ).is_err()
                || env
                    .cpu
                    .read(input, &mut bytes[index * 8..index * 8 + 8])
                    .is_err()
            {
                return failed(env, ACCESS_VIOLATION);
            }
        }
    }
    operations::set_information(
        env,
        handle,
        4,
        bytes,
        Box::new(|env, result| match result {
            Ok(()) => Outcome::Return(1),
            Err(status) => failed(env, status),
        }),
    )
}
pub(in crate::kernel32) fn active(env: &mut Env<'_>, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| !matches!(overridden(env, name), Ok(None)))
}

pub(in crate::kernel32) fn times(env: &mut Env<'_>, handle: u32, outputs: [u32; 3]) -> Outcome {
    for output in outputs {
        if output != 0 && check_range(env.cpu, output, 8, Prot::WRITE).is_err() {
            return failed(env, ACCESS_VIOLATION);
        }
    }
    operations::information(
        env,
        handle,
        4,
        40,
        false,
        Box::new(move |env, result| match result {
            Ok(bytes) => {
                // A native callback can change page protection. Revalidate all
                // caller outputs together before publishing any of the times.
                for output in outputs {
                    if output != 0 && check_range(env.cpu, output, 8, Prot::WRITE).is_err() {
                        return failed(env, ACCESS_VIOLATION);
                    }
                }
                for (index, output) in outputs.into_iter().enumerate() {
                    if output != 0
                        && env
                            .cpu
                            .write(output, &bytes[index * 8..index * 8 + 8])
                            .is_err()
                    {
                        return failed(env, ACCESS_VIOLATION);
                    }
                }
                Outcome::Return(1)
            }
            Err(status) => failed(env, status),
        }),
    )
}

pub(in crate::kernel32) fn end(env: &mut Env<'_>, handle: u32) -> Outcome {
    operations::query(
        env,
        handle,
        14,
        Box::new(move |env, result| match result {
            Ok(position) => operations::set_information(
                env,
                handle,
                20,
                position.to_le_bytes().to_vec(),
                Box::new(|env, result| match result {
                    Ok(()) => Outcome::Return(1),
                    Err(status) => failed(env, status),
                }),
            ),
            Err(status) => failed(env, status),
        }),
    )
}

pub(in crate::kernel32) fn attributes(env: &mut Env<'_>, path: &str, flags: u32) -> Outcome {
    let mut bytes = vec![0; 40];
    bytes[32..36].copy_from_slice(&flags.to_le_bytes());
    operations::open(
        env,
        path,
        0x0010_0100,
        7,
        1,
        0x60,
        Box::new(move |env, result| match result {
            Ok(handle) => operations::set_information(
                env,
                handle,
                4,
                bytes,
                Box::new(move |env, set| {
                    operations::close(
                        env,
                        handle,
                        Box::new(move |env, close| match set.and(close) {
                            Ok(()) => Outcome::Return(1),
                            Err(status) => failed(env, status),
                        }),
                    )
                }),
            ),
            Err(status) => failed(env, status),
        }),
    )
}

pub(in crate::kernel32) fn information(env: &mut Env<'_>, handle: u32, output: u32) -> Outcome {
    if output == 0 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    if check_range(env.cpu, output, 52, Prot::WRITE).is_err() {
        return failed(env, ACCESS_VIOLATION);
    }
    let mut bytes = vec![0; 52];
    operations::information(
        env,
        handle,
        4,
        40,
        false,
        Box::new(move |env, basic| {
            let basic = match basic {
                Ok(bytes) => bytes,
                Err(status) => return failed(env, status),
            };
            bytes[..4].copy_from_slice(&basic[32..36]);
            bytes[4..28].copy_from_slice(&basic[..24]);
            operations::information(
                env,
                handle,
                5,
                24,
                false,
                Box::new(move |env, standard| {
                    let standard = match standard {
                        Ok(bytes) => bytes,
                        Err(status) => return failed(env, status),
                    };
                    bytes[32..36].copy_from_slice(&standard[12..16]);
                    bytes[36..40].copy_from_slice(&standard[8..12]);
                    bytes[40..44].copy_from_slice(&standard[16..20]);
                    operations::information(
                        env,
                        handle,
                        6,
                        8,
                        false,
                        Box::new(move |env, internal| {
                            let internal = match internal {
                                Ok(bytes) => bytes,
                                Err(status) => return failed(env, status),
                            };
                            bytes[44..48].copy_from_slice(&internal[4..8]);
                            bytes[48..52].copy_from_slice(&internal[..4]);
                            operations::information(
                                env,
                                handle,
                                1,
                                20,
                                true,
                                Box::new(move |env, volume| {
                                    let volume = match volume {
                                        Ok(bytes) => bytes,
                                        Err(status) => return failed(env, status),
                                    };
                                    bytes[28..32].copy_from_slice(&volume[8..12]);
                                    if check_range(env.cpu, output, 52, Prot::WRITE).is_err()
                                        || env.cpu.write(output, &bytes).is_err()
                                    {
                                        return failed(env, ACCESS_VIOLATION);
                                    }
                                    Outcome::Return(1)
                                }),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}

#[cfg(test)]
mod tests;
