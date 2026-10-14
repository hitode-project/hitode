//! Native-semantics x86 for small, hot APIs. A thunk jumps here instead of
//! trapping, so the call stays in generated code; rare cases (`host`) jump to
//! the registered handler, which remains the implementation of record. The
//! listings are the source; tests compare each one against Windows behavior.

use adv_abi::{GuestCode, Registry};
mod heap;

/// Attach after every handler is registered: re-registering a handler drops it.
pub(crate) fn install(reg: &mut Registry) {
    for (module, name, guest) in [
        ("ntdll.dll", "RtlAllocateHeap", &heap::ALLOCATE),
        ("ntdll.dll", "RtlFreeHeap", &heap::FREE),
        ("user32.dll", "SetRectEmpty", &SET_RECT_EMPTY),
        ("user32.dll", "IsRectEmpty", &IS_RECT_EMPTY),
        ("user32.dll", "IntersectRect", &INTERSECT_RECT),
        ("user32.dll", "UnionRect", &UNION_RECT),
        ("kernel32.dll", "GetLastError", &GET_LAST_ERROR),
        ("kernel32.dll", "SetLastError", &SET_LAST_ERROR),
        ("kernel32.dll", "DebugBreak", &DEBUG_BREAK),
        ("kernel32.dll", "TlsGetValue", &TLS_GET_VALUE),
        (
            "kernel32.dll",
            "EnterCriticalSection",
            &ENTER_CRITICAL_SECTION,
        ),
        (
            "kernel32.dll",
            "LeaveCriticalSection",
            &LEAVE_CRITICAL_SECTION,
        ),
        (
            "ntdll.dll",
            "RtlEnterCriticalSection",
            &ENTER_CRITICAL_SECTION,
        ),
        (
            "ntdll.dll",
            "RtlLeaveCriticalSection",
            &LEAVE_CRITICAL_SECTION,
        ),
    ] {
        reg.set_guest(module, name, guest);
    }
}

/// Authored `int3; ret`: the ordinary CPU exception path owns dispatch and
/// continuation, so an unhandled breakpoint cannot be silently ignored.
pub(crate) static DEBUG_BREAK: GuestCode = GuestCode {
    code: &[0xcc, 0xc3],
    host: &[],
    links: &[],
};

/// `user32.dll!SetRectEmpty`
/// ```text
///     mov eax, dword ptr [esp+4]
///     test eax, eax
///     jz null
///     xor ecx, ecx
///     mov dword ptr [eax], ecx
///     mov dword ptr [eax+4], ecx
///     mov dword ptr [eax+8], ecx
///     mov dword ptr [eax+12], ecx
///     mov eax, 1
///     ret 4
/// null:
///     ret 4
/// ```
pub(crate) static SET_RECT_EMPTY: GuestCode = GuestCode {
    code: &[
        0x8b, 0x44, 0x24, 0x04, 0x85, 0xc0, 0x74, 0x15, 0x31, 0xc9, 0x89, 0x08, 0x89, 0x48, 0x04,
        0x89, 0x48, 0x08, 0x89, 0x48, 0x0c, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc2, 0x04, 0x00, 0xc2,
        0x04, 0x00,
    ],
    host: &[],
    links: &[],
};

/// `user32.dll!IsRectEmpty`
/// ```text
///     mov ecx, dword ptr [esp+4]
///     mov eax, 1
///     test ecx, ecx
///     jz done
///     mov edx, dword ptr [ecx]
///     cmp edx, dword ptr [ecx+8]
///     jge done
///     mov edx, dword ptr [ecx+4]
///     cmp edx, dword ptr [ecx+12]
///     jge done
///     xor eax, eax
/// done:
///     ret 4
/// ```
pub(crate) static IS_RECT_EMPTY: GuestCode = GuestCode {
    code: &[
        0x8b, 0x4c, 0x24, 0x04, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x85, 0xc9, 0x74, 0x11, 0x8b, 0x11,
        0x3b, 0x51, 0x08, 0x7d, 0x0a, 0x8b, 0x51, 0x04, 0x3b, 0x51, 0x0c, 0x7d, 0x02, 0x31, 0xc0,
        0xc2, 0x04, 0x00,
    ],
    host: &[],
    links: &[],
};

