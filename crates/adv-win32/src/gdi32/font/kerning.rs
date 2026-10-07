//! Unicode kerning pairs from the shared selected-font realization.
use super::*;
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(pairs_w_ABI);
}

#[win32(gdi32, name = "GetKerningPairsW", level = implement)]
fn pairs_w(env: &mut Env<'_>, dc: Handle, capacity: u32, output: GuestPtr<u8>) -> u32 {
    // Authored native oracle: invalid/NULL HDC returns zero and preserves
    // LastError, unlike some other GDI queries in this personality.
    if !matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0),
        Some(GdiObj::Dc { .. })
    ) {
        return 0;
    }
    if !output.is_null() {
        let length = capacity.checked_mul(8).filter(|_| capacity != 0);
        let Some(length) = length else {
            set_last_error(env, 87);
            return 0;
        };
        if capacity > text::MAX_KERNING_PAIRS {
            adv_abi::warn_unavailable(
                env,
                "gdi32.dll!GetKerningPairsW",
                "kerning-pair output capacity exceeds the supported 1,048,576-entry budget",
            );
            set_last_error(env, 87);
            return 0;
        }
        // Validate all advertised entries before realizing a font or publishing
        // a partial result, even when the provider has fewer than capacity.
        if adv_abi::guest::check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err() {
            set_last_error(env, 87);
            return 0;
        }
    }
    let request = font_request(env, dc.0);
    let text = &mut env.ext.get_mut::<Fonts>().text;
    let result = if output.is_null() {
        // The provider ignores capacity for count-only queries.
        text.kerning_pair_count(&request)
    } else {
        text.kerning_pairs(&request, capacity).and_then(|pairs| {
            let count = pairs.len() as u32;
            let mut packed = Vec::with_capacity(pairs.len() * 8);
            for pair in pairs {
                packed.extend_from_slice(&pair.first.to_le_bytes());
                packed.extend_from_slice(&pair.second.to_le_bytes());
                packed.extend_from_slice(&pair.adjustment.to_le_bytes());
            }
            if !packed.is_empty() && env.cpu.write(output.addr(), &packed).is_err() {
                return Err(TextError::Font(
                    "kerning-pair output became inaccessible".into(),
                ));
            }
            Ok(count)
        })
    };
    match result {
        Ok(count) => count,
        Err(error) => {
            adv_abi::warn_unavailable(env, "gdi32.dll!GetKerningPairsW", &error.to_string());
            set_last_error(env, 50);
            0
        }
    }
}
