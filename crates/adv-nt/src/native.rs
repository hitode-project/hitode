//! Native bodies of distribution functions.
//!
//! A function of a distribution image whose bytes its code identity table
//! confirms ([`crate::identity`]) can have a host body. Translated calls to
//! the function branch to a thunk instead ([`adv_machine::Sys::redirects`]),
//! which runs the body as a leaf call, without leaving translated code. The
//! interpreter runs Wine's own code, and so does a call the body declines:
//! it resumes at a trampoline holding the function's first instruction,
//! which jumps to the second. A body therefore has the function's effect
//! whenever it accepts a call, and declines anything else, such as memory
//! the function would fault on.

use std::collections::HashMap;

use adv_cpu::backend::{Guest, Prot};
use adv_machine::layout::PAGE;
use adv_machine::{Conv, Env, EnvLeafHandler, Ext, HostFn, Outcome, Sys};
use iced_x86::{Decoder, DecoderOptions, FlowControl};

use crate::identity::{self, Images};

mod crt;
mod mixer;
mod nls;

/// The module the bodies' thunks are registered under.
pub const MODULE: &str = "hitode.native";

struct Body {
    /// The image and function, which also names the thunk.
    name: &'static str,
    conv: Conv,
    leaf: EnvLeafHandler,
    handler: adv_machine::Handler,
}

const BODIES: &[Body] = &[
    // Wine's i386 memcpy copies byte by byte and allows overlap, as
    // memmove does, which jumps to it.
    Body {
        name: "ntdll.dll!memcpy",
        conv: Conv::Cdecl { argc: 3 },
        leaf: memmove,
        handler: resume_original,
    },
    Body {
        name: "dsound.dll!DSOUND_MixToPrimary",
        conv: Conv::Cdecl { argc: 6 },
        leaf: mixer::mix,
        handler: mixer::resume,
    },
    Body {
        name: "ntdll.dll!NtQueryPerformanceCounter",
        conv: Conv::Stdcall { argc: 2 },
        leaf: query_performance_counter,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr_nt,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcrt.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp_sign,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "ucrtbase.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr70.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr71.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr80.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr90.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr100.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr110.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcsnlen",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsnlen,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcr120.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!memset",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memset,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!memmove",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memmove,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!memcmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!memchr",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::memchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strlen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::strlen,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcslen",
        conv: Conv::Cdecl { argc: 1 },
        leaf: crt::wcslen,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strcmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::strncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcscmp",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscmp_sign,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcsncmp",
        conv: Conv::Cdecl { argc: 3 },
        leaf: crt::wcsncmp,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcschr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcschr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcsrchr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsrchr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strcpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcpy,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcscpy",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscpy,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strcat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strcat,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcscat",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcscat,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!strstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::strstr,
        handler: resume_original,
    },
    Body {
        name: "msvcrtd.dll!wcsstr",
        conv: Conv::Cdecl { argc: 2 },
        leaf: crt::wcsstr,
        handler: resume_original,
    },
    Body {
        name: "kernelbase.dll!MultiByteToWideChar",
        conv: Conv::Stdcall { argc: 6 },
        leaf: nls::multi_byte_to_wide_char,
        handler: resume_original,
    },
    Body {
        name: "kernelbase.dll!WideCharToMultiByte",
        conv: Conv::Stdcall { argc: 8 },
        leaf: nls::wide_char_to_multi_byte,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlMultiByteToUnicodeN",
        conv: Conv::Stdcall { argc: 5 },
        leaf: nls::rtl_multi_byte_to_unicode_n,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlUnicodeToMultiByteN",
        conv: Conv::Stdcall { argc: 5 },
        leaf: nls::rtl_unicode_to_multi_byte_n,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlMultiByteToUnicodeSize",
        conv: Conv::Stdcall { argc: 3 },
        leaf: nls::rtl_multi_byte_to_unicode_size,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlUnicodeToMultiByteSize",
        conv: Conv::Stdcall { argc: 3 },
        leaf: nls::rtl_unicode_to_multi_byte_size,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlOemToUnicodeN",
        conv: Conv::Stdcall { argc: 5 },
        leaf: nls::rtl_oem_to_unicode_n,
        handler: resume_original,
    },
    Body {
        name: "ntdll.dll!RtlUnicodeToOemN",
        conv: Conv::Stdcall { argc: 5 },
        leaf: nls::rtl_unicode_to_oem_n,
        handler: resume_original,
    },
];

/// The host functions of the bodies.
pub(crate) fn host_fn(name: &str) -> Option<HostFn> {
    let body = BODIES.iter().find(|body| body.name == name)?;
    Some(HostFn {
        leaf: None,
        env_leaf: Some(body.leaf),
        conv: body.conv,
        handler: body.handler,
        data: 0,
        trace: false,
        guest: None,
    })
}

/// The trampolines of this process: their page, its used bytes, and each
/// bound function's trampoline by thunk index.
#[derive(Default)]
struct Trampolines {
    page: u32,
    used: u32,
    by_thunk: HashMap<u32, u32>,
}

