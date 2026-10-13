//! Native Windows linguistic services; the portable fallback retains its
//! existing code-unit profile until portable Windows sort weights are supplied.
use adv_os::Codec;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "CompareStringA"]
    fn native_compare_ansi(
        locale: u32,
        flags: u32,
        a: *const u8,
        an: i32,
        b: *const u8,
        bn: i32,
    ) -> i32;
    #[link_name = "CompareStringW"]
    fn native_compare(
        locale: u32,
        flags: u32,
        a: *const u16,
        an: i32,
        b: *const u16,
        bn: i32,
    ) -> i32;
    #[link_name = "LocaleNameToLCID"]
    fn native_locale(name: *const u16, flags: u32) -> u32;
    #[link_name = "GetLocaleInfoW"]
    fn native_info(locale: u32, kind: u32, output: *mut u16, count: i32) -> i32;
    #[link_name = "MultiByteToWideChar"]
    fn native_decode(
        cp: u32,
        flags: u32,
        bytes: *const u8,
        count: i32,
        units: *mut u16,
        capacity: i32,
    ) -> i32;
    #[link_name = "GetLastError"]
    fn native_error() -> u32;
}

pub(super) fn compare_ansi(
    locale: u32,
    flags: u32,
    codec: Codec,
    a: &[u8],
    b: &[u8],
) -> Result<u32, u32> {
    #[cfg(windows)]
    {
        let an = i32::try_from(a.len()).map_err(|_| 87u32)?;
        let bn = i32::try_from(b.len()).map_err(|_| 87u32)?;
        if flags & 0x40000000 == 0 {
            let result =
                unsafe { native_compare_ansi(locale, flags, a.as_ptr(), an, b.as_ptr(), bn) };
            return if result == 0 {
                Err(unsafe { native_error() })
            } else {
                Ok(result as u32)
            };
        }
        // Validate ANSI-only flag restrictions before using the guest ACP.
        if unsafe { native_compare_ansi(locale, flags, b"a".as_ptr(), 1, b"a".as_ptr(), 1) } == 0 {
            return Err(unsafe { native_error() });
        }
    }
    let cp = if flags & 0x40000000 != 0 {
        codec.acp
    } else {
        codepage(locale, codec.acp)?
    };
    let (a, b) = (decode(codec, cp, a)?, decode(codec, cp, b)?);
    compare(locale, flags & !0x40000000, &a, &b)
}

pub(super) fn compare(locale: u32, flags: u32, a: &[u16], b: &[u16]) -> Result<u32, u32> {
    #[cfg(windows)]
    {
        let an = i32::try_from(a.len()).map_err(|_| 87u32)?;
        let bn = i32::try_from(b.len()).map_err(|_| 87u32)?;
        // Only owned host slices cross the native boundary, never guest pointers.
        let result = unsafe { native_compare(locale, flags, a.as_ptr(), an, b.as_ptr(), bn) };
        if result == 0 {
            Err(unsafe { native_error() })
        } else {
            Ok(result as u32)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = locale;
        if flags & !1 != 0 {
            return Err(1004);
        }
        let fold = |u: u16| {
            if flags & 1 != 0 {
                adv_os::nls::unit_case(u, false)
            } else {
                u
            }
        };
        Ok((a.iter().map(|&u| fold(u)).cmp(b.iter().map(|&u| fold(u))) as i32 + 2) as u32)
    }
}

pub(super) fn locale(name: &[u16], default: u32) -> Result<u32, u32> {
    if name.is_empty() {
        return Ok(0x7f);
    }
    if String::from_utf16_lossy(name) == "!x-sys-default-locale" {
        return Ok(default);
    }
    #[cfg(windows)]
    {
        let name: Vec<_> = name.iter().copied().chain([0]).collect();
        let locale = unsafe { native_locale(name.as_ptr(), 0x08000000) };
        if locale == 0 {
            Err(unsafe { native_error() })
        } else {
            Ok(locale)
        }
    }
    #[cfg(not(windows))]
    {
        match String::from_utf16_lossy(name).to_ascii_lowercase().as_str() {
            "ja-jp" => Ok(0x411),
            "zh-cn" => Ok(0x804),
            "en-us" => Ok(0x409),
            _ => Err(87),
        }
    }
}

pub(super) fn codepage(locale: u32, default: u32) -> Result<u32, u32> {
    #[cfg(windows)]
    {
        let mut cp = 0u32;
        let result = unsafe { native_info(locale, 0x20001004, (&mut cp as *mut u32).cast(), 2) };
        if result == 0 {
            Err(unsafe { native_error() })
        } else {
            Ok(if cp == 0 { default } else { cp })
        }
    }
    #[cfg(not(windows))]
    {
        Ok(match locale & 0xffff {
            0x411 => 932,
            0x804 => 936,
            _ => default,
        })
    }
}

pub(super) fn decode(codec: Codec, cp: u32, bytes: &[u8]) -> Result<Vec<u16>, u32> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(windows)]
    {
        let _ = codec;
        let count = i32::try_from(bytes.len()).map_err(|_| 87u32)?;
        let length =
            unsafe { native_decode(cp, 0, bytes.as_ptr(), count, std::ptr::null_mut(), 0) };
        if length == 0 {
            return Err(unsafe { native_error() });
        }
        let mut units = vec![0; length as usize];
        if unsafe { native_decode(cp, 0, bytes.as_ptr(), count, units.as_mut_ptr(), length) } == 0 {
            Err(unsafe { native_error() })
        } else {
            Ok(units)
        }
    }
    #[cfg(not(windows))]
    {
        Ok(codec.to_utf16(cp, bytes))
    }
}
