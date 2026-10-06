//! Unicode character advances with complete guest output validation.
use super::*;
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(widths_w_ABI);
}

#[win32(gdi32, name = "GetCharWidth32W", level = implement)]
fn widths_w(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<i32>,
) -> Outcome {
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    let invalid = |env: &mut Env<'_>| {
        set_last_error(env, 87);
        Outcome::Return(0)
    };
    if first > last || last > u32::from(u16::MAX) {
        return invalid(env);
    }
    let bytes = (last - first + 1) * 4;
    if output.is_null()
        || adv_abi::guest::check_range(env.cpu, output.addr(), bytes, Prot::WRITE).is_err()
    {
        return invalid(env);
    }
    let request = font_request(env, dc.0);
    match env
        .ext
        .get_mut::<Fonts>()
        .text
        .character_widths(&request, first as u16, last as u16)
    {
        Ok(widths) => {
            let packed: Vec<_> = widths.into_iter().flat_map(i32::to_le_bytes).collect();
            if env.cpu.write(output.addr(), &packed).is_ok() {
                Outcome::Return(1)
            } else {
                invalid(env)
            }
        }
        Err(error) => {
            adv_abi::warn_unavailable(env, "gdi32.dll!GetCharWidth32W", &error.to_string());
            set_last_error(env, 50);
            Outcome::Return(0)
        }
    }
}
