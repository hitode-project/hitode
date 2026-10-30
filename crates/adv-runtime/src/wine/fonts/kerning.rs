//! Unicode kerning pairs from the shared selected-font realization.
use super::*;
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(pairs_w_ABI);
    registry.register(pairs_a_ABI);
}

#[win32(gdi32, name = "GetKerningPairsW", level = implement)]
pub(super) fn pairs_w(env: &mut Env<'_>, dc: Handle, capacity: u32, output: GuestPtr<u8>) -> u32 {
    pairs(env, dc, capacity, output, true)
}
#[win32(gdi32, name = "GetKerningPairsA", level = implement)]
pub(super) fn pairs_a(env: &mut Env<'_>, dc: Handle, capacity: u32, output: GuestPtr<u8>) -> u32 {
    pairs(env, dc, capacity, output, false)
}
pub(super) fn pairs(
    env: &mut Env<'_>,
    dc: Handle,
    capacity: u32,
    output: GuestPtr<u8>,
    wide: bool,
) -> u32 {
    let api = if wide {
        "gdi32.dll!GetKerningPairsW"
    } else {
        "gdi32.dll!GetKerningPairsA"
    };
    // Authored native oracle: invalid/NULL HDC returns zero and preserves
    // LastError, unlike some other GDI queries in this personality.
    if dc_pointer(env, dc.0).is_none() {
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
                api,
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
    let codec = env.ext.get::<Fonts>().codec;
    let codepage = ansi_codepage(env, &request);
    let text = &mut env.ext.get_mut::<Fonts>().text;
    let result = if !wide {
        text.kerning_pairs(&request, text::MAX_KERNING_PAIRS)
            .and_then(|pairs| {
                if pairs.is_empty() {
                    return Ok(0);
                }
                let codepage = codepage.ok_or_else(|| {
                    TextError::Font("ANSI kerning charset has no supported mapping".into())
                })?;
                let encode = |unit: u16| {
                    let (bytes, lossy) = codec.from_utf16(codepage, &[unit]);
                    if lossy {
                        return None;
                    }
                    match bytes.as_slice() {
                        [byte] => Some(u16::from(*byte)),
                        [lead, trail] => Some(u16::from_be_bytes([*lead, *trail])),
                        _ => None,
                    }
                };
                // Native A filters unencodable Unicode pairs, then applies capacity;
                // DBCS words place the lead byte in the high byte, as glyph queries do.
                let converted: Vec<_> = pairs
                    .into_iter()
                    .filter_map(|pair| {
                        Some(text::KerningPair {
                            first: encode(pair.first)?,
                            second: encode(pair.second)?,
                            adjustment: pair.adjustment,
                        })
                    })
                    .collect();
                if output.is_null() {
                    return Ok(converted.len() as u32);
                }
                let count = converted.len().min(capacity as usize);
                let mut bytes = Vec::with_capacity(count * 8);
                for pair in converted.into_iter().take(count) {
                    bytes.extend(pair.first.to_le_bytes());
                    bytes.extend(pair.second.to_le_bytes());
                    bytes.extend(pair.adjustment.to_le_bytes());
                }
                if !bytes.is_empty() && env.cpu.write(output.addr(), &bytes).is_err() {
                    return Err(TextError::Font(
                        "kerning-pair output became inaccessible".into(),
                    ));
                }
                Ok(count as u32)
            })
    } else if output.is_null() {
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
            adv_abi::warn_unavailable(env, api, &error.to_string());
            set_last_error(env, 50);
            0
        }
    }
}
