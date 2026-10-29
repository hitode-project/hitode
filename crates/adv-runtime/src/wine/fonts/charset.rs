//! Queries the realized font shared with TEXTMETRIC and glyph rasterization.
use super::*;

const DEFAULT_CHARSET: i32 = 1;

pub(super) fn register(registry: &mut Registry) {
    registry.register(text_charset_ABI);
    registry.register(text_charset_info_ABI);
}

#[win32(gdi32, name = "GetTextCharset", level = implement)]
pub(super) fn text_charset(env: &mut Env<'_>, dc: Handle) -> i32 {
    query(env, dc.0, "gdi32.dll!GetTextCharset")
}

#[win32(gdi32, name = "GetTextCharsetInfo", level = implement)]
pub(super) fn text_charset_info(
    env: &mut Env<'_>,
    dc: Handle,
    signature: GuestPtr<()>,
    flags: u32,
) -> i32 {
    if reject_dc(env, dc.0) {
        return DEFAULT_CHARSET;
    }
    let _ = flags;
    if !signature.is_null() {
        let request = font_request(env, dc.0);
        let Ok(fs) = env.ext.get_mut::<Fonts>().text.signature(&request) else {
            return DEFAULT_CHARSET;
        };
        let bytes: Vec<_> = fs
            .unicode
            .into_iter()
            .chain(fs.codepages)
            .flat_map(u32::to_le_bytes)
            .collect();
        if env.cpu.write(signature.addr(), &bytes).is_err() {
            set_last_error(env, 998);
            return DEFAULT_CHARSET;
        }
    }
    query(env, dc.0, "gdi32.dll!GetTextCharsetInfo")
}

pub(super) fn query(env: &mut Env<'_>, dc: u32, api: &str) -> i32 {
    if reject_dc(env, dc) {
        return DEFAULT_CHARSET;
    }
    let request = font_request(env, dc);
    match env.ext.get_mut::<Fonts>().text.metrics(&request) {
        Ok(metrics) => i32::from(encode_encoding(metrics.encoding)),
        Err(error) => {
            adv_abi::warn_unavailable(
                env,
                api,
                &format!("realized font charset unavailable: {error}"),
            );
            set_last_error(
                env,
                if error == TextError::Unavailable {
                    120
                } else {
                    87
                },
            );
            DEFAULT_CHARSET
        }
    }
}
