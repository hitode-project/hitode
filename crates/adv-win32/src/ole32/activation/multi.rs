//! Multi-interface activation queries a single constructed guest COM identity.
use super::*;
fn requests(env: &mut Env<'_>, count: u32, results: u32) -> Result<Vec<(u32, u32)>, u32> {
    let bytes = count
        .checked_mul(size_of::<MULTI_QI>() as u32)
        .ok_or(INVALID)?;
    if count == 0 {
        return Err(INVALID);
    }
    check_range(env.cpu, results, bytes, Prot::RW).map_err(|_| POINTER)?;
    let mut requests = Vec::new();
    for index in 0..count {
        let address = results + index * size_of::<MULTI_QI>() as u32;
        let row = GuestPtr::<MULTI_QI>::new(address)
            .read(env.cpu)
            .map_err(|_| POINTER)?;
        guid(env, row.pIID)?;
        if row.pItf != 0 {
            return Err(INVALID);
        }
        requests.push((address, row.pIID));
    }
    for (address, _) in &requests {
        env.cpu.write_u32(*address + 4, 0).map_err(|_| POINTER)?;
        env.cpu
            .write_u32(*address + 8, 0x80004002)
            .map_err(|_| POINTER)?;
    }
    Ok(requests)
}
fn failed(env: &mut Env<'_>, requests: &[(u32, u32)], status: u32) -> Outcome {
    for (row, _) in requests {
        if env.cpu.write_u32(*row + 8, status).is_err() {
            return Outcome::Return(POINTER);
        }
    }
    Outcome::Return(status)
}
fn query_next(
    env: &mut Env<'_>,
    object: u32,
    requests: Vec<(u32, u32)>,
    index: usize,
    success: usize,
    scratch: u32,
) -> Outcome {
    if index == requests.len() {
        let status = if success == requests.len() {
            0
        } else if success == 0 {
            0x80004002
        } else {
            0x80012
        };
        let release = match calls::method(env, object, 2) {
            Ok(target) => target,
            Err(error) => {
                free(env, scratch);
                return Outcome::Return(error);
            }
        };
        return Outcome::call(release, [object], move |env, _| {
            free(env, scratch);
            Outcome::Return(status)
        });
    }
    let (row, iid) = requests[index];
    let target = match calls::method(env, object, 0) {
        Ok(target) => target,
        Err(error) => {
            free(env, scratch);
            return Outcome::Return(error);
        }
    };
    Outcome::call(target, [object, iid, row + 4], move |env, result| {
        let status = result.value();
        let good = status & 0x80000000 == 0;
        if env.cpu.write_u32(row + 8, status).is_err() {
            free(env, scratch);
            return Outcome::Return(POINTER);
        }
        query_next(
            env,
            object,
            requests,
            index + 1,
            success + usize::from(good),
            scratch,
        )
    })
}
#[win32(ole32,name="CoCreateInstanceEx",level=implement)]
pub(super) fn create_ex(
    env: &mut Env<'_>,
    class: GuestPtr<Iid>,
    outer: GuestPtr<()>,
    context: u32,
    server: GuestPtr<COSERVERINFO>,
    count: u32,
    results: GuestPtr<MULTI_QI>,
) -> Outcome {
    let requests = match requests(env, count, results.addr()) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let class = match class.read(env.cpu) {
        Ok(value) => value,
        Err(_) => return Outcome::Return(POINTER),
    };
    if !server.is_null() {
        let server = match server.read(env.cpu) {
            Ok(value) => value,
            Err(_) => return Outcome::Return(POINTER),
        };
        if server.dwReserved1 != 0 || server.dwReserved2 != 0 {
            return Outcome::Return(INVALID);
        }
        if server.pwszName != 0 {
            let name = match WideStr(GuestPtr::new(server.pwszName))
                .read(env.cpu, 32768)
                .ok()
                .and_then(|s| String::from_utf16(&s).ok())
            {
                Some(name) => name,
                None => return Outcome::Return(INVALID),
            };
            let local = name.is_empty()
                || name == "."
                || name.eq_ignore_ascii_case("localhost")
                || name.eq_ignore_ascii_case(adv_os::identity::COMPUTER_NAME);
            if !local {
                return Outcome::Return(0x800706ba);
            }
        }
        if server.pAuthInfo != 0 {
            adv_abi::warn_unavailable(
                env,
                "ole32.dll!CoCreateInstanceEx",
                "explicit COM activation authentication is unavailable",
            );
            return Outcome::Return(0x80004001);
        }
    }
    let Some(construct) = constructor(env, class, context) else {
        return failed(env, &requests, UNREGISTERED);
    };
    let Some(scratch) = allocate(env, 4) else {
        return Outcome::Return(MEMORY);
    };
    let outcome = construct(env, IUNKNOWN, outer.addr(), scratch);
    calls::complete(
        env,
        outcome,
        Box::new(move |env, result| {
            if result & 0x80000000 != 0 {
                free(env, scratch);
                return failed(env, &requests, result);
            }
            let object = env.cpu.read_u32(scratch).unwrap_or(0);
            if object == 0 {
                free(env, scratch);
                return Outcome::Return(0x8000ffff);
            }
            query_next(env, object, requests, 0, 0, scratch)
        }),
    )
}
