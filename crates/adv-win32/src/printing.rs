//! The configured local print server owns no printers, jobs or device modes.
//! Handles refer to that server alone; no host spooler or printer is opened.
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Namespace, Outcome};

struct Servers(Namespace);
struct PrintServer {
    _access: u32,
}
fn fail(env: &mut Env<'_>, error: u32, result: u32) -> Outcome {
    crate::util::set_last_error(env, error);
    Outcome::Return(result)
}
fn text(env: &Env<'_>, pointer: u32) -> Result<String, u32> {
    let units = crate::network::text(env, pointer, false)?;
    String::from_utf16(&units).map_err(|_| 87)
}
fn valid(env: &Env<'_>, handle: u32) -> bool {
    env.ext.try_get::<Servers>().is_some_and(|servers| {
        env.sys
            .objects
            .lookup_alias::<PrintServer>(servers.0, handle)
            .is_some()
    })
}
fn open(env: &mut Env<'_>) -> Outcome {
    let (name, output, defaults) = (env.arg(0), env.arg(1), env.arg(2));
    if output == 0 || check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return fail(env, 87, 0);
    }
    let _ = env.cpu.write_u32(output, 0);
    let name = match text(env, name) {
        Ok(name) => name,
        Err(error) => return fail(env, error, 0),
    };
    let local = name.eq_ignore_ascii_case(&format!("\\\\{}", adv_os::identity::COMPUTER_NAME));
    if env.arg(0) != 0 && !local {
        return fail(env, 1801, 0);
    }
    let mut access = 2;
    if defaults != 0 {
        if check_range(env.cpu, defaults, 12, Prot::READ).is_err() {
            return fail(env, 87, 0);
        }
        access = env.cpu.read_u32(defaults + 8).unwrap();
        if access & 0x80000000 != 0 {
            access = (access & !0x80000000) | 0x20002;
        }
        if access & 0x02000000 != 0 {
            access = (access & !0x02000000) | 0x20002;
        }
        if access & !0x20002 != 0 {
            return fail(env, 5, 0);
        }
    }
    let namespace = if let Some(servers) = env.ext.try_get::<Servers>() {
        servers.0
    } else {
        let Some(namespace) = env.sys.objects.new_namespace_in(0x1100_0004, 0x11ff_fffc) else {
            return fail(env, 8, 0);
        };
        env.ext.insert(Servers(namespace));
        namespace
    };
    let Some(handle) = env
        .sys
        .objects
        .insert_host_in(namespace, PrintServer { _access: access })
    else {
        return fail(env, 8, 0);
    };
    if env.cpu.write_u32(output, handle).is_err() {
        env.sys.objects.close_alias(namespace, handle);
        return fail(env, 998, 0);
    }
    Outcome::Return(1)
}
fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    if !valid(env, handle) {
        return fail(env, 6, 0);
    }
    env.sys
        .objects
        .close_alias(env.ext.get::<Servers>().0, handle);
    Outcome::Return(1)
}
fn enumerate(env: &mut Env<'_>) -> Outcome {
    let flags = env.arg(0);
    let level = env.arg(2);
    if !matches!(level, 1 | 2 | 4 | 5) {
        return fail(env, 124, 0);
    }
    if flags & !0x7e != 0 {
        return fail(env, 1004, 0);
    }
    if flags & (0x10 | 0x40) != 0 {
        return fail(env, 53, 0);
    }
    if flags & 8 != 0 {
        let name = match text(env, env.arg(1)) {
            Ok(name) => name,
            Err(error) => return fail(env, error, 0),
        };
        if !name.is_empty()
            && !name.eq_ignore_ascii_case(&format!("\\\\{}", adv_os::identity::COMPUTER_NAME))
        {
            return fail(env, 53, 0);
        }
        adv_abi::warn_unavailable(
            env,
            "winspool.drv!EnumPrintersA",
            "print-provider namespace enumeration is unavailable",
        );
        return fail(env, 50, 0);
    }
    // There are no installed or connected printers in the guest profile.
    // Empty results require no output data, including a NULL zero-size probe.
    let (needed, returned) = (env.arg(5), env.arg(6));
    if [needed, returned]
        .into_iter()
        .any(|p| p == 0 || check_range(env.cpu, p, 4, Prot::WRITE).is_err())
    {
        return fail(env, 87, 0);
    }
    let _ = env.cpu.write_u32(needed, 0);
    let _ = env.cpu.write_u32(returned, 0);
    Outcome::Return(1)
}
fn properties(env: &mut Env<'_>) -> Outcome {
    if env.arg(1) == 0 && env.arg(2) != 0 {
        if let Err(error) = text(env, env.arg(2)) {
            return fail(env, error, u32::MAX);
        }
        return fail(env, 1801, u32::MAX);
    }
    // A local server handle is not a printer device and cannot supply DEVMODE.
    fail(env, 6, u32::MAX)
}
pub(super) fn register(registry: &mut Registry) {
    for (name, count, handler) in [
        ("OpenPrinterA", 3, open as adv_machine::Handler),
        ("ClosePrinter", 1, close),
        ("EnumPrintersA", 7, enumerate),
        ("DocumentPropertiesA", 6, properties),
    ] {
        crate::stdcall(registry, "winspool.drv", name, count, handler);
    }
}
