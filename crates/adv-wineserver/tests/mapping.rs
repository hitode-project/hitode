//! The server's file mappings stay data on the host.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::ffi::{c_int, c_void};
use std::io::Write;
use std::os::fd::AsRawFd;

unsafe extern "C" {
    fn hitode_mmap(
        addr: *mut c_void,
        size: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: libc::off_t,
    ) -> *mut c_void;
}

// spec: WINE-SRV-4
#[test]
fn executable_mapping_requests_get_readable_data() {
    let _ = adv_wineserver::PROTOCOL_VERSION;
    let path = std::env::temp_dir().join(format!("adv-server-map-{}", std::process::id()));
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&[0x5a; 4096]).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let map = unsafe {
        hitode_mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(map, libc::MAP_FAILED);
    assert_eq!(unsafe { *map.cast::<u8>() }, 0x5a);
    assert_eq!(protection(map as usize) & libc::PROT_EXEC, 0);
    unsafe { libc::munmap(map, 4096) };
}

#[cfg(target_vendor = "apple")]
fn protection(address: usize) -> c_int {
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_vm_region(
            task: u32,
            address: *mut u64,
            size: *mut u64,
            flavor: c_int,
            info: *mut c_int,
            count: *mut u32,
            object: *mut u32,
        ) -> c_int;
    }
    // vm_region_basic_info_64 starts with its current protection.
    let (mut info, mut count) = ([0; 9], 9);
    let (mut address, mut size, mut object) = (address as u64, 0, 0);
    let status = unsafe {
        mach_vm_region(
            mach_task_self_,
            &mut address,
            &mut size,
            9, // VM_REGION_BASIC_INFO_64
            info.as_mut_ptr(),
            &mut count,
            &mut object,
        )
    };
    assert_eq!(status, 0);
    info[0]
}

#[cfg(target_os = "android")]
fn protection(address: usize) -> c_int {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let line = maps
        .lines()
        .find(|line| {
            let (start, end) = line.split_once(' ').unwrap().0.split_once('-').unwrap();
            let range =
                usize::from_str_radix(start, 16).unwrap()..usize::from_str_radix(end, 16).unwrap();
            range.contains(&address)
        })
        .unwrap();
    let flags = line.split(' ').nth(1).unwrap().as_bytes();
    (if flags[0] == b'r' { libc::PROT_READ } else { 0 })
        | (if flags[2] == b'x' { libc::PROT_EXEC } else { 0 })
}
