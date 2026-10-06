//! The virtual ACM catalog describes the decoders used by ACM streams.
use super::*;

const SUPPORT: u32 = 1 | 2; // CODEC | CONVERTER: ADPCM decode and PCM conversion.
const ENUM_FLAGS: u32 = 0xc000_0000; // NOLOCAL | DISABLED.

struct Driver;

#[derive(Default)]
pub(super) struct Catalog {
    namespace: Option<Namespace>,
    driver: Option<u32>,
}

impl Catalog {
    fn id(&mut self, objects: &mut Objects) -> Result<u32, u32> {
        if let Some(id) = self.driver {
            return Ok(id);
        }
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0xad00_0004, 0xadff_fffc);
        }
        let id = objects
            .insert_host_in(self.namespace.ok_or(NO_MEMORY)?, Driver)
            .ok_or(NO_MEMORY)?;
        self.driver = Some(id);
        Ok(id)
    }
}

#[win32(msacm32, name = "acmDriverEnum", level = state)]
fn enumerate(e: &mut Env<'_>, callback: GuestPtr<()>, instance: u32, flags: u32) -> Outcome {
    if flags & !ENUM_FLAGS != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    if callback.addr() == 0 || !e.cpu.accessible(callback.addr(), 1, Prot::EXEC) {
        return Outcome::Return(INVALID_PARAMETER);
    }
    ensure(e);
    let id = match e.ext.get_mut::<Acm>().drivers.id(&mut e.sys.objects) {
        Ok(id) => id,
        Err(error) => return Outcome::Return(error),
    };
    // There is one global synchronous software driver. Its stable driver ID is
    // owned by the process object table. A callback can re-enter ACM or sleep;
    // no catalog borrow is retained across guest execution. Both early stop
    // (FALSE) and exhaustion (TRUE) finish enumeration successfully.
    Outcome::call(callback.addr(), [id, instance, SUPPORT], |_, _| {
        Outcome::Return(0)
    })
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(enumerate_ABI);
}

#[cfg(test)]
mod tests;
