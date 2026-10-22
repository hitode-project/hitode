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
// spec: SYS-PRINT-1
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
            "tx-open transaction={transaction:x} result={result} output-cleared={} output-preserved={} output-root={}",
            opened.is_null(),
            opened == 0x5555usize as Handle,
            opened == root
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
fn native_nonempty_transacted_open_control() {
    let root = (0x80000001u32 as i32 as isize) as Handle;
    let name = "Software\0".encode_utf16().collect::<Vec<_>>();
    let mut opened = 0x5555usize as Handle;
    let result = unsafe {
        RegOpenKeyTransactedW(
            root,
            name.as_ptr(),
            0,
            0x20019,
            &mut opened,
            0x55555555usize as Handle,
            std::ptr::null(),
        )
    };
    eprintln!(
        "tx-open-nonempty result={result} output-cleared={} output-preserved={}",
        opened.is_null(),
        opened == 0x5555usize as Handle
    );
    if result == 0 {
        unsafe { RegCloseKey(opened) };
    }
}
#[test]
// spec: SYS-STR-1
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
// spec: SYS-NET-6
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
                let expected = if kind == "null" {
                    (6, 0xcdcdcdcd)
                } else if !write {
                    (12018, 0xcdcdcdcd)
                } else if length == 0 {
                    (87, 0xcdcdcdcd)
                } else {
                    (6, 0)
                };
                assert_eq!((error, count), expected);
            }
        }
    }
}

