//! Element ownership follows the existing BSTR allocator and guest IUnknown methods.
use super::*;
use crate::misc::variant;
use adv_os::com::{MAX_BSTR_BYTES, copy_bstr, free_bstr, owns_bstr};
const LOCKED: u32 = 0x8002000d;
pub(in crate::misc) fn destroy_then(env: &mut Env<'_>, array: u32, then: Done<()>) -> Outcome {
    if array == 0 {
        return then(env, Ok(()));
    }
    let (view, allocation) = match checked_owned(env, array) {
        Ok(value) => value,
        Err(error) => return then(env, Err(error)),
    };
    if env.cpu.read_u32(array + LOCKS).ok() != Some(0) {
        return then(env, Err(LOCKED));
    }
    if !matches!(allocation.kind, 8 | 9 | 12 | 13) {
        retire(env, array);
        return then(env, Ok(()));
    }
    clear_next(env, array, view, allocation.kind, 0, then)
}
fn clear_next(
    env: &mut Env<'_>,
    array: u32,
    view: View,
    kind: u16,
    mut index: u32,
    then: Done<()>,
) -> Outcome {
    loop {
        if index == view.count {
            retire(env, array);
            return then(env, Ok(()));
        }
        let address = view.data + index * view.element;
        if kind == 12 {
            let outcome = variant::clear_value(env, address);
            match outcome {
                Outcome::Return(0) => {
                    index += 1;
                    continue;
                }
                Outcome::Return(error) => return then(env, Err(error)),
                pending => {
                    return variant::complete(
                        env,
                        pending,
                        Box::new(move |env, status| {
                            if status == 0 {
                                clear_next(env, array, view, kind, index + 1, then)
                            } else {
                                then(env, Err(status))
                            }
                        }),
                    );
                }
            }
        }
        let pointer = env.cpu.read_u32(address).unwrap();
        if kind == 8 {
            if !free_bstr(env, pointer) {
                capability(env, "array BSTR is not owned by the shared allocator");
                return then(env, Err(INVALID));
            }
        } else if pointer != 0 {
            let target = match variant::method(env, pointer, 2) {
                Ok(target) => target,
                Err(error) => return then(env, Err(error)),
            };
            return Outcome::call(target, [pointer], move |env, _| {
                if env.cpu.write_u32(address, 0).is_err() {
                    return then(env, Err(INVALID));
                }
                clear_next(env, array, view, kind, index + 1, then)
            });
        }
        let _ = env.cpu.write_u32(address, 0);
        index += 1;
    }
}
pub(in crate::misc) fn clone_then(env: &mut Env<'_>, array: u32, then: Done<u32>) -> Outcome {
    if array == 0 {
        return then(env, Ok(0));
    }
    let (source, allocation) = match checked_owned(env, array) {
        Ok(value) => value,
        Err(error) => return then(env, Err(error)),
    };
    let status = super::super::adjust(env, array, 1);
    if status != 0 {
        return then(env, Err(status as u32));
    }
    let finish: Done<u32> = Box::new(move |env, result| {
        super::super::adjust(env, array, -1);
        then(env, result)
    });
    let bounds = source.bounds.iter().rev().copied().collect::<Vec<_>>();
    let destination = alloc(env, allocation.kind, &bounds);
    if destination == 0 {
        return finish(env, Err(MEMORY));
    }
    let target = env.cpu.read_u32(destination + DATA).unwrap();
    if !matches!(allocation.kind, 8 | 9 | 12 | 13) {
        if source.bytes != 0
            && env
                .cpu
                .copy_memory(source.data, target, source.bytes as usize)
                .is_err()
        {
            retire(env, destination);
            return finish(env, Err(INVALID));
        }
        return finish(env, Ok(destination));
    }
    copy_next(env, source, allocation.kind, destination, target, 0, finish)
}
fn failed(env: &mut Env<'_>, destination: u32, error: u32, then: Done<u32>) -> Outcome {
    destroy_then(
        env,
        destination,
        Box::new(move |env, _| then(env, Err(error))),
    )
}
fn copy_next(
    env: &mut Env<'_>,
    source: View,
    kind: u16,
    destination: u32,
    target: u32,
    mut index: u32,
    then: Done<u32>,
) -> Outcome {
    loop {
        if index == source.count {
            return then(env, Ok(destination));
        }
        let from = source.data + index * source.element;
        let to = target + index * source.element;
        if kind == 12 {
            let outcome = variant::copy_value(env, to, from);
            match outcome {
                Outcome::Return(0) => {
                    index += 1;
                    continue;
                }
                Outcome::Return(error) => return failed(env, destination, error, then),
                pending => {
                    return variant::complete(
                        env,
                        pending,
                        Box::new(move |env, status| {
                            if status == 0 {
                                copy_next(env, source, kind, destination, target, index + 1, then)
                            } else {
                                failed(env, destination, status, then)
                            }
                        }),
                    );
                }
            }
        }
        let pointer = env.cpu.read_u32(from).unwrap();
        if kind == 8 && pointer != 0 {
            if !owns_bstr(env, pointer) {
                capability(env, "array BSTR is not owned by the shared allocator");
                return failed(env, destination, INVALID, then);
            }
            let length = match env.cpu.read_u32(pointer - 4) {
                Ok(length) if length <= MAX_BSTR_BYTES => length,
                _ => return failed(env, destination, INVALID, then),
            };
            let copy = copy_bstr(env, pointer, length);
            if copy == 0 {
                return failed(env, destination, MEMORY, then);
            }
            let _ = env.cpu.write_u32(to, copy);
        } else {
            let target_method = if pointer == 0 {
                None
            } else {
                match variant::method(env, pointer, 1) {
                    Ok(target) => Some(target),
                    Err(error) => return failed(env, destination, error, then),
                }
            };
            let _ = env.cpu.write_u32(to, pointer);
            if let Some(method) = target_method {
                return Outcome::call(method, [pointer], move |env, _| {
                    copy_next(env, source, kind, destination, target, index + 1, then)
                });
            }
        }
        index += 1;
    }
}
