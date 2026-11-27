//! Class linkage and class instance device children.

use super::*;
use crate::d3d11::object::ClassInstance;
use adv_abi::types::D3D11_CLASS_INSTANCE_DESC;

pub(super) fn create_linkage(env: &mut Env<'_>) -> Outcome {
    let (device, out) = (env.arg(0), env.arg(1));
    status((|| {
        device::report(env, device)?;
        output(env, out, |env| {
            let pointer = alloc(env, "ID3D11ClassLinkage")?;
            if let Err(error) =
                table(env).attach_class_linkage(&mut env.sys.objects, pointer, device)
            {
                free(env, pointer);
                return Err(error.into());
            }
            Ok(pointer)
        })
    })())
}

pub(super) fn create_instance(env: &mut Env<'_>) -> Outcome {
    let [linkage, name, cb, vector, texture, sampler, out] =
        std::array::from_fn(|index| env.arg(index as u32));
    status((|| {
        let class = device::semantic_name(env, name)?;
        let obj = ops::object(env, table(env), linkage)?;
        let definitions = obj.class_linkage().ok_or(Error::InvalidArgument)?;
        let ty = definitions
            .iter()
            .find_map(|shader| shader.classes.iter().find(|ty| ty.name == class))
            .ok_or(Error::InvalidArgument)?;
        if (ty.constants != 0
            && (cb >= 14 || vector.saturating_add(u32::from(ty.constants)) > 4096))
            || (ty.textures != 0 && texture.saturating_add(u32::from(ty.textures)) > 128)
            || (ty.samplers != 0 && sampler.saturating_add(u32::from(ty.samplers)) > 16)
        {
            return Err(Error::InvalidArgument);
        }
        let instance = ClassInstance {
            linkage,
            class,
            name: Vec::new(),
            desc: D3D11_CLASS_INSTANCE_DESC {
                TypeId: u32::from(ty.id),
                ConstantBuffer: cb,
                BaseConstantBufferOffset: vector,
                BaseTexture: texture,
                BaseSampler: sampler,
                Created: 1,
                ..Default::default()
            },
        };
        attach(env, obj.device().unwrap(), out, instance)
    })())
}

pub(super) fn get_instance(env: &mut Env<'_>) -> Outcome {
    let [linkage, name, index, out] = std::array::from_fn(|slot| env.arg(slot as u32));
    status((|| {
        let name = device::semantic_name(env, name)?;
        let object = ops::object_mut(env, table(env), linkage)?;
        let id = object.class_name_id(&name).ok_or(Error::InvalidArgument)?;
        let device = object.device().unwrap();
        let instance = ClassInstance {
            linkage,
            class: Vec::new(),
            name,
            desc: D3D11_CLASS_INSTANCE_DESC {
                InstanceId: id,
                InstanceIndex: index,
                ..Default::default()
            },
        };
        attach(env, device, out, instance)
    })())
}

fn attach(env: &mut Env<'_>, device: u32, out: u32, instance: ClassInstance) -> Result<u32, Error> {
    output(env, out, |env| {
        let pointer = alloc(env, "ID3D11ClassInstance")?;
        if let Err(error) =
            table(env).attach_class_instance(&mut env.sys.objects, pointer, device, instance)
        {
            free(env, pointer);
            return Err(error.into());
        }
        Ok(pointer)
    })
}

pub(super) fn get_linkage(env: &mut Env<'_>) -> Outcome {
    related(env, |object| {
        object.class_instance().map(|instance| instance.linkage)
    })
}

pub(super) fn class_desc(env: &mut Env<'_>) -> Outcome {
    call((|| {
        let desc = ops::object(env, table(env), env.arg(0))?
            .class_instance()
            .ok_or(Error::InvalidArgument)?
            .desc;
        write(env, env.arg(1), desc)
    })())
}

pub(super) fn instance_name(env: &mut Env<'_>) -> Outcome {
    name(env, |instance| &instance.name)
}

pub(super) fn type_name(env: &mut Env<'_>) -> Outcome {
    name(env, |instance| &instance.class)
}

fn name(env: &mut Env<'_>, select: impl FnOnce(&ClassInstance) -> &[u8]) -> Outcome {
    let [this, out, size] = std::array::from_fn(|index| env.arg(index as u32));
    call((|| {
        let value = select(
            ops::object(env, table(env), this)?
                .class_instance()
                .ok_or(Error::InvalidArgument)?,
        );
        let mut bytes = [value, &[0]].concat();
        let required = bytes.len() as u32;
        if out != 0 {
            let capacity = read::<u32>(env, size)?.min(required) as usize;
            if capacity != 0 {
                bytes[capacity - 1] = 0;
                env.cpu
                    .write(out, &bytes[..capacity])
                    .map_err(|_| Error::InvalidArgument)?;
            }
        }
        write(env, size, required)
    })())
}