/// `user32.dll!IntersectRect`
/// ```text
///     push ebx
///     push esi
///     push edi
///     push ebp
///     mov eax, dword ptr [esp+20]
///     mov esi, dword ptr [esp+24]
///     mov edi, dword ptr [esp+28]
///     test eax, eax
///     jz fail
///     test esi, esi
///     jz fail
///     test edi, edi
///     jz fail
///     mov ebx, dword ptr [esi]
///     mov ecx, dword ptr [edi]
///     cmp ebx, ecx
///     cmovl ebx, ecx
///     mov ecx, dword ptr [esi+4]
///     mov edx, dword ptr [edi+4]
///     cmp ecx, edx
///     cmovl ecx, edx
///     mov edx, dword ptr [esi+8]
///     mov ebp, dword ptr [edi+8]
///     cmp edx, ebp
///     cmovg edx, ebp
///     mov esi, dword ptr [esi+12]
///     mov edi, dword ptr [edi+12]
///     cmp esi, edi
///     cmovg esi, edi
///     cmp ebx, edx
///     jge empty
///     cmp ecx, esi
///     jge empty
///     mov dword ptr [eax], ebx
///     mov dword ptr [eax+4], ecx
///     mov dword ptr [eax+8], edx
///     mov dword ptr [eax+12], esi
///     mov eax, 1
///     jmp out
/// empty:
///     xor ecx, ecx
///     mov dword ptr [eax], ecx
///     mov dword ptr [eax+4], ecx
///     mov dword ptr [eax+8], ecx
///     mov dword ptr [eax+12], ecx
/// fail:
///     xor eax, eax
/// out:
///     pop ebp
///     pop edi
///     pop esi
///     pop ebx
///     ret 12
/// ```
pub(crate) static INTERSECT_RECT: GuestCode = GuestCode {
    code: &[
        0x53, 0x56, 0x57, 0x55, 0x8b, 0x44, 0x24, 0x14, 0x8b, 0x74, 0x24, 0x18, 0x8b, 0x7c, 0x24,
        0x1c, 0x85, 0xc0, 0x74, 0x59, 0x85, 0xf6, 0x74, 0x55, 0x85, 0xff, 0x74, 0x51, 0x8b, 0x1e,
        0x8b, 0x0f, 0x39, 0xcb, 0x0f, 0x4c, 0xd9, 0x8b, 0x4e, 0x04, 0x8b, 0x57, 0x04, 0x39, 0xd1,
        0x0f, 0x4c, 0xca, 0x8b, 0x56, 0x08, 0x8b, 0x6f, 0x08, 0x39, 0xea, 0x0f, 0x4f, 0xd5, 0x8b,
        0x76, 0x0c, 0x8b, 0x7f, 0x0c, 0x39, 0xfe, 0x0f, 0x4f, 0xf7, 0x39, 0xd3, 0x7d, 0x16, 0x39,
        0xf1, 0x7d, 0x12, 0x89, 0x18, 0x89, 0x48, 0x04, 0x89, 0x50, 0x08, 0x89, 0x70, 0x0c, 0xb8,
        0x01, 0x00, 0x00, 0x00, 0xeb, 0x0f, 0x31, 0xc9, 0x89, 0x08, 0x89, 0x48, 0x04, 0x89, 0x48,
        0x08, 0x89, 0x48, 0x0c, 0x31, 0xc0, 0x5d, 0x5f, 0x5e, 0x5b, 0xc2, 0x0c, 0x00,
    ],
    host: &[],
    links: &[],
};

