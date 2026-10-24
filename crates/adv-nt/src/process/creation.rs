//! Wine's new_process/new_thread handshake. The supervisor creates the machine
//! only after this host call returns its execution lease.
use crate::{
    file,
    object::{Attributes, read_u32, write_bytes, write_u32},
    protocol, section, server,
    status::{self, Error},
};
use adv_machine::{Env, Outcome};
use std::cell::Cell;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;

pub struct Child {
    pub socket: UnixStream,
    pub image: String,
}
pub struct Children {
    pub pending: Vec<Child>,
    live: Rc<Cell<usize>>,
    total: Rc<Cell<usize>>,
}
impl Default for Children {
    fn default() -> Self {
        Self {
            pending: vec![],
            live: Rc::new(Cell::new(1)),
            total: Rc::new(Cell::new(1)),
        }
    }
}
impl Children {
    pub fn fork(&self) -> Self {
        Self {
            pending: vec![],
            live: self.live.clone(),
            total: self.total.clone(),
        }
    }
    pub fn exited(&self) {
        self.live.set(self.live.get() - 1);
    }
}

pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if env.ext.try_get::<Children>().is_none() {
        return Outcome::Return(status::NOT_SUPPORTED);
    }
    let children = env.ext.get::<Children>();
    if children.live.get() >= 8 || children.total.get() >= 128 {
        return Outcome::Return(status::NO_MEMORY);
    }
    let result = prepare(env, args);
    let (info, process, thread, pid, tid, image_info) = match result {
        Ok(value) => value,
        Err(error) => return status::finish(Err(error)),
    };
    let args = args.to_vec();
    Outcome::host_request(move |env| {
        crate::sync::wait_then(env, info, move |env, code| {
            let result = (|| {
                status::check(code)?;
                let mut request = protocol::GetNewProcessInfoRequest {
                    info,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                if reply.success == 0 {
                    return Err((reply.exit_code as u32).max(1).into());
                }
                let list = args[10];
                let count = read_u32(env, list)?.saturating_sub(4) / 16;
                for index in 0..count {
                    let at = list + 4 + index * 16;
                    let output = match read_u32(env, at)? {
                        0x10003 => [pid.to_le_bytes(), tid.to_le_bytes()].concat(),
                        6 => image_info.to_vec(),
                        _ => continue,
                    };
                    let len = (read_u32(env, at + 4)? as usize).min(output.len());
                    write_bytes(env, read_u32(env, at + 8)?, &output[..len])?;
                    let returned = read_u32(env, at + 12)?;
                    if returned != 0 {
                        write_u32(env, returned, len as u32)?;
                    }
                }
                write_u32(env, args[0], process)?;
                write_u32(env, args[1], thread)?;
                Ok(())
            })();
            let _ = server::close(env, info);
            if result.is_err() {
                let _ = server::close(env, process);
                let _ = server::close(env, thread);
            }
            status::finish(result)
        })
    })
}

type Created = (u32, u32, u32, u32, u32, [u8; 48]);

fn prepare(env: &mut Env<'_>, args: &[u32]) -> Result<Created, Error> {
    let [
        process_out,
        thread_out,
        process_access,
        thread_access,
        process_attr,
        thread_attr,
        flags,
        thread_flags,
        params,
        _,
        list,
    ] = *<&[u32; 11]>::try_from(args).unwrap();
    write_u32(env, process_out, 0)?;
    write_u32(env, thread_out, 0)?;
    if thread_flags & !1 != 0 {
        return Err(status::INVALID_PARAMETER.into());
    }
    let mut image =
        super::read_unicode_string(env.cpu, params + 0x38).ok_or(status::ACCESS_VIOLATION)?;
    let count = read_u32(env, list)?.saturating_sub(4) / 16;
    let mut parent = 0;
    let mut token = 0;
    for index in 0..count {
        let at = list + 4 + index * 16;
        match read_u32(env, at)? {
            0x20000 => parent = read_u32(env, at + 8)?,
            0x60002 => token = read_u32(env, at + 8)?,
            0x20005 => {
                let size = read_u32(env, at + 4)?;
                let mut bytes = vec![0; size as usize];
                env.cpu
                    .read(read_u32(env, at + 8)?, &mut bytes)
                    .map_err(|_| status::ACCESS_VIOLATION)?;
                image = String::from_utf16_lossy(
                    &bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes(*b))
                        .collect::<Vec<_>>(),
                );
            }
            0x60001 => return Err(status::NOT_SUPPORTED.into()),
            _ => {}
        }
    }
    if !image.starts_with(r"\??\") {
        image = format!(r"\??\{image}");
    }
    let file = file::open_path(env, image.encode_utf16().collect(), 0x80100000, 5, 0x60)?;
    let mapping = section::create_mapping(env, 0xf001f, None, 0, 0x20, 0x1000000, file);
    server::close(env, file)?;
    let (mapping, _) = mapping?;
    let mut request = protocol::GetMappingInfoRequest {
        handle: mapping,
        access: 1,
        ..Default::default()
    };
    let mut pe_info = [0; section::PE_IMAGE_INFO_SIZE];
    let answer = server::request(env, &mut request, &[], &mut pe_info);
    server::close(env, mapping)?;
    status::check(answer?.0.header.error)?;
    let image_info = section::image_information(&pe_info);
    let (startup, environment) = crate::params::serialize_startup(env, params, &image)?;
    let (socket, other) = UnixStream::pair().map_err(|_| status::NO_MEMORY)?;
    server::pass_fd(env, other.as_raw_fd())?;
    let attr = if process_attr == 0 {
        Attributes {
            root: 0,
            attributes: 0,
            name: None,
            security: vec![],
        }
    } else {
        Attributes::read(env, process_attr)?
    };
    let mut request = protocol::NewProcessRequest {
        token,
        parent_process: parent,
        flags,
        socket_fd: other.as_raw_fd(),
        access: process_access,
        machine: server::MACHINE_I386,
        info_size: startup.len() as u32,
        ..Default::default()
    };
    let (process, _) = server::request(
        env,
        &mut request,
        &[&attr.wire(), &startup, &environment],
        &mut [],
    )?;
    status::check(process.header.error)?;
    let attr = if thread_attr == 0 {
        Attributes {
            root: 0,
            attributes: 0,
            name: None,
            security: vec![],
        }
    } else {
        Attributes::read(env, thread_attr)?
    };
    let mut request = protocol::NewThreadRequest {
        process: process.handle,
        access: thread_access,
        flags: thread_flags,
        request_fd: -1,
        ..Default::default()
    };
    let (thread, _) = server::request(env, &mut request, &[&attr.wire()], &mut [])?;
    status::check(thread.header.error)?;
    let children = env.ext.get_mut::<Children>();
    children.live.set(children.live.get() + 1);
    children.total.set(children.total.get() + 1);
    children.pending.push(Child {
        socket,
        image: image.trim_start_matches(r"\??\").to_owned(),
    });
    Ok((
        process.info,
        process.handle,
        thread.handle,
        process.pid,
        thread.tid,
        image_info,
    ))
}
