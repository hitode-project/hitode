//! Wine's system APCs run in the target process, through its ordinary NT services.
use crate::{
    memory,
    object::{read_u32, write_bytes, write_u32},
    protocol, section, server,
    status::{self, Error},
    thread,
};
use adv_cpu::backend::Reg;
use adv_machine::{Env, Outcome};
fn get(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn outcome(out: Outcome) -> Result<u32, Error> {
    match out {
        Outcome::Return(code) => Ok(code),
        Outcome::Fail(e) => Err(Error::Unsupported(e)),
        _ => Err(Error::Unsupported(
            "asynchronous system APC operation".into(),
        )),
    }
}

pub(crate) fn invoke(env: &mut Env<'_>, call: &[u8], own: bool) -> Result<[u8; 40], Error> {
    let kind = get(call, 0);
    if kind == 2 {
        return crate::file::complete_async(env, call.try_into().unwrap());
    }
    let scratch = env.cpu.reg(Reg::Esp) - 512;
    let mut saved = [0; 512];
    env.cpu
        .read(scratch, &mut saved)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    let operation = (|| {
        let mut result = [0; 40];
        put(&mut result, 0, kind);
        write_u32(env, scratch, get(call, 8))?;
        write_u32(env, scratch + 4, get(call, 16))?;
        write_bytes(env, scratch + 16, &call[24..32])?;
        let code = match kind {
            0 => 0,
            3 => outcome(memory::allocate(
                env,
                &[
                    u32::MAX,
                    scratch,
                    get(call, 24),
                    scratch + 4,
                    get(call, 4),
                    get(call, 32),
                ],
            ))?,
            4 => {
                // Address requirements use the same extended-parameter parser as local calls.
                write_bytes(
                    env,
                    scratch + 64,
                    &[
                        1u32.to_le_bytes(),
                        0u32.to_le_bytes(),
                        (scratch + 96).to_le_bytes(),
                        0u32.to_le_bytes(),
                    ]
                    .concat(),
                )?;
                for (to, from) in [(96, 24), (100, 32), (104, 40)] {
                    write_u32(env, scratch + to, get(call, from))?;
                }
                outcome(memory::allocate_ex(
                    env,
                    &[
                        u32::MAX,
                        scratch,
                        scratch + 4,
                        get(call, 4),
                        get(call, 48),
                        scratch + 64,
                        1,
                    ],
                ))?
            }
            5 => outcome(memory::free(
                env,
                &[u32::MAX, scratch, scratch + 4, get(call, 4)],
            ))?,
            6 => outcome(memory::query(
                env,
                &[u32::MAX, get(call, 8), 0, scratch + 64, 28, 0],
            ))?,
            7 => outcome(memory::protect(
                env,
                &[u32::MAX, scratch, scratch + 4, get(call, 4), scratch + 8],
            ))?,
            8 => outcome(memory::flush_virtual(
                env,
                &[u32::MAX, scratch, scratch + 4, 0],
            ))?,
            9 => outcome(memory::lock_virtual(
                env,
                &[u32::MAX, scratch, scratch + 4, 0],
            ))?,
            10 => outcome(memory::unlock_virtual(
                env,
                &[u32::MAX, scratch, scratch + 4, 0],
            ))?,
            11 => outcome(section::map_view(
                env,
                &[
                    get(call, 4),
                    u32::MAX,
                    scratch,
                    get(call, 32),
                    0,
                    scratch + 16,
                    scratch + 4,
                    0,
                    get(call, 40),
                    get(call, 44),
                ],
            ))?,
            12 => outcome(section::map_view_ex(
                env,
                &[
                    get(call, 4),
                    u32::MAX,
                    scratch,
                    scratch + 16,
                    scratch + 4,
                    get(call, 48),
                    get(call, 52),
                    0,
                    0,
                ],
            ))?,
            13 => outcome(section::unmap_view_ex(
                env,
                &[u32::MAX, get(call, 8), get(call, 4)],
            ))?,
            14 => outcome(thread::create(
                env,
                &[
                    scratch + 8,
                    0x1fffff,
                    0,
                    u32::MAX,
                    get(call, 8),
                    get(call, 16),
                    get(call, 4),
                    get(call, 24),
                    get(call, 40),
                    get(call, 32),
                    0,
                ],
            ))?,
            15 => outcome(crate::object::duplicate(
                env,
                &[
                    u32::MAX,
                    get(call, 4),
                    get(call, 8),
                    scratch + 8,
                    get(call, 12),
                    get(call, 16),
                    get(call, 20),
                ],
            ))?,
            _ => return Err(Error::Unsupported(format!("system APC type {kind}"))),
        };
        put(&mut result, 4, code);
        if kind == 6 && code == 0 {
            let mut mbi = [0; 28];
            env.cpu
                .read(scratch + 64, &mut mbi)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            for (to, from) in [(8, 0), (16, 4), (24, 12)] {
                put(&mut result, to, get(&mbi, from));
            }
            for (at, v) in [
                (32, (get(&mbi, 16) >> 12) as u16),
                (34, get(&mbi, 20) as u16),
                (36, get(&mbi, 8) as u16),
                (38, (get(&mbi, 24) >> 16) as u16),
            ] {
                result[at..at + 2].copy_from_slice(&v.to_le_bytes());
            }
        } else if kind == 14 && code == 0 {
            let handle = read_u32(env, scratch + 8)?;
            let mut info = protocol::GetThreadInfoRequest {
                handle,
                access: 0,
                ..Default::default()
            };
            let (info, _) = server::request(env, &mut info, &[], &mut [])?;
            status::check(info.header.error)?;
            put(&mut result, 8, info.pid);
            put(&mut result, 12, info.tid);
            put(&mut result, 16, info.teb as u32);
            put(&mut result, 24, handle);
        } else if kind == 15 {
            put(&mut result, 8, read_u32(env, scratch + 8)?);
        } else {
            put(&mut result, 8, read_u32(env, scratch)?);
            put(&mut result, 16, read_u32(env, scratch + 4)?);
            if kind == 7 {
                put(&mut result, 24, read_u32(env, scratch + 8)?);
            }
        }
        if !own && matches!(kind, 11 | 12 | 15) {
            server::close(env, get(call, if kind == 15 { 8 } else { 4 }))?;
        }
        Ok(result)
    })();
    env.cpu
        .write(scratch, &saved)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    operation
}

type Then = Box<dyn FnOnce(&mut Env<'_>, [u8; 40]) -> Outcome>;
pub(crate) fn queue(env: &mut Env<'_>, process: u32, call: [u8; 64], then: Then) -> Outcome {
    let mut request = protocol::QueueApcRequest {
        handle: process,
        ..Default::default()
    };
    let reply = server::request(env, &mut request, &[&call], &mut []);
    let reply = match reply {
        Ok((r, _)) if r.header.error == 0 => r,
        Ok((r, _)) => return Outcome::Return(r.header.error),
        Err(e) => return status::finish(Err(e)),
    };
    if reply.self_ != 0 {
        return match invoke(env, &call, true) {
            Ok(result) => then(env, result),
            Err(e) => status::finish(Err(e)),
        };
    }
    crate::sync::wait_then(env, reply.handle, move |env, code| {
        if code != 0 {
            return Outcome::Return(code);
        }
        let mut request = protocol::GetApcResultRequest {
            handle: reply.handle,
            ..Default::default()
        };
        match server::request(env, &mut request, &[], &mut []) {
            Ok((reply, _)) if reply.header.error == 0 => then(env, reply.result),
            Ok((reply, _)) => Outcome::Return(reply.header.error),
            Err(e) => status::finish(Err(e)),
        }
    })
}

pub(crate) fn memory(env: &mut Env<'_>, args: &[u32], kind: u32) -> Outcome {
    let result = (|| {
        let mut call = [0; 64];
        put(&mut call, 0, kind);
        match kind {
            3 => {
                put(&mut call, 4, args[4]);
                put(&mut call, 8, read_u32(env, args[1])?);
                put(&mut call, 16, read_u32(env, args[3])?);
                put(&mut call, 24, args[2]);
                put(&mut call, 32, args[5]);
            }
            5 | 7 | 8 | 9 | 10 => {
                put(&mut call, 8, read_u32(env, args[1])?);
                put(&mut call, 16, read_u32(env, args[2])?);
                if kind == 5 || kind == 7 {
                    put(&mut call, 4, args[3]);
                }
            }
            6 => {
                if args[2] != 0 {
                    return Err(status::INVALID_INFO_CLASS.into());
                }
                put(&mut call, 8, args[1]);
            }
            _ => return Err(status::NOT_SUPPORTED.into()),
        }
        Ok(call)
    })();
    let call = match result {
        Ok(call) => call,
        Err(e) => return status::finish(Err(e)),
    };
    let args = args.to_vec();
    queue(
        env,
        args[0],
        call,
        Box::new(move |env, result| {
            status::finish((|| {
                status::check(get(&result, 4))?;
                if kind == 6 {
                    let mut mbi = [0; 28];
                    for (to, from) in [(0, 8), (4, 16), (12, 24)] {
                        put(&mut mbi, to, get(&result, from));
                    }
                    for (to, from, shift) in [(16, 32, 12), (20, 34, 0), (8, 36, 0), (24, 38, 16)] {
                        put(
                            &mut mbi,
                            to,
                            (u16::from_le_bytes(result[from..from + 2].try_into().unwrap()) as u32)
                                << shift,
                        );
                    }
                    if args[4] < 28 {
                        return Err(status::INFO_LENGTH_MISMATCH.into());
                    }
                    write_bytes(env, args[3], &mbi)?;
                    if args[5] != 0 {
                        write_u32(env, args[5], 28)?;
                    }
                } else {
                    write_u32(env, args[1], get(&result, 8))?;
                    write_u32(env, args[if kind == 3 { 3 } else { 2 }], get(&result, 16))?;
                    if kind == 7 {
                        write_u32(env, args[4], get(&result, 24))?;
                    }
                }
                Ok(())
            })())
        }),
    )
}

pub(crate) fn create_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let mut call = [0; 64];
    put(&mut call, 0, 14);
    for (at, index) in [(4, 6), (8, 4), (16, 5), (24, 7), (32, 9), (40, 8)] {
        put(&mut call, at, args[index]);
    }
    let args = args.to_vec();
    queue(
        env,
        args[3],
        call,
        Box::new(move |env, result| {
            status::finish((|| {
                status::check(get(&result, 4))?;
                write_u32(env, args[0], get(&result, 24))?;
                if args[10] != 0 {
                    let count = read_u32(env, args[10])?.saturating_sub(4) / 16;
                    for index in 0..count {
                        let at = args[10] + 4 + index * 16;
                        let bytes = match read_u32(env, at)? {
                            0x10003 => result[8..16].to_vec(),
                            0x10004 => result[16..20].to_vec(),
                            _ => continue,
                        };
                        let pointer = read_u32(env, at + 8)?;
                        write_bytes(env, pointer, &bytes)?;
                    }
                }
                Ok(())
            })())
        }),
    )
}

pub(crate) fn map(env: &mut Env<'_>, args: &[u32], extended: bool) -> Outcome {
    let prepared = (|| {
        let mut call = [0; 64];
        put(&mut call, 0, if extended { 12 } else { 11 });
        put(&mut call, 4, args[0]);
        put(&mut call, 8, read_u32(env, args[2])?);
        let (offset, size, kind, prot) = if extended {
            (args[3], args[4], args[5], args[6])
        } else {
            (args[5], args[6], args[8], args[9])
        };
        put(&mut call, 16, read_u32(env, size)?);
        if offset != 0 {
            env.cpu
                .read(offset, &mut call[24..32])
                .map_err(|_| status::ACCESS_VIOLATION)?;
        }
        put(&mut call, if extended { 48 } else { 40 }, kind);
        put(&mut call, if extended { 52 } else { 44 }, prot);
        if extended {
            for index in 0..args[8] {
                let at = args[7] + index * 16;
                let ty = read_u32(env, at)? & 0xff;
                let value = read_u32(env, at + 8)?;
                match ty {
                    1 => {
                        put(&mut call, 32, read_u32(env, value)?);
                        put(&mut call, 40, read_u32(env, value + 4)?);
                    }
                    6 => put(&mut call, 56, value),
                    _ => return Err(status::INVALID_PARAMETER.into()),
                }
            }
        } else {
            put(&mut call, 32, args[3]);
        }
        Ok(call)
    })();
    let call = match prepared {
        Ok(call) => call,
        Err(e) => return status::finish(Err(e)),
    };
    let args = args.to_vec();
    queue(
        env,
        args[1],
        call,
        Box::new(move |env, result| {
            status::finish((|| {
                let code = get(&result, 4);
                if code != 0 && code != 0x40000003 {
                    return Err(code.into());
                }
                write_u32(env, args[2], get(&result, 8))?;
                write_u32(env, args[if extended { 4 } else { 6 }], get(&result, 16))?;
                status::check(code)
            })())
        }),
    )
}

pub(crate) fn unmap(env: &mut Env<'_>, args: &[u32], extended: bool) -> Outcome {
    let mut call = [0; 64];
    put(&mut call, 0, 13);
    put(&mut call, 8, args[1]);
    if extended {
        put(&mut call, 4, args[2]);
    }
    queue(
        env,
        args[0],
        call,
        Box::new(|_, result| Outcome::Return(get(&result, 4))),
    )
}
