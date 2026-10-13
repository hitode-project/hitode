//! Bounded copies use native handles, bytes and offsets throughout the operation.
use super::*;
use crate::util::set_last_error;
use std::{cell::RefCell, rc::Rc};

fn failed(env: &mut Env<'_>, status: u32) -> Outcome {
    set_last_error(env, win32_error(status));
    Outcome::Return(0)
}

fn identity(env: &mut Env<'_>, handle: u32, then: operations::Done<(u64, u32)>) -> Outcome {
    operations::information(
        env,
        handle,
        6,
        8,
        false,
        Box::new(move |env, internal| {
            let internal = match internal {
                Ok(bytes) => u64::from_le_bytes(bytes.try_into().unwrap()),
                Err(status) => return then(env, Err(status)),
            };
            operations::information(
                env,
                handle,
                1,
                20,
                true,
                Box::new(move |env, volume| match volume {
                    Ok(bytes) => then(
                        env,
                        Ok((
                            internal,
                            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                        )),
                    ),
                    Err(status) => then(env, Err(status)),
                }),
            )
        }),
    )
}

struct Copy {
    scratch: Attributes,
    source: u32,
    target: u32,
    copied: u64,
    count: u32,
    written: u32,
    moving: Option<String>,
    write_through: bool,
}

impl Copy {
    fn finish(self, env: &mut Env<'_>, result: Result<(), u32>) -> Outcome {
        if result.is_ok() && self.write_through {
            operations::flush(
                env,
                self.target,
                Box::new(move |env, result| self.close(env, result)),
            )
        } else {
            self.close(env, result)
        }
    }

    fn close(self, env: &mut Env<'_>, result: Result<(), u32>) -> Outcome {
        operations::close(
            env,
            self.target,
            Box::new(move |env, target| {
                let result = result.and(target);
                operations::close(
                    env,
                    self.source,
                    Box::new(move |env, source| {
                        self.scratch.free(env);
                        match result.and(source) {
                            Ok(()) => match self.moving {
                                Some(source) => super::mutation::delete_copied(env, source),
                                None => Outcome::Return(1),
                            },
                            Err(status) => failed(env, status),
                        }
                    }),
                )
            }),
        )
    }

    fn transferred(
        mut self,
        env: &mut Env<'_>,
        writing: bool,
        result: Result<u32, u32>,
    ) -> Result<Self, Outcome> {
        let count = match result {
            Ok(count) => count,
            Err(status) => return Err(self.finish(env, Err(status))),
        };
        if count == 0 {
            if writing {
                return Err(self.finish(env, Err(0xc0000001)));
            }
            let length = self.copied.to_le_bytes().to_vec();
            return Err(operations::set_information(
                env,
                self.target,
                20,
                length,
                Box::new(move |env, result| self.finish(env, result)),
            ));
        }
        if writing {
            self.written += count;
            if self.written == self.count {
                self.copied += u64::from(self.count);
                if self.copied > i64::MAX as u64 {
                    return Err(self.finish(env, Err(FILE_TOO_LARGE)));
                }
            }
        } else {
            self.count = count;
            self.written = 0;
        }
        Ok(self)
    }

    fn transfer(mut self, env: &mut Env<'_>) -> Outcome {
        loop {
            let writing = self.written != self.count;
            let (handle, buffer, length) = if writing {
                (
                    self.target,
                    self.scratch.extra + self.written,
                    self.count - self.written,
                )
            } else {
                (self.source, self.scratch.extra, CHUNK)
            };
            let completed = Rc::new(RefCell::new(None));
            let inline = Rc::downgrade(&completed);
            let outcome = operations::transfer(
                env,
                handle,
                buffer,
                length,
                writing,
                Box::new(move |env, result| {
                    // Pristine NT entries complete inline. Hand their state back
                    // to the loop so file length cannot grow the host call stack.
                    // A guest continuation outlives this slot and resumes afresh.
                    if let Some(completed) = inline.upgrade() {
                        *completed.borrow_mut() = Some((self, result));
                        Outcome::Return(0)
                    } else {
                        match self.transferred(env, writing, result) {
                            Ok(copy) => copy.transfer(env),
                            Err(outcome) => outcome,
                        }
                    }
                }),
            );
            let Some((copy, result)) = completed.borrow_mut().take() else {
                return outcome;
            };
            self = match copy.transferred(env, writing, result) {
                Ok(copy) => copy,
                Err(outcome) => return outcome,
            };
        }
    }

