//! Map admission before synchronization or guest allocation.
use super::super::desc::{Resource, Usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    StillDrawing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Map {
    Dynamic,
    Staging { access: Access, wait: bool },
}

impl Map {
    pub fn admit(
        resource: Resource,
        subresource: u32,
        mode: u32,
        flags: u32,
    ) -> Result<Self, Error> {
        const DO_NOT_WAIT: u32 = 0x100000;
        if subresource >= resource.subresources() || flags & !DO_NOT_WAIT != 0 {
            return Err(Error::InvalidArgument);
        }
        let cpu = resource.cpu_access();
        match (resource.usage(), mode) {
            (Usage::Dynamic, 4) if flags == 0 => Ok(Self::Dynamic),
            (Usage::Dynamic, 5) if flags == 0 && matches!(resource, Resource::Buffer(_)) => {
                Ok(Self::Dynamic)
            }
            (Usage::Staging, _) => {
                let access = match mode {
                    1 if cpu.read() => Access::Read,
                    2 if cpu.write() => Access::Write,
                    3 if cpu.read() && cpu.write() => Access::ReadWrite,
                    _ => return Err(Error::InvalidArgument),
                };
                Ok(Self::Staging {
                    access,
                    wait: flags == 0,
                })
            }
            _ => Err(Error::InvalidArgument),
        }
    }

    pub fn check_busy(self, busy: bool) -> Result<(), Error> {
        if busy && matches!(self, Self::Staging { wait: false, .. }) {
            Err(Error::StillDrawing)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
