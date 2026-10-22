//! File system calls, after Wine's `unix/file.c`: the platform finds the
//! host file and the server opens it and holds the handle.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use adv_machine::{Env, Outcome};

use crate::object::{Attributes, write_u32};
use crate::path::{self, Lookup};
use crate::protocol::CreateFileRequest;
use crate::server;
use crate::status::{self, Error};

const FILE_OPEN: u32 = 1;
const FILE_OVERWRITE: u32 = 4;
const FILE_OPEN_BY_FILE_ID: u32 = 0x2000;

/// IO_STATUS_BLOCK.Information after a create.
const FILE_SUPERSEDED: u32 = 0;
const FILE_OPENED: u32 = 1;
const FILE_CREATED: u32 = 2;
const FILE_OVERWRITTEN: u32 = 3;

/// The host side of the process's drives.
pub struct Files {
    /// `<prefix>/dosdevices`, where each drive is a link.
    pub dosdevices: PathBuf,
}

/// NtCreateFile(*handle, access, *attributes, *io, *allocation_size,
/// file_attributes, sharing, disposition, options, ea_buffer, ea_length)
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        access,
        attributes,
        io,
        _,
        file_attributes,
        sharing,
        disposition,
        options,
        _,
        _,
    ] = args
    else {
        unreachable!()
    };
    let open = Open {
        handle,
        access,
        attributes,
        io,
        file_attributes,
        sharing,
        disposition,
        options,
    };
    status::finish(open.run(env))
}

/// NtOpenFile(*handle, access, *attributes, *io, sharing, options)
pub fn open(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, io, sharing, options] = args else {
        unreachable!()
    };
    let open = Open {
        handle,
        access,
        attributes,
        io,
        file_attributes: 0,
        sharing,
        disposition: FILE_OPEN,
        options,
    };
    status::finish(open.run(env))
}

struct Open {
    handle: u32,
    access: u32,
    attributes: u32,
    io: u32,
    file_attributes: u32,
    sharing: u32,
    disposition: u32,
    options: u32,
}

impl Open {
    fn run(&self, env: &mut Env<'_>) -> Result<(), Error> {
        write_u32(env, self.handle, 0)?;
        let result = self.open(env);
        // Like Wine, the status block carries the status of every attempt.
        let status = match &result {
            Ok(_) => status::SUCCESS,
            Err(Error::Status(status)) => *status,
            Err(Error::Unsupported(_)) => return result.map(drop),
        };
        write_u32(env, self.io, status)?;
        let information = result?;
        write_u32(env, self.io + 4, information)?;
        Ok(())
    }

    /// Opens the file and returns the Information value.
    fn open(&self, env: &mut Env<'_>) -> Result<u32, Error> {
        let attributes = Attributes::read(env, self.attributes)?;
        let Some(name) = &attributes.name else {
            return Err(status::INVALID_PARAMETER.into());
        };
        if attributes.root != 0 {
            return Err(Error::Unsupported(
                "opening a file relative to a directory".into(),
            ));
        }
        if self.options & FILE_OPEN_BY_FILE_ID != 0 {
            return Err(Error::Unsupported("opening a file by its id".into()));
        }
        let dosdevices = &env.ext.get::<Files>().dosdevices;
        let (host, created) = match path::lookup(dosdevices, name)? {
            Lookup::Found(host) => (host, false),
            Lookup::Missing(_) if matches!(self.disposition, FILE_OPEN | FILE_OVERWRITE) => {
                return Err(status::OBJECT_NAME_NOT_FOUND.into());
            }
            Lookup::Missing(host) => (host, true),
            Lookup::Device => {
                return Err(Error::Unsupported(format!(
                    "opening device {}",
                    String::from_utf16_lossy(name)
                )));
            }
        };

        let mut request = CreateFileRequest {
            access: self.access,
            sharing: self.sharing,
            create: self.disposition as i32,
            options: self.options,
            attrs: self.file_attributes,
            ..Default::default()
        };
        let objattr = attributes.wire();
        let host = host.as_os_str().as_bytes();
        let (reply, _) = server::request(env, &mut request, &[&objattr, host], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, self.handle, reply.handle)?;
        Ok(match self.disposition {
            _ if created => FILE_CREATED,
            0 => FILE_SUPERSEDED,
            2 => FILE_CREATED,
            4 | 5 => FILE_OVERWRITTEN,
            _ => FILE_OPENED,
        })
    }
}