/// `user32.dll!UnionRect`
/// ```text
///     push ebx
///     push esi
///     push edi
///     push ebp
///     mov eax, dword ptr [esp+20]
///     mov esi, dword ptr [esp+24]
///     mov edi, dword ptr [esp+28]
///     test eax, eax
///     jz fail
///     test esi, esi
///     jz fail
///     test edi, edi
///     jz fail
///     mov ecx, dword ptr [esi]
///     cmp ecx, dword ptr [esi+8]
///     jge first_empty
///     mov ecx, dword ptr [esi+4]
///     cmp ecx, dword ptr [esi+12]
///     jge first_empty
///     mov ecx, dword ptr [edi]
///     cmp ecx, dword ptr [edi+8]
///     jge copy
///     mov ecx, dword ptr [edi+4]
///     cmp ecx, dword ptr [edi+12]
///     jge copy
///     mov ebx, dword ptr [esi]
///     mov ecx, dword ptr [edi]
///     cmp ebx, ecx
///     cmovg ebx, ecx
///     mov ecx, dword ptr [esi+4]
///     mov edx, dword ptr [edi+4]
///     cmp ecx, edx
///     cmovg ecx, edx
///     mov edx, dword ptr [esi+8]
///     mov ebp, dword ptr [edi+8]
///     cmp edx, ebp
///     cmovl edx, ebp
///     mov esi, dword ptr [esi+12]
///     mov edi, dword ptr [edi+12]
///     cmp esi, edi
///     cmovl esi, edi
///     jmp store
/// first_empty:
///     mov ecx, dword ptr [edi]
///     cmp ecx, dword ptr [edi+8]
///     jge both_empty
///     mov ecx, dword ptr [edi+4]
///     cmp ecx, dword ptr [edi+12]
///     jge both_empty
///     mov esi, edi
/// copy:
///     mov ebx, dword ptr [esi]
///     mov ecx, dword ptr [esi+4]
///     mov edx, dword ptr [esi+8]
///     mov esi, dword ptr [esi+12]
/// store:
///     mov dword ptr [eax], ebx
///     mov dword ptr [eax+4], ecx
///     mov dword ptr [eax+8], edx
///     mov dword ptr [eax+12], esi
///     mov eax, 1
///     jmp out
/// both_empty:
///     xor ecx, ecx
///     mov dword ptr [eax], ecx
///     mov dword ptr [eax+4], ecx
///     mov dword ptr [eax+8], ecx
///     mov dword ptr [eax+12], ecx
/// fail:
///     xor eax, eax
/// out:
///     pop ebp
///     pop edi
///     pop esi
///     pop ebx
///     ret 12
/// ```
pub(crate) static UNION_RECT: GuestCode = GuestCode {
    code: &[
        0x53, 0x56, 0x57, 0x55, 0x8b, 0x44, 0x24, 0x14, 0x8b, 0x74, 0x24, 0x18, 0x8b, 0x7c, 0x24,
        0x1c, 0x85, 0xc0, 0x0f, 0x84, 0x95, 0x00, 0x00, 0x00, 0x85, 0xf6, 0x0f, 0x84, 0x8d, 0x00,
        0x00, 0x00, 0x85, 0xff, 0x0f, 0x84, 0x85, 0x00, 0x00, 0x00, 0x8b, 0x0e, 0x3b, 0x4e, 0x08,
        0x7d, 0x43, 0x8b, 0x4e, 0x04, 0x3b, 0x4e, 0x0c, 0x7d, 0x3b, 0x8b, 0x0f, 0x3b, 0x4f, 0x08,
        0x7d, 0x45, 0x8b, 0x4f, 0x04, 0x3b, 0x4f, 0x0c, 0x7d, 0x3d, 0x8b, 0x1e, 0x8b, 0x0f, 0x39,
        0xcb, 0x0f, 0x4f, 0xd9, 0x8b, 0x4e, 0x04, 0x8b, 0x57, 0x04, 0x39, 0xd1, 0x0f, 0x4f, 0xca,
        0x8b, 0x56, 0x08, 0x8b, 0x6f, 0x08, 0x39, 0xea, 0x0f, 0x4c, 0xd5, 0x8b, 0x76, 0x0c, 0x8b,
        0x7f, 0x0c, 0x39, 0xfe, 0x0f, 0x4c, 0xf7, 0xeb, 0x1c, 0x8b, 0x0f, 0x3b, 0x4f, 0x08, 0x7d,
        0x27, 0x8b, 0x4f, 0x04, 0x3b, 0x4f, 0x0c, 0x7d, 0x1f, 0x89, 0xfe, 0x8b, 0x1e, 0x8b, 0x4e,
        0x04, 0x8b, 0x56, 0x08, 0x8b, 0x76, 0x0c, 0x89, 0x18, 0x89, 0x48, 0x04, 0x89, 0x50, 0x08,
        0x89, 0x70, 0x0c, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xeb, 0x0f, 0x31, 0xc9, 0x89, 0x08, 0x89,
        0x48, 0x04, 0x89, 0x48, 0x08, 0x89, 0x48, 0x0c, 0x31, 0xc0, 0x5d, 0x5f, 0x5e, 0x5b, 0xc2,
        0x0c, 0x00,
    ],
    host: &[],
    links: &[],
};

/// `kernel32.dll!GetLastError`
/// ```text
///     mov eax, dword ptr fs:[0x34]
///     ret
/// ```
pub(crate) static GET_LAST_ERROR: GuestCode = GuestCode {
    code: &[0x64, 0xa1, 0x34, 0x00, 0x00, 0x00, 0xc3],
    host: &[],
    links: &[],
};

