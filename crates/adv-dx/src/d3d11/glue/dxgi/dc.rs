use super::*;
use crate::d3d11::desc::Misc;
use crate::interop::{DcSurface, MemoryDc};
use adv_device::surface::{Access, PixelFormat};

pub(super) fn get(env: &mut Env<'_>) -> Outcome {
    let (this, discard, out) = (env.arg(0), env.arg(1) != 0, env.arg(2));
    let result = (|| {
        let provider = *env
            .ext
            .try_get::<MemoryDc>()
            .ok_or(dxgi::Error::Unavailable)?;
        write(env, out, 0u32)?;
        let (pointer, resource) = surface(env, this)?;
        let table = table(env);
        if *table
            .get_mut(&mut env.sys.objects, pointer)
            .unwrap()
            .dc()
            .unwrap()
            != 0
        {
            return Err(dxgi::Error::InvalidCall.into());
        }
        let backing = gpu::backing(env, pointer, resource)?;
        let text = gpu::shadow_text(env, pointer, 0);
        let storage = table
            .get_mut(&mut env.sys.objects, pointer)
            .unwrap()
            .mapping()
            .unwrap();
        let base = storage.pointer();
        let access = if discard { Access::Write } else { Access::Read };
        let mapped = storage.cpu_access(0, access, |layout, slice, parts| {
            if !discard {
                gpu::download(
                    env.ext,
                    env.cpu,
                    &backing,
                    resource,
                    0,
                    gpu::Shadow {
                        base,
                        layout,
                        slice,
                        text,
                    },
                    parts,
                )?;
            }
            Ok::<_, Error>(())
        })?;
        let image = dc_surface(resource, base, mapped.RowPitch);
        table.add_ref(&mut env.sys.objects, pointer)?;
        *table
            .get_mut(&mut env.sys.objects, pointer)
            .unwrap()
            .dc()
            .unwrap() = u32::MAX;
        Ok((provider, pointer, image))
    })();
    match result {
        Ok((provider, pointer, image)) => (provider.create)(
            env,
            image,
            Box::new(move |env, result| {
                let table = table(env);
                let lease = table
                    .get_mut(&mut env.sys.objects, pointer)
                    .unwrap()
                    .dc()
                    .unwrap();
                match result {
                    Ok(dc) => {
                        *lease = dc;
                        call(write(env, out, dc))
                    }
                    Err(_) => {
                        *lease = 0;
                        let mut released = table.release(&mut env.sys.objects, pointer);
                        released.references = dxgi::Error::OutOfMemory.hresult();
                        retire(env, released)
                    }
                }
            }),
        ),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

pub(super) fn release(env: &mut Env<'_>) -> Outcome {
    let this = env.arg(0);
    let result = (|| {
        let provider = *env
            .ext
            .try_get::<MemoryDc>()
            .ok_or(dxgi::Error::Unavailable)?;
        let (pointer, resource) = surface(env, this)?;
        let table = table(env);
        let object = table.get_mut(&mut env.sys.objects, pointer).unwrap();
        let dc = *object.dc().unwrap();
        if matches!(dc, 0 | u32::MAX) {
            return Err(dxgi::Error::InvalidCall.into());
        }
        let storage = object.mapping().unwrap();
        let image = dc_surface(resource, storage.pointer(), storage.layout(0).row_pitch());
        Ok((provider, pointer, dc, resource, image))
    })();
    let (provider, pointer, dc, resource, image) = match result {
        Ok(parts) => parts,
        Err(error) => return Outcome::Return(Error::hresult(error)),
    };
    (provider.flush)(
        env,
        dc,
        image,
        Box::new(move |env, result| {
            if result.is_err() {
                return Outcome::Return(dxgi::Error::InvalidCall.hresult());
            }
            (provider.release)(
                env,
                dc,
                Box::new(move |env, result| {
                    let table = table(env);
                    *table
                        .get_mut(&mut env.sys.objects, pointer)
                        .unwrap()
                        .dc()
                        .unwrap() = 0;
                    let result = result
                        .map_err(|_| Error::Dxgi(dxgi::Error::InvalidCall))
                        .and_then(|()| {
                            let backing = gpu::backing(env, pointer, resource)?;
                            let storage = table
                                .get_mut(&mut env.sys.objects, pointer)
                                .unwrap()
                                .mapping()
                                .unwrap();
                            storage.cpu_access(0, Access::Write, |_, _, _| Ok::<_, Error>(()))?;
                            gpu::staging_access(env, pointer, resource, 0, &backing, Access::Read)
                        });
                    let mut released = table.release(&mut env.sys.objects, pointer);
                    released.references = result.map_or_else(Error::hresult, |()| 0);
                    retire(env, released)
                }),
            )
        }),
    )
}

fn surface(env: &Env<'_>, this: u32) -> Result<(u32, Resource), Error> {
    let table = table(env);
    let pointer = table
        .canonical(&env.sys.objects, this)
        .ok_or(Error::InvalidArgument)?;
    let resource = ops::object(env, table, pointer)?
        .resource()
        .ok_or(Error::InvalidArgument)?;
    if !resource.misc().contains(Misc::GDI_COMPATIBLE) {
        return Err(dxgi::Error::InvalidCall.into());
    }
    Ok((pointer, resource))
}

fn dc_surface(resource: Resource, pixels: u32, pitch: u32) -> DcSurface {
    let Resource::Texture(texture) = resource else {
        unreachable!()
    };
    let [width, height, _] = texture.extent(0);
    DcSurface {
        surface: None,
        pixels,
        width,
        height,
        pitch,
        format: PixelFormat::Bgra32,
    }
}
