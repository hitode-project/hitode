use adv_cpu_core::backend::{FilePage, SharedReadView};
use adv_cpu_core::shared_memory::SharedAllocation;
use adv_cpu_core::{Memory, PAGE_SIZE, READ, WRITE};
use std::{os::fd::AsFd, sync::Arc};

fn read_only_storage() -> Arc<SharedAllocation> {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "adv-host-boundary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file.write_all(&[0x31; 2 * PAGE_SIZE]).unwrap();
    SharedAllocation::map_file(file.as_fd(), 0, 2 * PAGE_SIZE, false).unwrap()
}

// spec: NAT-BODY-17
#[test]
fn host_boundary_generation_mutations() {
    let mut memory = Memory::reserve(0, 8 * PAGE_SIZE).unwrap();
    let at = PAGE_SIZE as u32;
    macro_rules! changed {
        ($operation:expr) => {{
            let before = memory.map_generation();
            $operation;
            assert!(memory.map_generation() > before);
        }};
    }
    changed!(memory.map(at, PAGE_SIZE, READ | WRITE).unwrap());
    changed!(memory.protect(at, PAGE_SIZE, READ).unwrap());
    changed!(memory.watch_code(at, 1));
    changed!(memory.watch_code(at + 1, 1));
    changed!(memory.unmap(at, PAGE_SIZE).unwrap());
    changed!(memory.map_shared_audio(at, PAGE_SIZE).unwrap());
    changed!(memory.unmap(at, PAGE_SIZE).unwrap());
    changed!(
        memory
            .map_shared_file(at, 2 * PAGE_SIZE, read_only_storage(), READ, false)
            .unwrap()
    );
    changed!(memory.unmap(at, 2 * PAGE_SIZE).unwrap());
    let page = FilePage::new([0x22; PAGE_SIZE]);
    changed!(
        memory
            .map_file_pages(at, std::slice::from_ref(&page), READ | WRITE, false)
            .unwrap()
    );
    changed!(memory.prepare_native_access(at, 1, false).unwrap());
    assert_eq!(
        memory.writable_bytes(at, 1),
        None,
        "resident storage is never lent mutably"
    );
    changed!(memory.unmap(at, PAGE_SIZE).unwrap());
    changed!(
        memory
            .map_file_pages(at, &[page], READ | WRITE, true)
            .unwrap()
    );
    changed!(memory.write(at, &[1]).unwrap());
    changed!(memory.unmap(at, PAGE_SIZE).unwrap());
    changed!(
        memory
            .map_private_copy_pages(at, &[0x44; PAGE_SIZE], READ | WRITE)
            .unwrap()
    );
    changed!(memory.privatize_copy_pages(at, PAGE_SIZE).unwrap());
}

// spec: NAT-BODY-17
#[test]
fn host_boundary_tlb_hits_follow_protect_unmap_and_watch() {
    let mut memory = Memory::reserve(0, 8 * PAGE_SIZE).unwrap();
    let at = PAGE_SIZE as u32;
    let len = 4 * PAGE_SIZE;
    memory.map(at, len, READ | WRITE).unwrap();
    assert!(memory.writable_bytes(at, len).is_some());
    let misses = memory.host_access().tlb_misses;
    assert!(memory.accessible(at + 1, len - 2, WRITE));
    assert_eq!(
        memory.host_access().tlb_misses,
        misses,
        "same pages, different byte edges"
    );
    memory.protect(at, len, READ).unwrap();
    assert!(!memory.accessible(at, len, WRITE));
    let misses = memory.host_access().tlb_misses;
    assert!(memory.writable_bytes(at, len).is_none());
    assert!(memory.accessible(at, len, READ));
    assert_eq!(memory.host_access().tlb_misses, misses);
    memory.protect(at, len, READ | WRITE).unwrap();
    assert!(memory.writable_bytes(at, len).is_some());
    memory.watch_code(at + PAGE_SIZE as u32, 1);
    assert!(memory.writable_bytes(at, len).is_none());
    memory.unmap(at, len).unwrap();
    assert_eq!(
        memory.committed(at, len).unwrap_err(),
        "unmapped guest memory at 0x1000"
    );
    let misses = memory.host_access().tlb_misses;
    assert!(!memory.accessible(at, len, READ));
    assert!(memory.read(at, len).is_err());
    assert_eq!(memory.host_access().tlb_misses, misses);
    memory.map(at, len, READ).unwrap();
    assert_eq!(memory.read(at, len).unwrap(), &[0; 4 * PAGE_SIZE]);
    assert!(memory.writable_bytes(at, 0).is_some());
    assert!(memory.read_only_file_view(at, 0).is_none());
}