/// `kernel32.dll!SetLastError`
/// ```text
///     mov eax, dword ptr [esp+4]
///     mov dword ptr fs:[0x34], eax
///     ret 4
/// ```
pub(crate) static SET_LAST_ERROR: GuestCode = GuestCode {
    code: &[
        0x8b, 0x44, 0x24, 0x04, 0x64, 0xa3, 0x34, 0x00, 0x00, 0x00, 0xc2, 0x04, 0x00,
    ],
    host: &[],
    links: &[],
};

/// `kernel32.dll!TlsGetValue`
/// ```text
///     mov ecx, dword ptr [esp+4]
///     cmp ecx, 64
///     jae HOST
///     mov eax, dword ptr fs:[0xe10+ecx*4]
///     mov dword ptr fs:[0x34], 0
///     ret 4
/// ```
pub(crate) static TLS_GET_VALUE: GuestCode = GuestCode {
    code: &[
        0x8b, 0x4c, 0x24, 0x04, 0x83, 0xf9, 0x40, 0x0f, 0x83, 0xf3, 0xff, 0xff, 0x3f, 0x64, 0x8b,
        0x04, 0x8d, 0x10, 0x0e, 0x00, 0x00, 0x64, 0xc7, 0x05, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xc2, 0x04, 0x00,
    ],
    host: &[9],
    links: &[],
};

/// `kernel32.dll!EnterCriticalSection`
/// ```text
///     mov edx, dword ptr [esp+4]
///     mov ecx, dword ptr fs:[0x24]
///     mov eax, 1
///     lock xadd dword ptr [edx+4], eax
///     inc eax
///     jnz busy
///     mov dword ptr [edx+12], ecx
///     mov dword ptr [edx+8], 1
///     xor eax, eax
///     ret 4
/// busy:
///     cmp dword ptr [edx+12], ecx
///     jne LINK:kernel32.dll!<EnterCriticalSectionWait>
///     inc dword ptr [edx+8]
///     xor eax, eax
///     ret 4
/// ```
pub(crate) static ENTER_CRITICAL_SECTION: GuestCode = GuestCode {
    code: &[
        0x8b, 0x54, 0x24, 0x04, 0x64, 0x8b, 0x0d, 0x24, 0x00, 0x00, 0x00, 0xb8, 0x01, 0x00, 0x00,
        0x00, 0xf0, 0x0f, 0xc1, 0x42, 0x04, 0x40, 0x75, 0x0f, 0x89, 0x4a, 0x0c, 0xc7, 0x42, 0x08,
        0x01, 0x00, 0x00, 0x00, 0x31, 0xc0, 0xc2, 0x04, 0x00, 0x39, 0x4a, 0x0c, 0x0f, 0x85, 0xd2,
        0x0f, 0x00, 0x50, 0xff, 0x42, 0x08, 0x31, 0xc0, 0xc2, 0x04, 0x00,
    ],
    host: &[],
    links: &[(44, "kernel32.dll", "<EnterCriticalSectionWait>")],
};

/// `kernel32.dll!LeaveCriticalSection`
/// ```text
///     mov edx, dword ptr [esp+4]
///     mov ecx, dword ptr fs:[0x24]
///     cmp dword ptr [edx+12], ecx
///     jne HOST
///     dec dword ptr [edx+8]
///     jnz held
///     mov dword ptr [edx+12], 0
///     lock dec dword ptr [edx+4]
///     jns LINK:kernel32.dll!<LeaveCriticalSectionWake>
///     xor eax, eax
///     ret 4
/// held:
///     lock dec dword ptr [edx+4]
///     xor eax, eax
///     ret 4
/// ```
pub(crate) static LEAVE_CRITICAL_SECTION: GuestCode = GuestCode {
    code: &[
        0x8b, 0x54, 0x24, 0x04, 0x64, 0x8b, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x39, 0x4a, 0x0c, 0x0f,
        0x85, 0xec, 0xff, 0xff, 0x3f, 0xff, 0x4a, 0x08, 0x75, 0x16, 0xc7, 0x42, 0x0c, 0x00, 0x00,
        0x00, 0x00, 0xf0, 0xff, 0x4a, 0x04, 0x0f, 0x89, 0xd6, 0x0f, 0x00, 0x50, 0x31, 0xc0, 0xc2,
        0x04, 0x00, 0xf0, 0xff, 0x4a, 0x04, 0x31, 0xc0, 0xc2, 0x04, 0x00,
    ],
    host: &[16],
    links: &[(38, "kernel32.dll", "<LeaveCriticalSectionWake>")],
};