    fn start(self, env: &mut Env<'_>) -> Outcome {
        operations::seek(
            env,
            self.source,
            0,
            0,
            Box::new(move |env, source| {
                if let Err(status) = source {
                    return self.finish(env, Err(status));
                }
                operations::seek(
                    env,
                    self.target,
                    0,
                    0,
                    Box::new(move |env, target| {
                        if let Err(status) = target {
                            return self.finish(env, Err(status));
                        }
                        operations::set_information(
                            env,
                            self.target,
                            20,
                            vec![0; 8],
                            Box::new(move |env, result| match result {
                                Ok(()) => self.transfer(env),
                                Err(status) => self.finish(env, Err(status)),
                            }),
                        )
                    }),
                )
            }),
        )
    }
}

pub(in crate::kernel32) fn copy(
    env: &mut Env<'_>,
    source: String,
    target: String,
    fail_if_exists: bool,
) -> Outcome {
    copy_options(env, source, target, fail_if_exists, false, false)
}

pub(super) fn move_copy(env: &mut Env<'_>, source: String, target: String, flags: u32) -> Outcome {
    copy_options(env, source, target, flags & 1 == 0, true, flags & 8 != 0)
}

fn copy_options(
    env: &mut Env<'_>,
    source: String,
    target: String,
    fail_if_exists: bool,
    moving: bool,
    write_through: bool,
) -> Outcome {
    let source = env.ext.get::<Win32>().vfs.normalize(&source);
    let target = env.ext.get::<Win32>().vfs.normalize(&target);
    if source.eq_ignore_ascii_case(&target) {
        return failed(env, 0xc0000043);
    }
    let native_owns_names = super::metadata::active(env, &["NtCreateFile", "NtOpenFile"]);
    if !native_owns_names
        && !super::metadata::active(
            env,
            &["NtQueryInformationFile", "NtQueryVolumeInformationFile"],
        )
    {
        // Known host aliases can be refused before copy-up creates another instance.
        let vfs = &env.ext.get::<Win32>().vfs;
        if let (Some(source), Some(target)) = (vfs.lookup(&source), vfs.lookup(&target)) {
            let instance = |path| {
                std::fs::File::open(path)
                    .and_then(|file| adv_os::vfs::file_identity(&file))
                    .ok()
            };
            if let Some(source) = instance(source)
                && instance(target) == Some(source)
            {
                return failed(env, 0xc0000043);
            }
        }
    }
    if !native_owns_names && !fail_if_exists {
        if let Err(error) = super::super::replacement_allowed(env, &target, 0, true) {
            set_last_error(env, error);
            return Outcome::Return(0);
        }
    }
    let move_source = moving.then(|| source.clone());
    operations::open(
        env,
        &source,
        0x80100080,
        7,
        1,
        0x60,
        Box::new(move |env, source| {
            let source = match source {
                Ok(handle) => handle,
                Err(status) => return failed(env, status),
            };
            identity(
                env,
                source,
                Box::new(move |env, source_id| {
                    let source_id = match source_id {
                        Ok(identity) => identity,
                        Err(status) => {
                            return operations::close(
                                env,
                                source,
                                Box::new(move |env, _| failed(env, status)),
                            );
                        }
                    };
                    operations::open(
                        env,
                        &target,
                        0x40100080,
                        7,
                        if fail_if_exists { 2 } else { 3 },
                        0x60,
                        Box::new(move |env, target| {
                            let target = match target {
                                Ok(handle) => handle,
                                Err(status) => {
                                    return operations::close(
                                        env,
                                        source,
                                        Box::new(move |env, _| failed(env, status)),
                                    );
                                }
                            };
                            identity(
                                env,
                                target,
                                Box::new(move |env, target_id| {
                                    let scratch = match attributes(env, r"C:\", CHUNK) {
                                        Ok(scratch) => scratch,
                                        Err(status) => {
                                            return operations::close(
                                                env,
                                                target,
                                                Box::new(move |env, _| {
                                                    operations::close(
                                                        env,
                                                        source,
                                                        Box::new(move |env, _| failed(env, status)),
                                                    )
                                                }),
                                            );
                                        }
                                    };
                                    let copy = Copy {
                                        scratch,
                                        source,
                                        target,
                                        copied: 0,
                                        count: 0,
                                        written: 0,
                                        moving: move_source,
                                        write_through,
                                    };
                                    match target_id {
                                        Ok(target_id) if target_id == source_id => {
                                            copy.finish(env, Err(0xc0000043))
                                        }
                                        Ok(_) => copy.start(env),
                                        Err(status) => copy.finish(env, Err(status)),
                                    }
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