// spec: A64-MEM-11, A64-MEM-13
#[test]
fn host_boundary_word_copies_all_alignments_and_lengths() {
    let storage = SharedAllocation::new(PAGE_SIZE).unwrap();
    let mut memory = Memory::reserve(0, 4 * PAGE_SIZE).unwrap();
    memory
        .map_shared_file(
            PAGE_SIZE as u32,
            PAGE_SIZE,
            storage.clone(),
            READ | WRITE,
            false,
        )
        .unwrap();
    for offset in 0..8 {
        for len in 0..=17 {
            let input: Vec<_> = (0..len).map(|i| (i + 1) as u8).collect();
            storage.store_bytes(0, &[0xa5; 32]);
            storage.store_bytes(offset, &input);
            let mut expected = [0xa5; 32];
            expected[offset..offset + len].copy_from_slice(&input);
            let mut actual = [0; 32];
            storage.copy_into(0, &mut actual);
            assert_eq!(actual, expected, "store {offset}/{len}");
            for destination in 0..8 {
                let mut actual = [0xa5; 32];
                storage.copy_into(offset, &mut actual[destination..destination + len]);
                assert_eq!(&actual[destination..destination + len], &input);
                assert!(actual[..destination].iter().all(|b| *b == 0xa5));
                assert!(actual[destination + len..].iter().all(|b| *b == 0xa5));
            }
            let at = PAGE_SIZE as u32 + offset as u32;
            memory.write(at, &input).unwrap();
            memory.read_into(at, &mut actual[..len]).unwrap();
            assert_eq!(&actual[..len], &input);
            for (i, &byte) in expected.iter().enumerate() {
                assert_eq!(storage.load_u8(i), byte);
            }
            let mut tail = [0xff; 17];
            storage.copy_into(PAGE_SIZE - len, &mut tail);
            assert_eq!(tail, [0; 17]);
        }
    }
}

// spec: NAT-BODY-17
#[test]
fn host_boundary_pinned_identity_revalidates() {
    let mut memory = Memory::reserve(0, 8 * PAGE_SIZE).unwrap();
    let at = PAGE_SIZE as u32;
    memory
        .map_shared_file(at, 2 * PAGE_SIZE, read_only_storage(), READ, false)
        .unwrap();
    let pinned = memory.pinned(at + 3, PAGE_SIZE + 7).unwrap();
    let misses = memory.host_access().tlb_misses;
    assert!(memory.still_pinned(&pinned));
    assert_eq!(memory.host_access().tlb_misses, misses);
    memory.map(4 * PAGE_SIZE as u32, PAGE_SIZE, READ).unwrap();
    assert!(memory.still_pinned(&pinned));
    memory.committed(4 * PAGE_SIZE as u32, PAGE_SIZE).unwrap();
    let misses = memory.host_access().tlb_misses;
    assert!(memory.still_pinned(&pinned));
    assert_eq!(
        memory.host_access().tlb_misses,
        misses,
        "revalidation updates the pin generation"
    );
    memory.protect(at, 2 * PAGE_SIZE, READ | WRITE).unwrap();
    assert!(!memory.still_pinned(&pinned));
    assert!(memory.pinned(at, 1).is_none());
    memory.protect(at, 2 * PAGE_SIZE, READ).unwrap();
    assert!(memory.still_pinned(&pinned));
    memory.unmap(at, 2 * PAGE_SIZE).unwrap();
    assert!(!memory.still_pinned(&pinned));
    memory
        .map_shared_file(at, 2 * PAGE_SIZE, read_only_storage(), READ, false)
        .unwrap();
    assert!(!memory.still_pinned(&pinned));
    let other = Memory::reserve(0, 8 * PAGE_SIZE).unwrap();
    assert!(!other.still_pinned(&pinned));
}