/// Binds the bodies of the image `pe`, mapped at `base`.
pub(crate) fn bind(cpu: &mut dyn Guest, sys: &mut Sys, ext: &mut Ext, base: u32, pe: &adv_pe::Pe) {
    let Some(module) = ext
        .try_get::<Images>()
        .and_then(|images| images.0.get(&base))
        .cloned()
    else {
        return;
    };
    if !BODIES
        .iter()
        .any(|body| body.name.starts_with(module.name.as_str()))
    {
        return;
    }
    let Some(table) = module.table() else {
        return;
    };
    let mut relocations = pe.relocations().unwrap_or_default();
    relocations.sort_unstable();
    for body in BODIES {
        let Some(name) = body
            .name
            .strip_prefix(module.name.as_str())
            .and_then(|name| name.strip_prefix('!'))
        else {
            continue;
        };
        let Some(function) = table.named(name) else {
            continue;
        };
        let entry = base + function.rva;
        let relocated = |address: u32| {
            relocations
                .binary_search(&address.wrapping_sub(base))
                .is_ok()
        };
        if !identity::intact(cpu, entry, function, relocated) {
            log::debug!("{}: bytes differ from the distribution", body.name);
            continue;
        }
        if let Err(error) = bind_one(cpu, sys, ext, body.name, entry, function.size) {
            log::warn!("{}: no native body: {error}", body.name);
        }
    }
}

fn bind_one(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    name: &str,
    entry: u32,
    size: u32,
) -> Result<(), String> {
    let mut original = vec![0; size as usize];
    cpu.read(entry, &mut original).map_err(|e| e.to_string())?;
    let first = Decoder::with_ip(32, &original, u64::from(entry), DecoderOptions::NONE).decode();
    if first.is_invalid() || first.flow_control() != FlowControl::Next {
        return Err("the first instruction cannot move".into());
    }
    let length = first.len();
    let mut trampoline = original[..length].to_vec();
    trampoline.push(0xe9);
    let resume = entry + length as u32;
    let thunk = sys
        .thunks
        .publish(cpu, MODULE, name)
        .map_err(|e| e.to_string())?;
    let index = sys.thunks.published_index(MODULE, name).ok_or("no thunk")?;
    let trampolines = ext.get_or_default::<Trampolines>();
    if trampolines.page == 0 || trampolines.used + trampoline.len() as u32 + 4 > PAGE {
        trampolines.page = sys
            .vm
            .alloc(cpu, PAGE, Prot::RWX)
            .map_err(|e| e.to_string())?;
        trampolines.used = 0;
    }
    let at = trampolines.page + trampolines.used;
    let next = at + trampoline.len() as u32 + 4;
    trampoline.extend_from_slice(&resume.wrapping_sub(next).to_le_bytes());
    cpu.write(at, &trampoline).map_err(|e| e.to_string())?;
    trampolines.used = next - trampolines.page;
    trampolines.by_thunk.insert(index, at);
    let target = if name == "dsound.dll!DSOUND_MixToPrimary" {
        mixer::entry(cpu, sys, thunk)?
    } else {
        thunk
    };
    sys.redirects.insert(entry, (target, original.into()));
    Ok(())
}

/// The ordinary handler of a body's thunk, entered when the body declined:
/// the function runs from its trampoline with the caller's frame.
fn resume_original(env: &mut Env<'_>) -> Outcome {
    let Some(&trampoline) = env
        .ext
        .get_or_default::<Trampolines>()
        .by_thunk
        .get(&env.thunk)
    else {
        return Outcome::Fail("native body without a trampoline".into());
    };
    match env.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state.eip = trampoline;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

/// memmove(dst, src, n): EAX and EDX are `dst`, as Wine's code leaves them.
fn memmove(env: &mut Env<'_>) -> Option<u64> {
    let (dst, src, n) = (env.arg(0), env.arg(1), env.arg(2));
    if n != 0 {
        env.cpu.copy_memory(src, dst, n as usize).ok()?;
    }
    Some(u64::from(dst) << 32 | u64::from(dst))
}

// spec: NAT-BODY-2
fn query_performance_counter(env: &mut Env<'_>) -> Option<u64> {
    let (counter, frequency) = (env.arg(0), env.arg(1));
    if counter == 0
        || !env.cpu.accessible(counter, 8, Prot::WRITE)
        || (frequency != 0 && !env.cpu.accessible(frequency, 8, Prot::WRITE))
    {
        return None;
    }
    // The syscall boundary's system-time reads advance the virtual clock.
    if env.sys.clock.is_virtual() {
        crate::kuser::refresh(env);
        crate::server::set_time(env);
    }
    let ticks = crate::system::performance_counter(env);
    env.cpu.write(counter, &ticks.to_le_bytes()).ok()?;
    if frequency != 0 {
        env.cpu
            .write(frequency, &crate::system::TICKS_PER_SECOND.to_le_bytes())
            .ok()?;
    }
    Some(u64::from(crate::status::SUCCESS))
}
