#![cfg(windows)]
//! Read-only native controls use authored buffers, absent devices and existing root handles.
use std::ffi::c_void;
type Handle = *mut c_void;
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}
#[link(name = "winspool")]
unsafe extern "system" {
    fn OpenPrinterA(name: *const u8, handle: *mut Handle, defaults: *const c_void) -> i32;
    fn ClosePrinter(handle: Handle) -> i32;
    fn DocumentPropertiesA(
        window: Handle,
        printer: Handle,
        name: *const u8,
        output: *mut u8,
        input: *const u8,
        mode: u32,
    ) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyTransactedW(
        key: Handle,
        name: *const u16,
        options: u32,
        access: u32,
        output: *mut Handle,
        transaction: Handle,
        extended: *const c_void,
    ) -> i32;
    fn RegCreateKeyTransactedW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        access: u32,
        security: *const c_void,
        output: *mut Handle,
        disposition: *mut u32,
        transaction: Handle,
        extended: *const c_void,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
}
#[repr(C)]
struct CountedString {
    length: u16,
    maximum: u16,
    buffer: *mut u8,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlAnsiStringToUnicodeString(
        destination: *mut CountedString,
        source: *const CountedString,
        allocate: u8,
    ) -> i32;
}
#[link(name = "wininet")]
unsafe extern "system" {
    fn InternetOpenW(
        agent: *const u16,
        access: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> Handle;
    fn InternetCloseHandle(handle: Handle) -> i32;
    fn InternetSetOptionW(handle: Handle, option: u32, input: *const c_void, length: u32) -> i32;
    fn InternetQueryOptionA(handle: Handle, option: u32, output: *mut u8, length: *mut u32) -> i32;
    fn InternetReadFile(handle: Handle, output: *mut u8, length: u32, count: *mut u32) -> i32;
    fn InternetWriteFile(handle: Handle, input: *const u8, length: u32, count: *mut u32) -> i32;
}
#[test]
// spec: PERS-PRINT-1
fn absent_printer_failure_clears_handle_and_preserves_device_output() {
    let name = b"HitodeAuthoredAbsentPrinter-5E85A81D-6B28-42D2-9B8A-7E8DE280F437\0";
    let mut handle = 0x5555usize as Handle;
    let result = unsafe { OpenPrinterA(name.as_ptr(), &mut handle, std::ptr::null()) };
    let error = unsafe { GetLastError() };
    if result != 0 {
        unsafe { ClosePrinter(handle) };
    }
    assert_eq!(result, 0, "unexpected authored printer; error={error}");
    assert!(
        handle.is_null(),
        "failed OpenPrinter must clear its handle output"
    );
    let mut bytes = [0xcd; 64];
    let result = unsafe {
        DocumentPropertiesA(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_ptr(),
            bytes.as_mut_ptr(),
            std::ptr::null(),
            0,
        )
    };
    assert!(result < 0, "a nonexistent device has no DEVMODE size");
    assert!(
        bytes == [0xcd; 64],
        "failed size query modified device output"
    );
}
#[test]
fn native_transaction_handle_controls() {
    // Empty subkeys reopen the existing root; neither call creates or edits keys.
    let root = (0x80000001u32 as i32 as isize) as Handle;
    let name = [0u16];
    for transaction in [0usize, usize::MAX, 0x55555555] {
        let mut opened = 0x5555usize as Handle;
        let result = unsafe {
            RegOpenKeyTransactedW(
                root,
                name.as_ptr(),
                0,
                0x20019,
                &mut opened,
                transaction as Handle,
                std::ptr::null(),
            )
        };
        eprintln!(
            "tx-open transaction={transaction:x} result={result} output-cleared={} output-preserved={}",
            opened.is_null(),
            opened == 0x5555usize as Handle
        );
        if result == 0 {
            unsafe { RegCloseKey(opened) };
        }
        let mut opened = 0x5555usize as Handle;
        let mut disposition = 0xcdcdcdcd;
        let result = unsafe {
            RegCreateKeyTransactedW(
                root,
                name.as_ptr(),
                0,
                std::ptr::null(),
                0,
                0x20019,
                std::ptr::null(),
                &mut opened,
                &mut disposition,
                transaction as Handle,
                std::ptr::null(),
            )
        };
        eprintln!(
            "tx-create transaction={transaction:x} result={result} output-cleared={} output-preserved={} disposition={disposition:x}",
            opened.is_null(),
            opened == 0x5555usize as Handle
        );
        if result == 0 {
            unsafe { RegCloseKey(opened) };
        }
    }
}
#[test]
// spec: PERS-STR-1
fn native_counted_ansi_destination_capacity_controls() {
    let mut input = [0u8; 64];
    input[..4].copy_from_slice(b"ab\0c");
    let source = CountedString {
        length: 4,
        maximum: 64,
        buffer: input.as_mut_ptr(),
    };
    for maximum in [0u16, 2, 4, 6, 8, 10] {
        let mut bytes = [0xcdu8; 80];
        let mut destination = CountedString {
            length: 0x5555,
            maximum,
            buffer: unsafe { bytes.as_mut_ptr().add(8) },
        };
        let result = unsafe { RtlAnsiStringToUnicodeString(&mut destination, &source, 0) } as u32;
        eprintln!(
            "rtl-ansi capacity={maximum} result={result:x} length={} maximum={} output={:?}",
            destination.length,
            destination.maximum,
            &bytes[8..20]
        );
        assert_eq!(result, if maximum >= 10 { 0 } else { 0x80000005 });
        assert_eq!(destination.length, if maximum >= 10 { 8 } else { 0x5555 });
        assert_eq!(destination.maximum, maximum);
        let mut expected = [0xcd; 12];
        if maximum >= 10 {
            expected[..10].copy_from_slice(&[97, 0, 98, 0, 0, 0, 99, 0, 0, 0]);
        }
        assert_eq!(&bytes[8..20], &expected);
    }
    let mut input = vec![b'a'; 32832];
    let source = CountedString {
        length: 32767,
        maximum: 32767,
        buffer: input.as_mut_ptr(),
    };
    let mut bytes = vec![0xcd; 65552];
    let mut destination = CountedString {
        length: 0x5555,
        maximum: 0,
        buffer: unsafe { bytes.as_mut_ptr().add(8) },
    };
    let result = unsafe { RtlAnsiStringToUnicodeString(&mut destination, &source, 0) } as u32;
    eprintln!(
        "rtl-ansi oversized result={result:x} length={}",
        destination.length
    );
    assert_eq!(result, 0xc00000f0);
}
struct Internet(Handle);
impl Drop for Internet {
    fn drop(&mut self) {
        unsafe { InternetCloseHandle(self.0) };
    }
}
#[test]
// spec: PERS-NET-6
fn native_session_option_and_wrong_request_type_controls() {
    let mut agent = [0u16; 64];
    agent[..7].copy_from_slice(&[115, 101, 99, 111, 110, 100, 0]);
    let session = Internet(unsafe {
        InternetOpenW(agent.as_ptr(), 1, std::ptr::null(), std::ptr::null(), 0)
    });
    assert!(!session.0.is_null());
    let timeout = 12345u32;
    assert_ne!(
        unsafe { InternetSetOptionW(session.0, 2, (&timeout as *const u32).cast(), 4) },
        0
    );
    assert_ne!(
        unsafe { InternetSetOptionW(session.0, 41, agent.as_ptr().cast(), 14) },
        0,
        "error={}",
        unsafe { GetLastError() }
    );
    let mut bytes = [0xcdu8; 64];
    let mut size = 64;
    assert_ne!(
        unsafe { InternetQueryOptionA(session.0, 41, bytes.as_mut_ptr(), &mut size) },
        0
    );
    assert_eq!(&bytes[..7], b"second\0");
    for (kind, handle) in [("session", session.0), ("null", std::ptr::null_mut())] {
        for length in [0, 8] {
            for write in [false, true] {
                let mut count = 0xcdcdcdcdu32;
                let result = unsafe {
                    if write {
                        InternetWriteFile(handle, bytes.as_ptr(), length, &mut count)
                    } else {
                        InternetReadFile(handle, bytes.as_mut_ptr(), length, &mut count)
                    }
                };
                let error = unsafe { GetLastError() };
                eprintln!(
                    "internet-transfer kind={kind} write={write} length={length} result={result} error={error} count={count:x}"
                );
                assert_eq!(result, 0);
            }
        }
    }
}
