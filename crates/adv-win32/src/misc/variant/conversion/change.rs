//! Checked scalar coercion computes the new value before retiring destination ownership.
use super::*;
struct Converted {
    kind: u16,
    payload: Vec<u8>,
    string: u32,
}
impl Converted {
    fn scalar(kind: u16, payload: Vec<u8>) -> Self {
        Self {
            kind,
            payload,
            string: 0,
        }
    }
}
fn number(env: &Env<'_>, bytes: &Variant, locale: Locale) -> Result<Number, u32> {
    Ok(match vt(bytes) {
        0 => Number::from_integer(0),
        2 | 11 => Number::from_integer(i128::from(i16::from_le_bytes(
            bytes[8..10].try_into().unwrap(),
        ))),
        3 | 22 => Number::from_integer(i128::from(pointer(bytes) as i32)),
        16 => Number::from_integer(i128::from(bytes[8] as i8)),
        17 => Number::from_integer(i128::from(bytes[8])),
        18 => Number::from_integer(i128::from(u16::from_le_bytes(
            bytes[8..10].try_into().unwrap(),
        ))),
        19 | 23 => Number::from_integer(i128::from(pointer(bytes))),
        20 => Number::from_integer(i128::from(i64::from_le_bytes(
            bytes[8..16].try_into().unwrap(),
        ))),
        21 => Number::from_integer(i128::from(u64::from_le_bytes(
            bytes[8..16].try_into().unwrap(),
        ))),
        4 => Number::from_float(f64::from(f32::from_bits(pointer(bytes))))?,
        5 | 7 => Number::from_float(f64::from_le_bytes(bytes[8..16].try_into().unwrap()))?,
        6 => Number::from_currency(i64::from_le_bytes(bytes[8..16].try_into().unwrap())),
        8 => Number::parse(&super::super::coercion::text(env, pointer(bytes))?, locale)?,
        _ => return Err(MISMATCH),
    })
}
fn convert(
    env: &mut Env<'_>,
    bytes: &Variant,
    locale: Locale,
    flags: u32,
    target: u16,
    api: &'static str,
) -> Result<Converted, u32> {
    let source = vt(bytes);
    if source == 1 {
        return Err(MISMATCH);
    }
    if source & 0x2000 != 0 || matches!(source, 9 | 13 | 36) {
        return Err(warn(
            env,
            api,
            "conversion requires an unavailable Automation object value",
            NOT_IMPLEMENTED,
        ));
    }
    if target == 8 {
        let text = match source {
            0 => String::new(),
            7 => date::format(
                f64::from_le_bytes(bytes[8..16].try_into().unwrap()),
                locale,
                0,
            )?,
            11 if flags & 0x12 != 0 => {
                if pointer(bytes) as u16 == 0 {
                    "False".into()
                } else {
                    "True".into()
                }
            }
            _ => number(env, bytes, locale)?.text(),
        };
        let pointer = alloc_bstr(env, &text.encode_utf16().collect::<Vec<_>>());
        if pointer == 0 {
            return Err(OUT_OF_MEMORY);
        }
        return Ok(Converted {
            kind: 8,
            payload: pointer.to_le_bytes().to_vec(),
            string: pointer,
        });
    }
    if target == 0 {
        return Ok(Converted::scalar(0, vec![]));
    }
    if target == 1 {
        return if source == 0 {
            Ok(Converted::scalar(1, vec![]))
        } else {
            Err(MISMATCH)
        };
    }
    if target == 7 && source == 8 {
        let text = super::super::coercion::text(env, pointer(bytes))?;
        let value = date::parse(env, &text, locale, 0)?;
        return Ok(Converted::scalar(7, value.to_le_bytes().to_vec()));
    }
    if target == 11 {
        let value = if source == 8 {
            boolean(&super::super::coercion::text(env, pointer(bytes))?, locale)?
        } else if number(env, bytes, locale)?.nonzero() {
            -1i16
        } else {
            0
        };
        return Ok(Converted::scalar(11, value.to_le_bytes().to_vec()));
    }
    let number = number(env, bytes, locale)?;
    let range = |_| OVERFLOW;
    let payload = match target {
        2 => i16::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        3 | 22 => i32::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        16 => i8::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        17 => u8::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        18 => u16::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        19 | 23 => u32::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        20 => i64::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        21 => u64::try_from(number.fixed(0)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        6 => i64::try_from(number.fixed(4)?)
            .map_err(range)?
            .to_le_bytes()
            .to_vec(),
        4 => {
            let n = number.floating()? as f32;
            if !n.is_finite() {
                return Err(OVERFLOW);
            }
            n.to_le_bytes().to_vec()
        }
        5 | 7 => {
            let n = number.floating()?;
            if target == 7 {
                date::validate(n)?;
            }
            n.to_le_bytes().to_vec()
        }
        _ => {
            return Err(warn(
                env,
                api,
                "requested scalar Automation target is unavailable",
                NOT_IMPLEMENTED,
            ));
        }
    };
    Ok(Converted::scalar(target, payload))
}
pub(in crate::misc::variant) fn value(
    env: &mut Env<'_>,
    destination: u32,
    source: u32,
    id: u32,
    flags: u32,
    target: u32,
    api: &'static str,
) -> Outcome {
    let locale = match locale(env, id, 0, api, 0) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let target = target as u16;
    if flags & !0xff != 0 {
        return Outcome::Return(INVALID);
    }
    if let Err(error) = supported(env, target, api) {
        return Outcome::Return(error);
    }
    if target & 0x6000 != 0 || matches!(target, 12 | 15) {
        return Outcome::Return(BAD_TYPE);
    }
    let bytes = match read(env, source, false)
        .and_then(|bytes| super::super::indirect::resolve(env, bytes))
    {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    if let Err(error) = supported(env, vt(&bytes), api) {
        return Outcome::Return(error);
    }
    if read(env, destination, true).is_err() {
        return Outcome::Return(INVALID);
    }
    if vt(&bytes) == target {
        return super::super::indirect::copy_ind_value(env, destination, source);
    }
    // Alternative-calendar transformations require additional calendar data.
    if flags & 0x28 != 0 {
        return Outcome::Return(warn(
            env,
            api,
            "requested Automation calendar is unavailable",
            NOT_IMPLEMENTED,
        ));
    }
    let converted = match convert(env, &bytes, locale, flags, target, api) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let outcome = clear_value(env, destination);
    complete(
        env,
        outcome,
        Box::new(move |env, result| {
            let result = if result == 0 {
                match read(env, destination, true) {
                    Ok(mut bytes) => {
                        bytes[..2].copy_from_slice(&converted.kind.to_le_bytes());
                        bytes[8..8 + converted.payload.len()].copy_from_slice(&converted.payload);
                        if env.cpu.write(destination, &bytes).is_ok() {
                            0
                        } else {
                            INVALID
                        }
                    }
                    Err(error) => error,
                }
            } else {
                result
            };
            if result != 0 && converted.string != 0 {
                free_bstr(env, converted.string);
            }
            Outcome::Return(result)
        }),
    )
}
#[win32(oleaut32,name="VariantChangeTypeEx",level=implement)]
pub(super) fn change_ex(
    env: &mut Env<'_>,
    destination: GuestPtr<Variant>,
    source: GuestPtr<Variant>,
    locale: u32,
    flags: u32,
    target: u32,
) -> Outcome {
    value(
        env,
        destination.addr(),
        source.addr(),
        locale,
        flags as u16 as u32,
        target,
        "oleaut32.dll!VariantChangeTypeEx",
    )
}
