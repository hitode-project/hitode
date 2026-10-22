#![cfg(windows)]
//! Native views use a newly created private key; no host values, SID paths or timestamps are recorded.
use std::ffi::c_void;
type Handle = *mut c_void;
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        access: u32,
        security: *const c_void,
        output: *mut Handle,
        disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        size: u32,
    ) -> i32;
    fn RegDeleteKeyW(key: Handle, name: *const u16) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn ZwQueryKey(key: Handle, class: i32, buffer: *mut c_void, size: u32, needed: *mut u32)
    -> i32;
    fn ZwEnumerateKey(
        key: Handle,
        index: u32,
        class: i32,
        buffer: *mut c_void,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn ZwEnumerateValueKey(
        key: Handle,
        index: u32,
        class: i32,
        buffer: *mut c_void,
        size: u32,
        needed: *mut u32,
    ) -> i32;
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
struct Keys {
    parent: Handle,
    key: Handle,
    path: Vec<u16>,
}
impl Drop for Keys {
    fn drop(&mut self) {
        unsafe {
            if !self.key.is_null() {
                let _ = RegDeleteKeyW(self.key, wide("Child").as_ptr());
                RegCloseKey(self.key);
            }
            if !self.parent.is_null() {
                let _ = RegDeleteKeyW(self.parent, wide("Container").as_ptr());
                RegCloseKey(self.parent);
                let root = (0x80000001u32 as i32 as isize) as Handle;
                let _ = RegDeleteKeyW(root, self.path.as_ptr());
            }
        }
    }
}
fn create(parent: Handle, name: &str, class: &str) -> Handle {
    let name = wide(name);
    let mut class = wide(class);
    let mut key = std::ptr::null_mut();
    let mut disposition = 0;
    assert_eq!(
        unsafe {
            RegCreateKeyExW(
                parent,
                name.as_ptr(),
                0,
                class.as_mut_ptr(),
                0,
                0xf003f,
                std::ptr::null(),
                &mut key,
                &mut disposition,
            )
        },
        0
    );
    assert_eq!(
        disposition, 1,
        "native fixture must never reuse an existing key"
    );
    key
}
#[repr(C, align(8))]
struct Buffer([u8; 512]);
fn show(
    operation: &str,
    class: i32,
    size: u32,
    status: i32,
    needed: u32,
    buffer: &Buffer,
    time: bool,
) {
    let end = (size as usize).min(needed as usize).min(96);
    let start = if time { end.min(8) } else { 0 };
    let hex = buffer.0[start..end]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    eprintln!(
        "registry-view operation={operation} class={class} size={size} status={:x} needed={needed} fields={hex}",
        status as u32
    );
}
#[test]
fn native_authored_key_and_value_information_buffer_controls() {
    let root = (0x80000001u32 as i32 as isize) as Handle;
    let name = format!(
        "Software\\HitodeAuthored-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let parent = create(root, &name, "");
    let mut keys = Keys {
        parent,
        key: std::ptr::null_mut(),
        path: wide(&name),
    };
    keys.key = create(parent, "Container", "OwnClass");
    let child = create(keys.key, "Child", "");
    assert_eq!(unsafe { RegCloseKey(child) }, 0);
    let number = 17u32;
    assert_eq!(
        unsafe {
            RegSetValueExW(
                keys.key,
                wide("Odd").as_ptr(),
                0,
                4,
                (&number as *const u32).cast(),
                4,
            )
        },
        0
    );
    let value = wide("sample");
    assert_eq!(
        unsafe {
            RegSetValueExW(
                keys.key,
                wide("Text").as_ptr(),
                0,
                1,
                value.as_ptr().cast(),
                value.len() as u32 * 2,
            )
        },
        0
    );
    for class in [0, 1, 2, 4, 99] {
        for size in [0, 12, 16, 20, 24, 40, 44, 128] {
            let mut buffer = Buffer([0xcd; 512]);
            let mut needed = 0x55555555;
            let status = unsafe {
                ZwQueryKey(
                    keys.key,
                    class,
                    buffer.0.as_mut_ptr().cast(),
                    size,
                    &mut needed,
                )
            };
            show("query", class, size, status, needed, &buffer, true);
        }
    }
    for class in [0, 1, 2] {
        for size in [0, 16, 24, 44, 128] {
            let mut buffer = Buffer([0xcd; 512]);
            let mut needed = 0x55555555;
            let status = unsafe {
                ZwEnumerateKey(
                    keys.key,
                    0,
                    class,
                    buffer.0.as_mut_ptr().cast(),
                    size,
                    &mut needed,
                )
            };
            show("key", class, size, status, needed, &buffer, true);
        }
    }
    for class in 0..5 {
        for size in [0, 8, 12, 20, 24, 28, 32, 36, 128] {
            let mut buffer = Buffer([0xcd; 512]);
            let mut needed = 0x55555555;
            let status = unsafe {
                ZwEnumerateValueKey(
                    keys.key,
                    0,
                    class,
                    buffer.0.as_mut_ptr().cast(),
                    size,
                    &mut needed,
                )
            };
            show("value", class, size, status, needed, &buffer, false);
        }
    }
    let mut buffer = Buffer([0xcd; 512]);
    let mut needed = 0x55555555;
    let status = unsafe {
        ZwEnumerateKey(
            keys.key,
            1,
            0,
            buffer.0.as_mut_ptr().cast(),
            128,
            &mut needed,
        )
    };
    show("key-end", 0, 128, status, needed, &buffer, true);
    let mut needed = 0x55555555;
    let status = unsafe {
        ZwEnumerateValueKey(
            keys.key,
            2,
            0,
            buffer.0.as_mut_ptr().cast(),
            128,
            &mut needed,
        )
    };
    show("value-end", 0, 128, status, needed, &buffer, false);
}