#[link(name = "wininet")]
unsafe extern "system" {
    fn HttpAddRequestHeadersW(handle: Handle, headers: *const u16, length: u32, flags: u32) -> i32;
    fn HttpEndRequestW(handle: Handle, buffers: *mut c_void, flags: u32, context: usize) -> i32;
    fn HttpOpenRequestW(
        handle: Handle,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept: *const *const u16,
        flags: u32,
        context: usize,
    ) -> Handle;
    fn HttpQueryInfoW(
        handle: Handle,
        level: u32,
        output: *mut u8,
        length: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn HttpSendRequestExW(
        handle: Handle,
        input: *const c_void,
        output: *mut c_void,
        flags: u32,
        context: usize,
    ) -> i32;
    fn HttpSendRequestW(
        handle: Handle,
        headers: *const u16,
        length: u32,
        optional: *const u8,
        optional_length: u32,
    ) -> i32;
    fn InternetQueryDataAvailable(
        handle: Handle,
        count: *mut u32,
        flags: u32,
        context: usize,
    ) -> i32;
    fn InternetSetFilePointer(
        handle: Handle,
        distance: i32,
        high: *mut i32,
        method: u32,
        context: usize,
    ) -> u32;
    fn InternetErrorDlg(
        window: Handle,
        request: Handle,
        error: u32,
        flags: u32,
        data: *mut *mut c_void,
    ) -> u32;
}
fn native_front_case(name: &str, result: i32) {
    eprintln!(
        "internet-front name={name} result={result} error={}",
        unsafe { GetLastError() }
    );
    assert_eq!(result, 0);
}
#[test]
fn native_request_front_guard_controls() {
    let agent = [0u16];
    let session = Internet(unsafe {
        InternetOpenW(agent.as_ptr(), 1, std::ptr::null(), std::ptr::null(), 0)
    });
    assert!(!session.0.is_null());
    let headers = "X-Fixture: value\r\n\0".encode_utf16().collect::<Vec<_>>();
    let mut output = [0xcdu8; 64];
    let mut size = 64;
    let mut index = 0;
    unsafe {
        native_front_case(
            "add-null",
            HttpAddRequestHeadersW(session.0, std::ptr::null(), 0, 0),
        );
        native_front_case(
            "add-valid",
            HttpAddRequestHeadersW(session.0, headers.as_ptr(), u32::MAX, 0x20000000),
        );
        native_front_case(
            "end",
            HttpEndRequestW(session.0, std::ptr::null_mut(), 0, 0),
        );
        let request = HttpOpenRequestW(
            session.0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
        );
        let error = GetLastError();
        if !request.is_null() {
            InternetCloseHandle(request);
        }
        eprintln!(
            "internet-front name=open result={} error={error}",
            u32::from(!request.is_null())
        );
        assert!(request.is_null());
        native_front_case(
            "query",
            HttpQueryInfoW(session.0, 19, output.as_mut_ptr(), &mut size, &mut index),
        );
        native_front_case(
            "send-ex",
            HttpSendRequestExW(session.0, std::ptr::null(), std::ptr::null_mut(), 0, 0),
        );
        native_front_case(
            "send",
            HttpSendRequestW(session.0, std::ptr::null(), 0, std::ptr::null(), 0),
        );
        let mut count = 0xcdcdcdcd;
        native_front_case(
            "available",
            InternetQueryDataAvailable(session.0, &mut count, 0, 0),
        );
        assert_eq!(count, 0);
        count = 0xcdcdcdcd;
        let result = InternetQueryDataAvailable(std::ptr::null_mut(), &mut count, 0, 0);
        eprintln!(
            "internet-available-invalid result={result} error={} count={count:x}",
            GetLastError()
        );
        let mut high = 0;
        let pointer = InternetSetFilePointer(session.0, 1, &mut high, 0, 0);
        eprintln!(
            "internet-pointer result={pointer:x} error={} high={high}",
            GetLastError()
        );
        assert_eq!(pointer, u32::MAX);
        let result = InternetErrorDlg(std::ptr::null_mut(), session.0, 0, 8, std::ptr::null_mut());
        assert_eq!(result, 0);
        let result = InternetErrorDlg(std::ptr::null_mut(), session.0, 0, 0, std::ptr::null_mut());
        eprintln!("internet-error-dialog zero-flags result={result}");
        let value = 12345u32;
        for length in [0u32, 3, 4] {
            let result = InternetSetOptionW(session.0, 2, (&value as *const u32).cast(), length);
            eprintln!(
                "internet-option option=2 length={length} result={result} error={}",
                GetLastError()
            );
        }
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetLastError(error: u32);
    fn EnumSystemLocalesA(
        callback: Option<unsafe extern "system" fn(*const u8) -> i32>,
        flags: u32,
    ) -> i32;
    fn EnumSystemLocalesEx(
        callback: Option<unsafe extern "system" fn(*const u16, u32, isize) -> i32>,
        flags: u32,
        param: isize,
        reserved: *const c_void,
    ) -> i32;
    fn EnumCalendarInfoA(
        callback: Option<unsafe extern "system" fn(*const u8) -> i32>,
        locale: u32,
        calendar: u32,
        kind: u32,
    ) -> i32;
}
static CALLBACKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
unsafe extern "system" fn stop_ansi(_text: *const u8) -> i32 {
    CALLBACKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    0
}
unsafe extern "system" fn stop_locale(_text: *const u16, _flags: u32, _param: isize) -> i32 {
    CALLBACKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    0
}
#[test]
fn native_nls_callback_stop_controls() {
    for kind in [0, 1, 2] {
        CALLBACKS.store(0, std::sync::atomic::Ordering::SeqCst);
        unsafe { SetLastError(0x12345678) };
        let result = unsafe {
            match kind {
                0 => EnumSystemLocalesA(Some(stop_ansi), 1),
                1 => EnumSystemLocalesEx(Some(stop_locale), 0, 0x12345678, std::ptr::null()),
                _ => EnumCalendarInfoA(Some(stop_ansi), 0x411, 1, 5),
            }
        };
        eprintln!(
            "nls-stop kind={kind} result={result} error={} calls={}",
            unsafe { GetLastError() },
            CALLBACKS.load(std::sync::atomic::Ordering::SeqCst)
        );
        assert_eq!(CALLBACKS.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(result, 1);
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocaleInfoW(locale: u32, kind: u32, output: *mut u16, capacity: i32) -> i32;
    fn GetDateFormatEx(
        locale: *const u16,
        flags: u32,
        date: *const u16,
        picture: *const u16,
        output: *mut u16,
        capacity: i32,
        calendar: *const u16,
    ) -> i32;
}
#[test]
fn native_fixed_locale_table_controls() {
    // Explicit built-in LCIDs and NOUSEROVERRIDE query public locale tables,
    // never the runner's language preferences or customized user information.
    for locale in [0x409, 0x411, 0x804, 0x7f] {
        for kind in [
            1, 3, 4, 5, 7, 8, 0xb, 0xd, 0x14, 0x19, 0x1a, 0x1f, 0x20, 0x21, 0x23, 0x1001, 0x1002,
            0x1003, 0x1005, 0x1006, 0x100c,
        ] {
            let mut units = [0u16; 128];
            let count = unsafe {
                GetLocaleInfoW(
                    locale,
                    kind | 0x80000000,
                    units.as_mut_ptr(),
                    units.len() as i32,
                )
            };
            assert!(count > 0);
            eprintln!(
                "fixed-locale locale={locale:x} kind={kind:x} value={}",
                String::from_utf16_lossy(&units[..count as usize - 1])
            );
        }
    }
    let st = [2024u16, 2, 0, 29, 13, 4, 5, 0];
    let picture = "dddd MMMM MMM d\0".encode_utf16().collect::<Vec<_>>();
    for name in ["en-US", "ja-JP", "zh-CN", ""] {
        let name = name.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut output = [0u16; 128];
        let count = unsafe {
            GetDateFormatEx(
                name.as_ptr(),
                0,
                st.as_ptr(),
                picture.as_ptr(),
                output.as_mut_ptr(),
                128,
                std::ptr::null(),
            )
        };
        assert!(count > 0);
        eprintln!(
            "fixed-date value={}",
            String::from_utf16_lossy(&output[..count as usize - 1])
        );
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocaleNameToLCID(name: *const u16, flags: u32) -> u32;
    fn LCIDToLocaleName(locale: u32, output: *mut u16, capacity: i32, flags: u32) -> i32;
}
#[test]
fn native_neutral_locale_name_controls() {
    for text in ["en", "ja", "zh", "zh-Hans"] {
        let name = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        for flags in [0, 0x08000000] {
            let id = unsafe { LocaleNameToLCID(name.as_ptr(), flags) };
            assert_ne!(id, 0);
            let mut output = [0u16; 85];
            let count = unsafe { LCIDToLocaleName(id, output.as_mut_ptr(), 85, flags) };
            assert!(count > 0);
            eprintln!(
                "fixed-neutral name={text} flags={flags:x} id={id:x} result={}",
                String::from_utf16_lossy(&output[..count as usize - 1])
            );
        }
    }
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn LookupAccountSidA(
        system: *const u8,
        sid: *const u8,
        name: *mut u8,
        name_size: *mut u32,
        domain: *mut u8,
        domain_size: *mut u32,
        usage: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorOwner(
        descriptor: *const u8,
        owner: *mut *const u8,
        defaulted: *mut i32,
    ) -> i32;
}
#[test]
fn native_public_sid_and_descriptor_output_controls() {
    let sid = [1u8, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
    for capacity in [0, 32] {
        let mut name = [0u8; 32];
        let mut domain = [0u8; 32];
        let mut names = capacity;
        let mut domains = capacity;
        let mut usage = 0xcdcdcdcd;
        let result = unsafe {
            LookupAccountSidA(
                std::ptr::null(),
                sid.as_ptr(),
                name.as_mut_ptr(),
                &mut names,
                domain.as_mut_ptr(),
                &mut domains,
                &mut usage,
            )
        };
        eprintln!(
            "public-sid capacity={capacity} result={result} error={} names={names} domains={domains} usage={usage}",
            unsafe { GetLastError() }
        );
        assert_eq!(result, i32::from(capacity != 0));
    }
    let mut descriptor = [0u8; 32];
    descriptor[0] = 1;
    descriptor[2..4].copy_from_slice(&0x8001u16.to_le_bytes());
    descriptor[4..8].copy_from_slice(&20u32.to_le_bytes());
    descriptor[20..32].copy_from_slice(&sid);
    for revision in [1, 2] {
        descriptor[0] = revision;
        let mut owner = std::ptr::null();
        let mut defaulted = 0x5555;
        let result =
            unsafe { GetSecurityDescriptorOwner(descriptor.as_ptr(), &mut owner, &mut defaulted) };
        eprintln!(
            "public-sd revision={revision} result={result} error={} owner-valid={} defaulted={defaulted}",
            unsafe { GetLastError() },
            owner == unsafe { descriptor.as_ptr().add(20) }
        );
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
struct ArrayBound {
    count: u32,
    lower: i32,
}
#[repr(C)]
struct NativeArray {
    dimensions: u16,
    features: u16,
    element: u32,
    locks: u32,
    data: *mut u8,
    bounds: [ArrayBound; 2],
}
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SafeArrayCreate(kind: u16, dimensions: u32, bounds: *const ArrayBound) -> *mut NativeArray;
    fn SafeArrayDestroy(array: *mut NativeArray) -> i32;
    fn SafeArrayGetLBound(array: *const NativeArray, dimension: u32, value: *mut i32) -> i32;
    fn SafeArrayPtrOfIndex(
        array: *mut NativeArray,
        indices: *const i32,
        value: *mut *mut u8,
    ) -> i32;
    fn VarBstrFromBool(value: i16, locale: u32, flags: u32, output: *mut *mut u16) -> i32;
    fn VarBstrFromDate(value: f64, locale: u32, flags: u32, output: *mut *mut u16) -> i32;
    fn VarR8FromStr(text: *const u16, locale: u32, flags: u32, output: *mut f64) -> i32;
    fn VarCyFromStr(text: *const u16, locale: u32, flags: u32, output: *mut i64) -> i32;
    fn SysStringLen(text: *const u16) -> u32;
    fn SysFreeString(text: *mut u16);
}
#[test]
fn native_authored_safe_array_dimension_controls() {
    let bounds = [
        ArrayBound {
            count: 2,
            lower: -3,
        },
        ArrayBound { count: 3, lower: 7 },
    ];
    let array = unsafe { SafeArrayCreate(3, 2, bounds.as_ptr()) };
    assert!(!array.is_null());
    let value = unsafe { &*array };
    eprintln!(
        "array-created dims={} flags={:x} element={} first={},{} second={},{}",
        value.dimensions,
        value.features,
        value.element,
        value.bounds[0].count,
        value.bounds[0].lower,
        value.bounds[1].count,
        value.bounds[1].lower
    );
    for dimension in [1, 2] {
        let mut lower = 0;
        assert_eq!(
            unsafe { SafeArrayGetLBound(array, dimension, &mut lower) },
            0
        );
        eprintln!("array-bound dim={dimension} lower={lower}");
    }
    for index in [[-3, 7], [-2, 7], [-3, 8]] {
        let mut pointer = std::ptr::null_mut();
        let result = unsafe { SafeArrayPtrOfIndex(array, index.as_ptr(), &mut pointer) };
        assert_eq!(result, 0);
        eprintln!(
            "array-index index={index:?} offset={}",
            pointer as usize - value.data as usize
        );
    }
    assert_eq!(unsafe { SafeArrayDestroy(array) }, 0);
}
#[test]
fn native_authored_automation_scalar_controls() {
    for locale in [0x409, 0x411, 0x804] {
        for flags in [0, 0x10] {
            for value in [0, -1, 7] {
                let mut text = std::ptr::null_mut();
                let result = unsafe { VarBstrFromBool(value, locale, flags, &mut text) };
                assert_eq!(result, 0);
                let n = unsafe { SysStringLen(text) };
                let value_text = String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(text, n as usize)
                });
                unsafe { SysFreeString(text) };
                eprintln!(
                    "automation-bool locale={locale:x} flags={flags:x} value={value} text={value_text}"
                );
            }
        }
        for value in [0.0, 0.5, -1.25, 45292.0] {
            let mut text = std::ptr::null_mut();
            let result = unsafe { VarBstrFromDate(value, locale, 0, &mut text) };
            assert_eq!(result, 0);
            let n = unsafe { SysStringLen(text) };
            let value_text =
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n as usize) });
            unsafe { SysFreeString(text) };
            eprintln!("automation-date locale={locale:x} value={value} text={value_text}");
        }
    }
    for text in [
        "1,234.5", "(12.5)", "1e3", "&H10", "$1.5", "0.00005", "0.00015",
    ] {
        let units = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut number = 0.0;
        let mut currency = 0i64;
        let numeric = unsafe { VarR8FromStr(units.as_ptr(), 0x409, 0, &mut number) } as u32;
        let monetary = unsafe { VarCyFromStr(units.as_ptr(), 0x409, 0, &mut currency) } as u32;
        eprintln!(
            "automation-number text={text} r8-status={numeric:x} r8={number} cy-status={monetary:x} cy={currency}"
        );
    }
}
#[repr(C, align(8))]
struct NativeVariant([u8; 32]);
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn VariantCopy(destination: *mut NativeVariant, source: *const NativeVariant) -> i32;
    fn VariantClear(value: *mut NativeVariant) -> i32;
}
#[test]
fn native_null_array_variant_controls() {
    let mut source = NativeVariant([0; 32]);
    source.0[..2].copy_from_slice(&0x2003u16.to_le_bytes());
    let mut destination = NativeVariant([0; 32]);
    assert_eq!(unsafe { VariantCopy(&mut destination, &source) }, 0);
    assert_eq!(&destination.0[..2], &0x2003u16.to_le_bytes());
    assert_eq!(unsafe { VariantClear(&mut source) }, 0);
    assert_eq!(unsafe { VariantClear(&mut destination) }, 0);
}
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn VarBoolFromStr(text: *const u16, locale: u32, flags: u32, out: *mut i16) -> i32;
    fn VarBstrFromCy(value: i64, locale: u32, flags: u32, out: *mut *mut u16) -> i32;
    fn VarDateFromStr(text: *const u16, locale: u32, flags: u32, out: *mut f64) -> i32;
    fn VarI4FromStr(text: *const u16, locale: u32, flags: u32, out: *mut i32) -> i32;
    fn VarUI4FromStr(text: *const u16, locale: u32, flags: u32, out: *mut u32) -> i32;
}
#[test]
fn native_authored_automation_decimal_boundaries_and_date_parsing() {
    for value in [12500i64, i64::MIN, i64::MAX] {
        let mut output = std::ptr::null_mut();
        let status = unsafe { VarBstrFromCy(value, 0x409, 0, &mut output) } as u32;
        let n = unsafe { SysStringLen(output) };
        let text =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(output, n as usize) });
        eprintln!("automation-currency value={value} status={status:x} text={text}");
        unsafe { SysFreeString(output) };
    }
    for text in [
        "2.5",
        "1.5",
        "-1.5",
        "1.49999999999999999",
        "2147483647",
        "2147483648",
        "4294967295",
        "-1",
        "1,2",
        "1e-999",
        "true",
        "TrUe",
        "922337203685477.5807",
        "-922337203685477.5808",
    ] {
        let input = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        let (mut signed, mut unsigned, mut boolean, mut currency) = (
            0x55555555i32,
            0x55555555u32,
            0x5555i16,
            0x5555555555555555i64,
        );
        let a = unsafe { VarI4FromStr(input.as_ptr(), 0x409, 0, &mut signed) } as u32;
        let b = unsafe { VarUI4FromStr(input.as_ptr(), 0x409, 0, &mut unsigned) } as u32;
        let c = unsafe { VarBoolFromStr(input.as_ptr(), 0x409, 0, &mut boolean) } as u32;
        let d = unsafe { VarCyFromStr(input.as_ptr(), 0x409, 0, &mut currency) } as u32;
        eprintln!(
            "automation-boundary text={text} i4={a:x},{signed} ui4={b:x},{unsigned} bool={c:x},{boolean} cy={d:x},{currency}"
        );
    }
    for (locale, text) in [
        (0x409, "2/29/2024"),
        (0x411, "2024年2月29日"),
        (0x804, "2024/2/29"),
        (0x409, "12/29/1899 6:00 AM"),
        (0x409, "6:00 PM"),
        (0x409, "2/29/2023"),
        (0x411, "2024/2/29 午後6:00"),
    ] {
        let input = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut value = 555.0;
        let status = unsafe { VarDateFromStr(input.as_ptr(), locale, 0, &mut value) } as u32;
        eprintln!(
            "automation-parse-date locale={locale:x} text={text} status={status:x} value={value}"
        );
    }
}
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SysAllocStringLen(text: *const u16, length: u32) -> *mut u16;
    fn VariantChangeTypeEx(
        destination: *mut NativeVariant,
        source: *const NativeVariant,
        locale: u32,
        flags: u16,
        target: u16,
    ) -> i32;
    fn VariantCopyInd(destination: *mut NativeVariant, source: *const NativeVariant) -> i32;
}
fn native_scalar_variant(kind: u16, value: u64) -> NativeVariant {
    let mut value_bytes = NativeVariant([0; 32]);
    value_bytes.0[..2].copy_from_slice(&kind.to_le_bytes());
    value_bytes.0[8..16].copy_from_slice(&value.to_le_bytes());
    value_bytes
}
#[test]
fn native_authored_extended_variant_scalar_and_indirect_controls() {
    for (source, value, target, flags) in [
        (3, 42, 5, 0),
        (3, 42, 8, 0),
        (11, u64::from(u16::MAX), 8, 0),
        (11, u64::from(u16::MAX), 8, 2),
        (11, 7, 8, 0),
        (11, 7, 8, 2),
        (0, 0, 8, 0),
        (3, 42, 0, 0),
        (0, 0, 1, 0),
    ] {
        let source_value = native_scalar_variant(source, value);
        let mut destination = NativeVariant([0; 32]);
        let status =
            unsafe { VariantChangeTypeEx(&mut destination, &source_value, 0x409, flags, target) }
                as u32;
        let kind = u16::from_le_bytes(destination.0[..2].try_into().unwrap());
        let value = if status == 0 && kind == 8 {
            let pointer =
                usize::from_le_bytes(destination.0[8..8 + size_of::<usize>()].try_into().unwrap())
                    as *mut u16;
            let n = unsafe { SysStringLen(pointer) };
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, n as usize) })
        } else {
            format!(
                "{}",
                u64::from_le_bytes(destination.0[8..16].try_into().unwrap())
            )
        };
        eprintln!(
            "automation-variant source={source} target={target} flags={flags:x} status={status:x} kind={kind} value={value}"
        );
        assert_eq!(unsafe { VariantClear(&mut destination) }, 0);
    }
    for (text, target) in [
        ("2.5", 6),
        ("2/29/2024", 7),
        ("2147483648", 3),
        ("1e-999", 11),
    ] {
        let input = text.encode_utf16().collect::<Vec<_>>();
        let pointer = unsafe { SysAllocStringLen(input.as_ptr(), input.len() as u32) };
        assert!(!pointer.is_null());
        let mut source = native_scalar_variant(8, pointer as usize as u64);
        let mut destination = NativeVariant([0; 32]);
        let status =
            unsafe { VariantChangeTypeEx(&mut destination, &source, 0x409, 0, target) } as u32;
        let payload = u64::from_le_bytes(destination.0[8..16].try_into().unwrap());
        eprintln!(
            "automation-variant-string text={text} target={target} status={status:x} payload={payload}"
        );
        assert_eq!(unsafe { VariantClear(&mut destination) }, 0);
        assert_eq!(unsafe { VariantClear(&mut source) }, 0);
    }
    let value = 42i32;
    let source = native_scalar_variant(0x4003, &value as *const i32 as usize as u64);
    let mut destination = NativeVariant([0; 32]);
    assert_eq!(unsafe { VariantCopyInd(&mut destination, &source) }, 0);
    assert_eq!(
        u16::from_le_bytes(destination.0[..2].try_into().unwrap()),
        3
    );
    assert_eq!(
        i32::from_le_bytes(destination.0[8..12].try_into().unwrap()),
        42
    );
    assert_eq!(unsafe { VariantClear(&mut destination) }, 0);
}
