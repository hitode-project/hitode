//! HGLOBAL-backed COM streams share storage and keep independent clone cursors.
use crate::kernel32::memory::global;
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, types::STATSTG, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Identity, Outcome};
use adv_os::{
    Heaps,
    com::{Com, Iid, QueryError},
    global::Globals,
};
const STREAM: Iid = [12, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const SEQUENTIAL: Iid = [
    0x30, 0x3a, 0x73, 0x0c, 0x1c, 0x2a, 0xce, 0x11, 0xad, 0xe5, 0, 0xaa, 0, 0x44, 0x77, 0x3d,
];
const INVALID: u32 = 0x80070057;
const POINTER: u32 = 0x80004003;
const MEMORY: u32 = 0x8007000e;
const SEEK_ERROR: u32 = 0x80030019;
const INVALID_FUNCTION: u32 = 0x80030001;
#[derive(Clone, Copy)]
struct Storage {
    handle: u32,
    length: u32,
    delete: bool,
}
#[derive(Clone, Copy)]
struct Stream {
    storage: Identity<Storage>,
    position: u64,
}
struct State {
    com: Com,
    table: u32,
}
fn alloc(env: &mut Env<'_>, size: u32) -> Option<u32> {
    env.ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, size, true)
}
fn free(env: &mut Env<'_>, ptr: u32) {
    env.ext
        .get_mut::<Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, ptr);
}
fn initialize(env: &mut Env<'_>) -> bool {
    if env.ext.try_get::<State>().is_some() {
        return true;
    }
    let Some(table) = alloc(env, 56) else {
        return false;
    };
    let result = (|| {
        for s in adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| s.module == "IStream")
        {
            let entry = env
                .sys
                .thunks
                .publish(env.cpu, "ole32.dll", &format!("IStream::{}", s.name))
                .ok()?;
            env.cpu
                .write_u32(table + u32::from(s.com_slot?) * 4, entry)
                .ok()?;
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State { com, table });
        Some(())
    })();
    if result.is_none() {
        free(env, table);
    }
    result.is_some()
}
fn stream(env: &Env<'_>, this: u32) -> Option<Stream> {
    env.ext
        .try_get::<State>()?
        .com
        .get::<Stream>(&env.sys.objects, this)
        .copied()
}
fn publish(env: &mut Env<'_>, value: Stream) -> Option<u32> {
    let pointer = alloc(env, 4)?;
    let state = env.ext.get::<State>();
    let (table, com) = (state.table, state.com);
    if env.cpu.write_u32(pointer, table).is_err() {
        free(env, pointer);
        return None;
    }
    let Some(id) = com.create(&mut env.sys.objects, pointer, STREAM, value) else {
        free(env, pointer);
        return None;
    };
    if !com.register_interface(&mut env.sys.objects, id, SEQUENTIAL, pointer) {
        com.release::<Stream>(&mut env.sys.objects, pointer);
        free(env, pointer);
        return None;
    }
    Some(pointer)
}
fn output(env: &mut Env<'_>, pointer: u32, value: u32) -> bool {
    pointer != 0 && env.cpu.write_u32(pointer, value).is_ok()
}
fn optional32(env: &mut Env<'_>, pointer: u32, value: u32) -> bool {
    pointer == 0 || output(env, pointer, value)
}
fn optional64(env: &mut Env<'_>, pointer: u32, value: u64) -> bool {
    pointer == 0 || env.cpu.write(pointer, &value.to_le_bytes()).is_ok()
}
fn retire_storage(env: &mut Env<'_>, storage: Identity<Storage>) {
    if let Some(Some(storage)) = env.sys.objects.release(storage)
        && storage.delete
    {
        global::release(env, storage.handle, false);
    }
}
#[win32(ole32,name="CreateStreamOnHGlobal",level=state)]
fn create(env: &mut Env<'_>, handle: Handle, delete: i32, out: GuestPtr<()>) -> Outcome {
    if !output(env, out.addr(), 0) {
        return Outcome::Return(POINTER);
    }
    if !initialize(env) {
        return Outcome::Return(MEMORY);
    }
    let new = handle.0 == 0;
    let handle = if new {
        global::allocate(env, 2, 0)
    } else {
        handle.0
    };
    if new && handle == 0 {
        return Outcome::Return(MEMORY);
    }
    let Some(block) = env.ext.get::<Globals>().get(&env.sys.objects, handle) else {
        return Outcome::Return(INVALID);
    };
    if block.clipboard {
        return Outcome::Return(INVALID);
    }
    let value = Storage {
        handle,
        length: block.size,
        delete: delete != 0,
    };
    let Some(storage) = env.sys.objects.insert_owned(value) else {
        if new {
            global::release(env, handle, false);
        }
        return Outcome::Return(MEMORY);
    };
    let Some(pointer) = publish(
        env,
        Stream {
            storage,
            position: 0,
        },
    ) else {
        // Constructor rollback never takes ownership of the caller's HGLOBAL.
        env.sys.objects.release(storage);
        if new {
            global::release(env, handle, false);
        }
        return Outcome::Return(MEMORY);
    };
    if !output(env, out.addr(), pointer) {
        env.sys.objects.owned_mut(storage).unwrap().delete = new;
        release(env, GuestPtr::new(pointer));
        return Outcome::Return(POINTER);
    }
    Outcome::Return(0)
}
#[win32(ole32,name="GetHGlobalFromStream",level=state)]
fn hglobal(env: &mut Env<'_>, this: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !output(env, out.addr(), 0) {
        return Outcome::Return(POINTER);
    }
    let Some(s) = stream(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Some(storage) = env.sys.objects.owned(s.storage) else {
        return Outcome::Return(INVALID);
    };
    let handle = storage.handle;
    Outcome::Return(if output(env, out.addr(), handle) {
        0
    } else {
        POINTER
    })
}
#[win32(ole32,name="IStream::QueryInterface",level=state)]
fn query(env: &mut Env<'_>, this: GuestPtr<()>, iid: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !output(env, out.addr(), 0) {
        return Outcome::Return(POINTER);
    }
    let mut id = [0; 16];
    if env.cpu.read(iid.addr(), &mut id).is_err() {
        return Outcome::Return(POINTER);
    }
    let Some(state) = env.ext.try_get::<State>() else {
        return Outcome::Return(INVALID);
    };
    match state
        .com
        .query::<Stream>(&mut env.sys.objects, this.addr(), id)
    {
        Ok(pointer) => {
            if output(env, out.addr(), pointer) {
                Outcome::Return(0)
            } else {
                release(env, GuestPtr::new(pointer));
                Outcome::Return(POINTER)
            }
        }
        Err(QueryError::NoInterface) => Outcome::Return(0x80004002),
        Err(QueryError::ReferenceLimit) => Outcome::Return(MEMORY),
        Err(QueryError::InvalidObject) => Outcome::Return(INVALID),
    }
}
#[win32(ole32,name="IStream::AddRef",level=state)]
fn addref(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let value = env
        .ext
        .try_get::<State>()
        .and_then(|s| s.com.add_ref::<Stream>(&mut env.sys.objects, this.addr()))
        .unwrap_or(0);
    Outcome::Return(value)
}
#[win32(ole32,name="IStream::Release",level=state)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    let Some(state) = env.ext.try_get::<State>() else {
        return Outcome::Return(0);
    };
    let Some(released) = state
        .com
        .release::<Stream>(&mut env.sys.objects, this.addr())
    else {
        return Outcome::Return(0);
    };
    if let Some(destroyed) = released.destroyed {
        for pointer in destroyed.interfaces {
            free(env, pointer);
        }
        retire_storage(env, destroyed.value.storage);
    }
    Outcome::Return(released.references)
}
fn read_value(env: &mut Env<'_>, this: u32, data: u32, count: u32, out: u32) -> u32 {
    if !optional32(env, out, 0)
        || count != 0 && check_range(env.cpu, data, count, Prot::WRITE).is_err()
    {
        return POINTER;
    }
    let Some(s) = stream(env, this) else {
        return INVALID;
    };
    let Some(storage) = env.sys.objects.owned(s.storage).copied() else {
        return INVALID;
    };
    let Some(block) = env
        .ext
        .get::<Globals>()
        .get(&env.sys.objects, storage.handle)
    else {
        return INVALID;
    };
    let n = u64::from(count).min(u64::from(storage.length).saturating_sub(s.position)) as u32;
    if n != 0
        && env
            .cpu
            .copy_memory(block.pointer + s.position as u32, data, n as usize)
            .is_err()
    {
        return POINTER;
    }
    env.ext
        .get::<State>()
        .com
        .get_mut::<Stream>(&mut env.sys.objects, this)
        .unwrap()
        .position += u64::from(n);
    if !optional32(env, out, n) {
        return POINTER;
    }
    // The native HGLOBAL stream returns S_OK even for short reads and EOF.
    0
}
#[win32(ole32,name="IStream::Read",level=state)]
fn read(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    data: GuestPtr<()>,
    count: u32,
    out: GuestPtr<u32>,
) -> Outcome {
    Outcome::Return(read_value(env, this.addr(), data.addr(), count, out.addr()))
}
fn resize_value(env: &mut Env<'_>, id: Identity<Storage>, size: u32) -> u32 {
    let Some(storage) = env.sys.objects.owned(id).copied() else {
        return INVALID;
    };
    if size > 0x10000000 {
        adv_abi::warn_unavailable(
            env,
            "ole32.dll!IStream::SetSize",
            "stream storage exceeds the 256 MiB bounded frontend limit",
        );
        return MEMORY;
    }
    let handle = global::resize(env, storage.handle, size, 0x42, false);
    if handle == 0 {
        return MEMORY;
    }
    let value = env.sys.objects.owned_mut(id).unwrap();
    value.handle = handle;
    value.length = size;
    0
}
fn write_value(env: &mut Env<'_>, this: u32, data: u32, count: u32, out: u32) -> u32 {
    if !optional32(env, out, 0)
        || count != 0 && check_range(env.cpu, data, count, Prot::READ).is_err()
    {
        return POINTER;
    }
    let Some(s) = stream(env, this) else {
        return INVALID;
    };
    let Some(end) = s
        .position
        .checked_add(u64::from(count))
        .and_then(|n| u32::try_from(n).ok())
    else {
        return MEMORY;
    };
    let old = env.sys.objects.owned(s.storage).unwrap().length;
    if end > 0x10000000 {
        return MEMORY;
    }
    // Snapshot input before resizing: the source may alias the same HGLOBAL.
    let mut bytes = vec![0; count as usize];
    if count > 0 && env.cpu.read(data, &mut bytes).is_err() {
        return POINTER;
    }
    if count == 0 {
        return 0;
    }
    if end > old {
        let hr = resize_value(env, s.storage, end);
        if hr != 0 {
            return hr;
        }
    }
    let storage = *env.sys.objects.owned(s.storage).unwrap();
    let block = env
        .ext
        .get::<Globals>()
        .get(&env.sys.objects, storage.handle)
        .unwrap();
    let pointer = block.pointer;
    if s.position > u64::from(old) {
        let zero = [0; 4096];
        let mut start = old;
        while u64::from(start) < s.position {
            let n = (s.position - u64::from(start)).min(4096) as u32;
            if env.cpu.write(pointer + start, &zero[..n as usize]).is_err() {
                return POINTER;
            }
            start += n;
        }
    }
    if env.cpu.write(pointer + s.position as u32, &bytes).is_err() {
        return POINTER;
    }
    env.ext
        .get::<State>()
        .com
        .get_mut::<Stream>(&mut env.sys.objects, this)
        .unwrap()
        .position = u64::from(end);
    if !optional32(env, out, count) {
        return POINTER;
    }
    0
}
#[win32(ole32,name="IStream::Write",level=state)]
fn write(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    data: GuestPtr<()>,
    count: u32,
    out: GuestPtr<u32>,
) -> Outcome {
    Outcome::Return(write_value(
        env,
        this.addr(),
        data.addr(),
        count,
        out.addr(),
    ))
}
#[win32(ole32,name="IStream::Seek",level=state)]
fn seek(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    offset: i64,
    origin: u32,
    out: GuestPtr<u64>,
) -> Outcome {
    if out.addr() != 0 && check_range(env.cpu, out.addr(), 8, Prot::WRITE).is_err() {
        return Outcome::Return(POINTER);
    }
    let Some(s) = stream(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let base = match origin {
        0 => 0,
        1 => s.position,
        2 => u64::from(env.sys.objects.owned(s.storage).unwrap().length),
        _ => return Outcome::Return(INVALID_FUNCTION),
    };
    let Some(position) = base.checked_add_signed(offset) else {
        return Outcome::Return(SEEK_ERROR);
    };
    if !optional64(env, out.addr(), position) {
        return Outcome::Return(POINTER);
    }
    env.ext
        .get::<State>()
        .com
        .get_mut::<Stream>(&mut env.sys.objects, this.addr())
        .unwrap()
        .position = position;
    Outcome::Return(0)
}
#[win32(ole32,name="IStream::SetSize",level=state)]
fn set_size(env: &mut Env<'_>, this: GuestPtr<()>, size: u64) -> Outcome {
    let Some(s) = stream(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Ok(size) = u32::try_from(size) else {
        return Outcome::Return(MEMORY);
    };
    Outcome::Return(resize_value(env, s.storage, size))
}
struct Copy {
    this: u32,
    target: u32,
    remaining: u64,
    read: u64,
    written: u64,
    out_read: u32,
    out_written: u32,
    scratch: u32,
}
fn copy_done(env: &mut Env<'_>, copy: Copy, hr: u32) -> Outcome {
    let good = optional64(env, copy.out_read, copy.read)
        && optional64(env, copy.out_written, copy.written);
    free(env, copy.scratch);
    Outcome::Return(if good { hr } else { POINTER })
}
fn copy_next(env: &mut Env<'_>, mut copy: Copy) -> Outcome {
    if copy.remaining == 0 {
        return copy_done(env, copy, 0);
    }
    let n = copy.remaining.min(4096) as u32;
    let hr = read_value(env, copy.this, copy.scratch, n, copy.scratch + 4096);
    if hr != 0 {
        return copy_done(env, copy, hr);
    }
    let n = env.cpu.read_u32(copy.scratch + 4096).unwrap();
    if n == 0 {
        return copy_done(env, copy, 0);
    }
    copy.read += u64::from(n);
    copy.remaining -= u64::from(n);
    let method = env
        .cpu
        .read_u32(copy.target)
        .ok()
        .and_then(|p| p.checked_add(16))
        .and_then(|p| env.cpu.read_u32(p).ok());
    let Some(method) = method.filter(|&p| env.cpu.accessible(p, 1, Prot::EXEC)) else {
        return copy_done(env, copy, POINTER);
    };
    let _ = env.cpu.write_u32(copy.scratch + 4096, 0);
    Outcome::call(
        method,
        vec![copy.target, copy.scratch, n, copy.scratch + 4096],
        move |env, result| {
            let written = env.cpu.read_u32(copy.scratch + 4096).unwrap_or(0);
            if written > n {
                return copy_done(env, copy, INVALID);
            }
            copy.written += u64::from(written);
            if result.value() & 0x80000000 != 0 {
                return copy_done(env, copy, result.value());
            }
            if written < n {
                return copy_done(env, copy, 0x80030070);
            }
            copy_next(env, copy)
        },
    )
}
#[win32(ole32,name="IStream::CopyTo",level=state)]
fn copy_to(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    target: GuestPtr<()>,
    count: u64,
    out_read: GuestPtr<u64>,
    out_written: GuestPtr<u64>,
) -> Outcome {
    if !optional64(env, out_read.addr(), 0) || !optional64(env, out_written.addr(), 0) {
        return Outcome::Return(POINTER);
    }
    if stream(env, this.addr()).is_none() {
        return Outcome::Return(INVALID);
    }
    if target.is_null() {
        return Outcome::Return(POINTER);
    }
    let Some(scratch) = alloc(env, 4100) else {
        return Outcome::Return(MEMORY);
    };
    copy_next(
        env,
        Copy {
            this: this.addr(),
            target: target.addr(),
            remaining: count,
            read: 0,
            written: 0,
            out_read: out_read.addr(),
            out_written: out_written.addr(),
            scratch,
        },
    )
}
#[win32(ole32,name="IStream::Commit",level=state)]
fn commit(env: &mut Env<'_>, this: GuestPtr<()>, _flags: u32) -> Outcome {
    Outcome::Return(if stream(env, this.addr()).is_some() {
        0
    } else {
        INVALID
    })
}
#[win32(ole32,name="IStream::Revert",level=state)]
fn revert(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    commit(env, this, 0)
}
#[win32(ole32,name="IStream::LockRegion",level=safe_fail)]
fn lock(env: &mut Env<'_>, this: GuestPtr<()>, _offset: u64, _count: u64, _kind: u32) -> Outcome {
    Outcome::Return(if stream(env, this.addr()).is_some() {
        INVALID_FUNCTION
    } else {
        INVALID
    })
}
#[win32(ole32,name="IStream::UnlockRegion",level=safe_fail)]
fn unlock(env: &mut Env<'_>, this: GuestPtr<()>, offset: u64, count: u64, kind: u32) -> Outcome {
    lock(env, this, offset, count, kind)
}
#[win32(ole32,name="IStream::Stat",level=state)]
fn stat(env: &mut Env<'_>, this: GuestPtr<()>, out: GuestPtr<STATSTG>, flags: u32) -> Outcome {
    if flags > 2 {
        return Outcome::Return(INVALID);
    }
    let Some(s) = stream(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let storage = env.sys.objects.owned(s.storage).unwrap();
    let value = STATSTG {
        r#type: 2,
        cbSize: u64::from(storage.length),
        grfMode: 2,
        ..Default::default()
    };
    Outcome::Return(if out.write(env.cpu, value).is_ok() {
        0
    } else {
        POINTER
    })
}
#[win32(ole32,name="IStream::Clone",level=state)]
fn clone_stream(env: &mut Env<'_>, this: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !output(env, out.addr(), 0) {
        return Outcome::Return(POINTER);
    }
    let Some(s) = stream(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if !env.sys.objects.retain(s.storage) {
        return Outcome::Return(MEMORY);
    }
    let Some(pointer) = publish(env, s) else {
        retire_storage(env, s.storage);
        return Outcome::Return(MEMORY);
    };
    if !output(env, out.addr(), pointer) {
        release(env, GuestPtr::new(pointer));
        return Outcome::Return(POINTER);
    }
    Outcome::Return(0)
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        create_ABI,
        hglobal_ABI,
        query_ABI,
        addref_ABI,
        release_ABI,
        read_ABI,
        write_ABI,
        seek_ABI,
        set_size_ABI,
        copy_to_ABI,
        commit_ABI,
        revert_ABI,
        lock_ABI,
        unlock_ABI,
        stat_ABI,
        clone_stream_ABI,
    ] {
        reg.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture, env};
    use adv_cpu::backend::Guest;
    use adv_machine::Resume;

    #[test]
    fn cloned_streams_share_storage_and_keep_independent_positions() {
        let mut f = Fixture::new("hglobal-stream");
        assert_eq!(
            f.call("ole32.dll", "CreateStreamOnHGlobal", &[0, 1, DATA]),
            0
        );
        let s = f.m.cpu.read_u32(DATA).unwrap();
        f.m.cpu.write(DATA + 64, b"hello").unwrap();
        assert_eq!(
            f.call("ole32.dll", "IStream::Write", &[s, DATA + 64, 5, DATA + 4]),
            0
        );
        assert_eq!(f.call("ole32.dll", "IStream::Clone", &[s, DATA + 8]), 0);
        let clone = f.m.cpu.read_u32(DATA + 8).unwrap();
        assert_ne!(clone, s);
        assert_eq!(
            f.call("ole32.dll", "IStream::Seek", &[clone, 8, 0, 0, DATA + 16]),
            0
        );
        f.m.cpu.write(DATA + 64, b"!").unwrap();
        assert_eq!(
            f.call("ole32.dll", "IStream::Write", &[clone, DATA + 64, 1, 0]),
            0
        );
        assert_eq!(
            f.call("ole32.dll", "IStream::Read", &[s, DATA + 80, 16, DATA + 4]),
            0
        );
        assert_eq!(f.m.cpu.read_u32(DATA + 4).unwrap(), 4);
        let mut bytes = [0; 4];
        f.m.cpu.read(DATA + 80, &mut bytes).unwrap();
        assert_eq!(bytes, [0, 0, 0, b'!']);
        assert_eq!(
            f.call("ole32.dll", "IStream::Read", &[s, DATA + 80, 1, DATA + 4]),
            0
        );
        assert_eq!(f.m.cpu.read_u32(DATA + 4).unwrap(), 0);
        assert_eq!(
            f.call("ole32.dll", "IStream::Seek", &[s, u32::MAX, u32::MAX, 0, 0]),
            SEEK_ERROR
        );
        assert_eq!(f.call("ole32.dll", "IStream::Stat", &[s, DATA + 128, 1]), 0);
        let stat = GuestPtr::<STATSTG>::new(DATA + 128).read(&f.m.cpu).unwrap();
        assert_eq!((stat.r#type, stat.cbSize, stat.grfMode), (2, 9, 2));
        assert_eq!(
            f.call("ole32.dll", "GetHGlobalFromStream", &[s, DATA + 12]),
            0
        );
        let handle = f.m.cpu.read_u32(DATA + 12).unwrap();
        assert_eq!(f.call("ole32.dll", "IStream::Release", &[s]), 0);
        assert!(
            f.m.ext
                .get::<Globals>()
                .get(&f.m.sys.objects, handle)
                .is_some()
        );
        assert_eq!(f.call("ole32.dll", "IStream::Release", &[clone]), 0);
        assert!(
            f.m.ext
                .get::<Globals>()
                .get(&f.m.sys.objects, handle)
                .is_none()
        );
        f.cleanup();
    }

    #[test]
    fn copy_to_uses_the_destination_interface_and_balances_storage_ownership() {
        let mut f = Fixture::new("stream-copy");
        for out in [DATA, DATA + 4] {
            assert_eq!(
                f.call("ole32.dll", "CreateStreamOnHGlobal", &[0, 0, out]),
                0
            );
        }
        let src = f.m.cpu.read_u32(DATA).unwrap();
        let dst = f.m.cpu.read_u32(DATA + 4).unwrap();
        f.m.cpu.write(DATA + 64, b"copied").unwrap();
        assert_eq!(
            f.call("ole32.dll", "IStream::Write", &[src, DATA + 64, 6, 0]),
            0
        );
        assert_eq!(f.call("ole32.dll", "IStream::Seek", &[src, 0, 0, 0, 0]), 0);
        let mut outcome = f.invoke(
            "ole32.dll",
            "IStream::CopyTo",
            &[src, dst, 32, 0, DATA + 16, DATA + 24],
        );
        loop {
            match outcome {
                Outcome::Call {
                    target, args, then, ..
                } => {
                    let table = f.m.cpu.read_u32(dst).unwrap();
                    assert_eq!(target, f.m.cpu.read_u32(table + 16).unwrap());
                    let status = f.call("ole32.dll", "IStream::Write", &args);
                    outcome = then(&mut env(&mut f.m), Resume::Returned(status));
                }
                Outcome::Return(status) => {
                    assert_eq!(status, 0);
                    break;
                }
                _ => panic!("unexpected stream continuation"),
            }
        }
        for out in [DATA + 16, DATA + 24] {
            let mut n = [0; 8];
            f.m.cpu.read(out, &mut n).unwrap();
            assert_eq!(u64::from_le_bytes(n), 6);
        }
        assert_eq!(f.call("ole32.dll", "IStream::Seek", &[dst, 0, 0, 0, 0]), 0);
        assert_eq!(
            f.call("ole32.dll", "IStream::Read", &[dst, DATA + 80, 6, 0]),
            0
        );
        let mut bytes = [0; 6];
        f.m.cpu.read(DATA + 80, &mut bytes).unwrap();
        assert_eq!(&bytes, b"copied");
        for s in [src, dst] {
            assert_eq!(f.call("ole32.dll", "IStream::Commit", &[s, 0]), 0);
            assert_eq!(f.call("ole32.dll", "IStream::Revert", &[s]), 0);
            assert_eq!(
                f.call("ole32.dll", "IStream::LockRegion", &[s, 0, 0, 1, 0, 0]),
                INVALID_FUNCTION
            );
            assert_eq!(
                f.call("ole32.dll", "IStream::UnlockRegion", &[s, 0, 0, 1, 0, 0]),
                INVALID_FUNCTION
            );
            assert_eq!(
                f.call("ole32.dll", "GetHGlobalFromStream", &[s, DATA + 12]),
                0
            );
            let handle = f.m.cpu.read_u32(DATA + 12).unwrap();
            assert_eq!(f.call("ole32.dll", "IStream::Release", &[s]), 0);
            assert!(
                f.m.ext
                    .get::<Globals>()
                    .get(&f.m.sys.objects, handle)
                    .is_some()
            );
            assert_eq!(f.call("kernel32.dll", "GlobalFree", &[handle]), 0);
        }
        f.cleanup();
    }
}
