//! Device-child COM operations.
use super::object::{Error as ObjectError, Object, PrivateData, Release, Table};
use super::{caps, desc};
use adv_abi::{Env, com::Iid};

#[derive(Clone, Copy)]
pub(super) enum Error {
    InvalidArgument,
    NotFound,
    MoreData,
    Object(ObjectError),
    Caps(caps::Error),
    Description(desc::Error),
    Dxgi(crate::dxgi::Error),
    Map(super::object::map::Error),
    #[cfg(feature = "gpu")]
    Device,
}

impl From<ObjectError> for Error {
    fn from(value: ObjectError) -> Self {
        Self::Object(value)
    }
}

pub(super) fn object<'a>(
    env: &'a Env<'_>,
    table: Table,
    pointer: u32,
) -> Result<&'a Object, Error> {
    table
        .get(&env.sys.objects, pointer)
        .ok_or(Error::InvalidArgument)
}

pub(super) fn object_mut<'a>(
    env: &'a mut Env<'_>,
    table: Table,
    pointer: u32,
) -> Result<&'a mut Object, Error> {
    table
        .get_mut(&mut env.sys.objects, pointer)
        .ok_or(Error::InvalidArgument)
}

pub(super) fn query(env: &mut Env<'_>, table: Table, pointer: u32, iid: Iid) -> Result<u32, Error> {
    Ok(table.query(&mut env.sys.objects, pointer, iid)?)
}

pub(super) fn add_ref(env: &mut Env<'_>, table: Table, pointer: u32) -> Result<u32, Error> {
    Ok(table.add_ref(&mut env.sys.objects, pointer)?)
}

pub(super) fn release(env: &mut Env<'_>, table: Table, pointer: u32) -> Release {
    table.release(&mut env.sys.objects, pointer)
}

pub(super) fn related(
    env: &mut Env<'_>,
    table: Table,
    pointer: u32,
    relation: impl FnOnce(&Object) -> Option<u32>,
) -> Result<u32, Error> {
    let reference = relation(object(env, table, pointer)?).ok_or(Error::InvalidArgument)?;
    add_ref(env, table, reference)?;
    Ok(reference)
}

pub(super) fn set_private(
    env: &mut Env<'_>,
    table: Table,
    pointer: u32,
    iid: Iid,
    data: Option<PrivateData>,
) -> Result<Option<PrivateData>, Error> {
    Ok(object_mut(env, table, pointer)?.set_private(iid, data))
}

impl From<caps::Error> for Error {
    fn from(value: caps::Error) -> Self {
        Self::Caps(value)
    }
}

impl From<desc::Error> for Error {
    fn from(value: desc::Error) -> Self {
        Self::Description(value)
    }
}

impl From<crate::dxgi::Error> for Error {
    fn from(value: crate::dxgi::Error) -> Self {
        Self::Dxgi(value)
    }
}

impl From<super::object::map::Error> for Error {
    fn from(value: super::object::map::Error) -> Self {
        Self::Map(value)
    }
}

#[cfg(feature = "gpu")]
impl From<adv_device::gpu::Error> for Error {
    fn from(value: adv_device::gpu::Error) -> Self {
        log::debug!("D3D11 GPU command: {value}");
        Self::Device
    }
}

#[cfg(feature = "gpu")]
impl From<adv_gpu::Error> for Error {
    fn from(value: adv_gpu::Error) -> Self {
        log::debug!("D3D11 GPU resource: {value}");
        Self::Device
    }
}
