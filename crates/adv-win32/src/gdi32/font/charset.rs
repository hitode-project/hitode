//! Queries the realized font shared with TEXTMETRIC and glyph rasterization.
use super::*;

const DEFAULT_CHARSET: i32 = 1;

pub(super) fn register(registry: &mut Registry) {
    registry.register(text_charset_ABI);
    registry.register(text_charset_info_ABI);
}

#[win32(gdi32, name = "GetTextCharset", level = implement)]
fn text_charset(env: &mut Env<'_>, dc: Handle) -> i32 {
    query(env, dc.0, "gdi32.dll!GetTextCharset")
}

#[win32(gdi32, name = "GetTextCharsetInfo", level = implement)]
fn text_charset_info(env: &mut Env<'_>, dc: Handle, signature: GuestPtr<()>, flags: u32) -> i32 {
    if reject_dc(env, dc.0) {
        return DEFAULT_CHARSET;
    }
    // The SDK reserves this argument and requires zero. Other values have no
    // supported contract, even where a native implementation ignores them.
    if flags != 0 {
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!GetTextCharsetInfo",
            "reserved nonzero charset-query flags are unsupported",
        );
        set_last_error(env, 87);
        return DEFAULT_CHARSET;
    }
    if !signature.is_null() {
        if adv_abi::guest::check_range(env.cpu, signature.addr(), 24, adv_cpu::backend::Prot::WRITE)
            .is_err()
        {
            set_last_error(env, 998);
            return DEFAULT_CHARSET;
        }
        // Enumeration has coverage for enumerated faces, but that does not
        // identify the actual face chosen by the selected font mapper. Never
        // publish a fabricated all-zero or generic TrueType signature.
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!GetTextCharsetInfo",
            "selected realized-font coverage signature is not exposed by the text backend",
        );
        set_last_error(env, 50);
        return DEFAULT_CHARSET;
    }
    query(env, dc.0, "gdi32.dll!GetTextCharsetInfo")
}

fn query(env: &mut Env<'_>, dc: u32, api: &str) -> i32 {
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
