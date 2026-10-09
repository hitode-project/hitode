//! Dynamic DllMain's loader-owned SEH boundary. Inner guest handlers run first;
//! an escaping exception unwinds back into this guest wrapper, which returns
//! normally to the host continuation responsible for rollback and loader locks.
use super::*;
use adv_abi::GuestCode;

const NAME: &str = "dll_init";
pub(super) const EXCEPTION: u32 = 2;

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, notifications::INTERNAL, NAME, 5, |env| {
        // Disabling direct guest API routes must retain the SEH boundary.
        let Some(body) = env.sys.thunks.get(env.thunk).guest_body() else {
            return Outcome::Fail("DLL initializer wrapper is missing".into());
        };
        match env.checkpoint() {
            Ok(mut checkpoint) => {
                checkpoint.state.eip = body;
                Outcome::ResumeExecution(checkpoint)
            }
            Err(error) => Outcome::Fail(error.to_string()),
        }
    });
    reg.set_guest(notifications::INTERNAL, NAME, &CODE);
}

pub(super) fn prepare(env: &mut Env<'_>, call: &mut InitCall) -> Result<(), String> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    // Without a resident kernel32 image, guest imports of RtlUnwind bind to the
    // published host entry; that entry is then the live public routine.
    let Some(id) = modules.by_name("kernel32.dll") else {
        let unwind = env
            .sys
            .thunks
            .resolve(env.cpu, "kernel32.dll", "RtlUnwind")
            .map_err(|error| error.to_string())?;
        return wrap(env, call, unwind);
    };
    let mut loader = Loader::new(env.cpu, env.sys, modules, &win32.vfs);
    // Resolve the live public entry, including EAT/entry hooks, just as guest
    // code would. Never bypass it with a private host implementation.
    let unwind = match loader.guest_override(id, "RtlUnwind") {
        Ok(Some(target)) => target,
        Ok(None) => {
            loader
                .resolve_dynamic(
                    id,
                    &ImportName::Name {
                        hint: 0,
                        name: "RtlUnwind".into(),
                    },
                )
                .map_err(|error| error.to_string())?
                .0
        }
        Err(error) => return Err(error.to_string()),
    };
    wrap(env, call, unwind)
}

fn wrap(env: &mut Env<'_>, call: &mut InitCall, unwind: u32) -> Result<(), String> {
    let target = env
        .sys
        .thunks
        .resolve(env.cpu, notifications::INTERNAL, NAME)
        .map_err(|error| error.to_string())?;
    call.args.insert(0, call.target);
    call.args.push(unwind);
    call.target = target;
    Ok(())
}

/// Authored x86 stdcall(target, module, reason, reserved, unwind). The four
/// registration words are previous, handler, recovery, and live RtlUnwind.
/// Ordinary BOOL returns normalize to 0/1; an unwound initializer returns 2.
/// RtlUnwind gets a new STATUS_UNWIND record (including for noncontinuable
/// exceptions), as observed in the native fixture. No faulting instruction is
/// resumed and no host loader continuation is discarded.
/// ```text
///     push ebp; push ebx; push esi; push edi
///     push [esp+36]               ; live unwind entry
///     call pic
/// pic: pop eax
///     lea edx, [eax+cleanup-pic]; lea eax, [eax+handler-pic]
///     push edx; push eax; push fs:[0]; mov fs:[0], esp
///     push [esp+48]; push [esp+48]; push [esp+48]
///     call [esp+48]               ; DllMain(module, reason, reserved)
///     test eax, eax; setne al; movzx eax, al
/// cleanup:
///     mov ecx, [esp]; mov fs:[0], ecx; add esp, 16
///     pop edi; pop esi; pop ebx; pop ebp; ret 20
/// handler:
///     mov ecx, [esp+4]; test dword [ecx+4], 6; jnz search
///     mov ebx, [esp+8]            ; establisher
///     push 0; push 0; push 0; push ebx; call [ebx+12]
///     mov esp, ebx; mov eax, 2; jmp [esp+8]
/// search: mov eax, 1; ret
/// ```
static CODE: GuestCode = GuestCode {
    code: &[
        0x55, 0x53, 0x56, 0x57, 0xff, 0x74, 0x24, 0x24, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x58, 0x8d,
        0x90, 0x35, 0x00, 0x00, 0x00, 0x8d, 0x80, 0x49, 0x00, 0x00, 0x00, 0x52, 0x50, 0x64, 0xff,
        0x35, 0x00, 0x00, 0x00, 0x00, 0x64, 0x89, 0x25, 0x00, 0x00, 0x00, 0x00, 0xff, 0x74, 0x24,
        0x30, 0xff, 0x74, 0x24, 0x30, 0xff, 0x74, 0x24, 0x30, 0xff, 0x54, 0x24, 0x30, 0x85, 0xc0,
        0x0f, 0x95, 0xc0, 0x0f, 0xb6, 0xc0, 0x8b, 0x0c, 0x24, 0x64, 0x89, 0x0d, 0x00, 0x00, 0x00,
        0x00, 0x83, 0xc4, 0x10, 0x5f, 0x5e, 0x5b, 0x5d, 0xc2, 0x14, 0x00, 0x8b, 0x4c, 0x24, 0x04,
        0xf7, 0x41, 0x04, 0x06, 0x00, 0x00, 0x00, 0x75, 0x19, 0x8b, 0x5c, 0x24, 0x08, 0x6a, 0x00,
        0x6a, 0x00, 0x6a, 0x00, 0x53, 0xff, 0x53, 0x0c, 0x89, 0xdc, 0xb8, 0x02, 0x00, 0x00, 0x00,
        0xff, 0x64, 0x24, 0x08, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3,
    ],
    host: &[],
    links: &[],
};
