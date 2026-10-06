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
    fn get<'a>(&self, objects: &'a Objects, id: u32) -> Option<&'a Driver> {
        objects.owned(objects.lookup_alias::<Driver>(self.namespace?, id)?)
    }

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

fn details(e: &mut Env<'_>, id: u32, output: u32, flags: u32, wide: bool) -> Result<(), u32> {
    if flags != 0 {
        return Err(INVALID_FLAG);
    }
    ensure(e);
    e.ext
        .get::<Acm>()
        .drivers
        .get(&e.sys.objects, id)
        .ok_or(INVALID_HANDLE)?;
    if output == 0 || !e.cpu.accessible(output, 4, Prot::READ) {
        return Err(INVALID_PARAMETER);
    }
    // ACM allows older, shorter structures, down to cbStruct alone. Native
    // clamps oversized requests to the current structure and returns that size.
    let size = e.cpu.read_u32(output).map_err(|_| INVALID_PARAMETER)?;
    let available = size.min(if wide { 1800 } else { 920 });
    if size < 4 || !e.cpu.accessible(output, available, Prot::RW) {
        return Err(INVALID_PARAMETER);
    }
    let mut bytes = vec![0; if wide { 1800 } else { 920 }];
    let word = |bytes: &mut [u8], at: usize, value: u32| {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    word(&mut bytes, 0, available);
    word(&mut bytes, 4, u32::from_le_bytes(*b"audc"));
    word(&mut bytes, 16, 0x0400_0000); // ACM version 4.0.
    word(&mut bytes, 20, 0x0100_0000); // Virtual driver version 1.0.
    word(&mut bytes, 24, SUPPORT);
    word(&mut bytes, 28, 3); // PCM, MS ADPCM, IMA ADPCM.
    let mut offset = 40;
    for (capacity, text) in [
        (32, "Symphonia"),
        (128, "Symphonia software audio decoder"),
        (80, "Symphonia contributors"),
        (128, "MPL-2.0"),
        (
            512,
            "PCM 8/16-bit conversion; Microsoft and IMA ADPCM decoding",
        ),
    ] {
        if wide {
            for (i, unit) in text.encode_utf16().take(capacity - 1).enumerate() {
                bytes[offset + i * 2..offset + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
        } else {
            let count = text.len().min(capacity - 1);
            bytes[offset..offset + count].copy_from_slice(&text.as_bytes()[..count]);
        }
        offset += capacity * if wide { 2 } else { 1 };
    }
    e.cpu
        .write(output, &bytes[..available as usize])
        .map_err(|_| INVALID_PARAMETER)
}

#[win32(msacm32, name = "acmDriverDetailsA", level = state)]
fn details_a(e: &mut Env<'_>, id: Handle, output: GuestPtr<()>, flags: u32) -> u32 {
    details(e, id.0, output.addr(), flags, false)
        .err()
        .unwrap_or(0)
}

#[win32(msacm32, name = "acmDriverDetailsW", level = state)]
fn details_w(e: &mut Env<'_>, id: Handle, output: GuestPtr<()>, flags: u32) -> u32 {
    details(e, id.0, output.addr(), flags, true)
        .err()
        .unwrap_or(0)
}

fn metric(e: &mut Env<'_>, object: u32, index: u32, output: u32) -> Result<(), u32> {
    if output == 0 || !e.cpu.accessible(output, 4, Prot::WRITE) {
        return Err(INVALID_PARAMETER);
    }
    ensure(e);
    let catalog = &e.ext.get::<Acm>().drivers;
    // Unknown non-null handles fail before initializing the result. NULL is a
    // valid ACM-wide selector, so each metric initializes its own default first.
    if object != 0 {
        catalog.get(&e.sys.objects, object).ok_or(INVALID_HANDLE)?;
    }
    if matches!(index, 1..=6 | 20..=24 | 50 | 51 | 100 | 101) {
        e.cpu.write_u32(output, 0).map_err(|_| INVALID_PARAMETER)?;
    }
    let value = match index {
        // Counts describe the same global catalog that enumeration traverses.
        1..=6 | 20..=24 => {
            if object != 0 {
                return Err(INVALID_HANDLE);
            }
            u32::from(matches!(index, 1..=3))
        }
        50 => {
            // MS ADPCM's standard coefficient table is the largest format.
            18 + 32
        }
        51 => return Err(NOT_POSSIBLE), // No filter formats are installed.
        100 | 101 => {
            if object == 0 {
                return Err(INVALID_HANDLE);
            }
            if index == 100 { SUPPORT } else { 1 }
        }
        30 | 31 => {
            e.cpu
                .write_u32(output, u32::MAX)
                .map_err(|_| INVALID_PARAMETER)?;
            return Err(INVALID_HANDLE); // No hardware wave device association.
        }
        _ => {
            adv_abi::warn_unavailable(e, "acmMetrics", "requested ACM metric is unavailable");
            return Err(NOT_SUPPORTED);
        }
    };
    e.cpu
        .write_u32(output, value)
        .map_err(|_| INVALID_PARAMETER)
}

#[win32(msacm32, name = "acmMetrics", level = state)]
fn metrics(e: &mut Env<'_>, object: Handle, index: u32, output: GuestPtr<()>) -> u32 {
    metric(e, object.0, index, output.addr()).err().unwrap_or(0)
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
    registry.register(details_a_ABI);
    registry.register(details_w_ABI);
    registry.register(metrics_ABI);
}

#[cfg(test)]
mod tests;