// spec: NAT-BODY-17
#[test]
fn host_boundary_counters_distinguish_contiguous_and_page_copies() {
    let mut memory = Memory::reserve(0, 8 * PAGE_SIZE).unwrap();
    // Counting is a diagnostic (EXEC-CALL-1).
    memory.count_host_access(true);
    memory
        .map(PAGE_SIZE as u32, 2 * PAGE_SIZE, READ | WRITE)
        .unwrap();
    let before = memory.host_access();
    memory.write(PAGE_SIZE as u32, &[1; 8]).unwrap();
    memory.read_into(PAGE_SIZE as u32, &mut [0; 8]).unwrap();
    memory
        .copy(PAGE_SIZE as u32, 2 * PAGE_SIZE as u32, 8)
        .unwrap();
    memory.fill(PAGE_SIZE as u32, &[0], 8).unwrap();
    let after = memory.host_access();
    assert_eq!(after.reads - before.reads, 2);
    assert_eq!(after.read_bytes - before.read_bytes, 16);
    assert_eq!(after.writes - before.writes, 3);
    assert_eq!(after.written_bytes - before.written_bytes, 24);
    assert_eq!(after.fallbacks, 0);
    memory
        .map_shared_file(
            3 * PAGE_SIZE as u32,
            2 * PAGE_SIZE,
            read_only_storage(),
            READ,
            false,
        )
        .unwrap();
    let before = memory.host_access();
    memory
        .read_into(3 * PAGE_SIZE as u32, &mut [0; PAGE_SIZE + 1])
        .unwrap();
    let after = memory.host_access();
    assert_eq!(after.shared_copies - before.shared_copies, 1);
    assert_eq!(after.fallbacks - before.fallbacks, 0);
    let before = memory.host_access();
    let mut bytes = [0; 2];
    memory
        .read_into(3 * PAGE_SIZE as u32 - 1, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [0, 0x31]);
    let after = memory.host_access();
    assert_eq!(after.shared_copies - before.shared_copies, 1);
    assert_eq!(after.fallbacks - before.fallbacks, 2);
    assert!(memory.write(3 * PAGE_SIZE as u32, &[1]).is_err());
    assert_eq!(memory.host_access().writes, after.writes);
}

// spec: NAT-BODY-17
#[test]
fn host_boundary_tlb_keeps_alternating_pages_and_replaces_collisions() {
    let mut memory = Memory::reserve(0, 12 * PAGE_SIZE).unwrap();
    memory
        .map(PAGE_SIZE as u32, 3 * PAGE_SIZE, READ | WRITE)
        .unwrap();
    let mut bytes = [0; 16];
    for page in 1..=3 {
        memory
            .read_into(page * PAGE_SIZE as u32, &mut bytes)
            .unwrap();
    }
    let misses = memory.host_access().tlb_misses;
    for _ in 0..10 {
        for page in 1..=3 {
            memory
                .read_into(page * PAGE_SIZE as u32, &mut bytes)
                .unwrap();
        }
    }
    assert_eq!(memory.host_access().tlb_misses, misses);
    let alias = 9 * PAGE_SIZE as u32;
    memory.map(alias, PAGE_SIZE, READ).unwrap();
    memory.write(alias, &[0x55; 16]).unwrap();
    for _ in 0..3 {
        memory.read_into(alias, &mut bytes).unwrap();
        assert_eq!(bytes, [0x55; 16]);
        memory.read_into(PAGE_SIZE as u32, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 16]);
    }
    memory.protect(alias, PAGE_SIZE, 0).unwrap();
    assert!(!memory.accessible(alias, 16, READ));
    memory.unmap(alias, PAGE_SIZE).unwrap();
    assert!(memory.read_into(alias, &mut bytes).is_err());
    let page = FilePage::new([0x66; PAGE_SIZE]);
    memory
        .map_file_pages(alias, &[page], READ | WRITE, false)
        .unwrap();
    memory.prepare_native_access(alias, 16, false).unwrap();
    assert!(memory.writable_bytes(alias, 16).is_none());
    memory.unmap(alias, PAGE_SIZE).unwrap();
    memory.map(alias, PAGE_SIZE, READ | WRITE).unwrap();
    assert!(memory.writable_bytes(alias, 16).is_some());
}

