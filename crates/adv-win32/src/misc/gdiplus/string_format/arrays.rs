//! Copy bounded layout arrays before changing the device-owned format state.
use super::*;

#[win32(gdiplus, name = "GdipSetStringFormatTabStops", level = implement)]
fn set_tabs(
    env: &mut Env<'_>,
    format: GuestPtr<()>,
    first: f32,
    count: i32,
    stops: GuestPtr<f32>,
) -> i32 {
    let call = (|| {
        if count < 0 || stops.is_null() || first < 0. {
            return Err(2);
        }
        let bytes = (count as u32).checked_mul(4).ok_or(2)?;
        adv_abi::guest::check_range(env.cpu, stops.addr(), bytes, Prot::READ).map_err(|_| 2)?;
        if bytes as usize > MAX_BYTES {
            return Err(3);
        }
        let mut tabs = Vec::new();
        tabs.try_reserve_exact(count as usize).map_err(|_| 3)?;
        for i in 0..count as u32 {
            tabs.push(f32::from_bits(
                env.cpu.read_u32(stops.addr() + i * 4).map_err(|_| 2)?,
            ));
        }
        object(env, format.addr())?.execute(Op::Tabs(Some((first, &tabs))))?;
        Ok(())
    })();
    call.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipGetStringFormatTabStops", level = implement)]
fn get_tabs(
    env: &mut Env<'_>,
    format: GuestPtr<()>,
    count: i32,
    first: GuestPtr<f32>,
    stops: GuestPtr<f32>,
) -> i32 {
    let call = (|| {
        if count < 0 || first.is_null() || stops.is_null() {
            return Err(2);
        }
        let bytes = (count as u32).checked_mul(4).ok_or(2)?;
        adv_abi::guest::check_range(env.cpu, first.addr(), 4, Prot::WRITE).map_err(|_| 2)?;
        adv_abi::guest::check_range(env.cpu, stops.addr(), bytes, Prot::WRITE).map_err(|_| 2)?;
        match object(env, format.addr())?.execute(Op::Tabs(None))? {
            Reply::Tabs(offset, tabs) if tabs.len() == count as usize => {
                first.write(env.cpu, offset).map_err(|_| 2)?;
                for (i, value) in tabs.into_iter().enumerate() {
                    env.cpu
                        .write_u32(stops.addr() + i as u32 * 4, value.to_bits())
                        .map_err(|_| 2)?;
                }
                Ok(())
            }
            _ => Err(2),
        }
    })();
    call.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipSetStringFormatMeasurableCharacterRanges", level = implement)]
fn set_ranges(env: &mut Env<'_>, format: GuestPtr<()>, count: i32, ranges: GuestPtr<()>) -> i32 {
    let call = (|| {
        if count > 32 {
            return Err(11);
        }
        if count < 0 || (count > 0 && ranges.is_null()) {
            return Err(2);
        }
        let mut values = [[0; 2]; 32];
        if count > 0 {
            adv_abi::guest::check_range(env.cpu, ranges.addr(), count as u32 * 8, Prot::READ)
                .map_err(|_| 2)?;
            for (i, value) in values[..count as usize].iter_mut().flatten().enumerate() {
                *value = env
                    .cpu
                    .read_u32(ranges.addr() + i as u32 * 4)
                    .map_err(|_| 2)? as i32;
            }
        }
        object(env, format.addr())?.execute(Op::CharacterRanges(&values[..count as usize]))?;
        Ok(())
    })();
    call.err().unwrap_or(0)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [set_tabs_ABI, get_tabs_ABI, set_ranges_ABI] {
        registry.register(api);
    }
}