// spec: EXEC-CORE-6, EXEC-CORE-7, A64-MEM-11, A64-MEM-13
#[test]
fn coherent_file_aliases_track_only_observed_bytes() {
    use adv_cpu_core::EXEC;
    use adv_cpu_core::memory::LINEAR;
    let path = std::env::temp_dir().join(format!("adv-coherent-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file.set_len(0x8000).unwrap();
    for coherent in [false, true] {
        let mut memory = Memory::window(0, 0x40000).unwrap();
        let rw = SharedAllocation::map_file(file.as_fd(), 0, 0x8000, true).unwrap();
        let rx = SharedAllocation::map_file(file.as_fd(), 0x4000, 0x4000, false).unwrap();
        memory
            .map_shared_file(0x10000, 0x8000, rw, READ | WRITE, coherent)
            .unwrap();
        memory
            .map_shared_file(0x20000, 0x4000, rx.clone(), READ | EXEC, coherent)
            .unwrap();
        assert_ne!(memory.perms[0x14] & LINEAR, 0);
        memory.watch_code(0x20008, 4);
        assert_eq!(memory.code_tracked(0x20008), coherent);
        assert_eq!(memory.perms[0x14] & LINEAR == 0, coherent);
        let version = memory.versions[0x20];
        memory.dirty_pages.clear();
        assert!(!memory.stores_code(0x14018, 4));
        assert_eq!(memory.stores_code(0x14008, 4), coherent);
        memory.write(0x14018, b"data").unwrap();
        assert_eq!(memory.versions[0x20], version);
        assert!(memory.dirty_pages.is_empty());
        memory.write(0x14008, b"code").unwrap();
        assert!(memory.matches(0x20008, b"code"));
        assert_eq!(memory.versions[0x20] != version, coherent);
        assert_eq!(memory.dirty_pages.contains_key(&0x20000), coherent);
        // A new alias and a remapped watched address preserve the barrier.
        let alias = SharedAllocation::map_file(file.as_fd(), 0x4000, 0x4000, true).unwrap();
        memory
            .map_shared_file(0x30000, 0x4000, alias, READ | WRITE, coherent)
            .unwrap();
        assert_eq!(memory.perms[0x30] & LINEAR == 0, coherent);
        memory.unmap(0x20000, 0x4000).unwrap();
        memory.map(0x20000, 0x4000, READ | WRITE).unwrap();
        let version = memory.versions[0x20];
        memory.write(0x14008, b"away").unwrap();
        assert_eq!(memory.versions[0x20], version);
        memory.unmap(0x20000, 0x4000).unwrap();
        memory
            .map_shared_file(0x20000, 0x4000, rx, READ | EXEC, coherent)
            .unwrap();
        let version = memory.versions[0x20];
        memory.revoke_shared_file_coherence(
            adv_cpu_core::shared_memory::file_identity(file.as_fd()).unwrap(),
        );
        assert!(!memory.code_tracked(0x20008));
        assert_eq!(memory.versions[0x20] > version, coherent);
        let version = memory.versions[0x20];
        use std::os::unix::fs::FileExt;
        file.write_all_at(b"else", 0x4008).unwrap();
        assert_eq!(memory.versions[0x20], version);
        assert!(!memory.matches(0x20008, b"away"));
    }
}
