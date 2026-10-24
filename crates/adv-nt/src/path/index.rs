//! Directory lookup indexes; exact host lookups remain in the parent module.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Cache {
    #[cfg(target_vendor = "apple")]
    volumes: HashMap<u64, bool>,
    directories: VecDeque<Index>,
}

struct Index {
    identity: (u64, u64),
    modified: (i64, i64),
    names: HashMap<String, OsString>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(super) fn find(directory: &Path, name: &str) -> Option<PathBuf> {
    let metadata = directory.metadata().ok()?;
    CACHE.with_borrow_mut(|cache| {
        let units: Vec<_> = name.encode_utf16().collect();
        let alias = units.len() >= 8 && units[4] == 126 && super::short_name(&units).is_empty();
        if !alias && !cache.case_sensitive(directory, metadata.dev()) {
            return None;
        }
        let identity = (metadata.dev(), metadata.ino());
        let modified = (metadata.mtime(), metadata.mtime_nsec());
        let position = cache
            .directories
            .iter()
            .position(|entry| entry.identity == identity);
        let mut entry = position.and_then(|position| cache.directories.remove(position));
        if entry
            .as_ref()
            .is_none_or(|entry| entry.modified != modified)
        {
            let mut names = HashMap::new();
            for entry in std::fs::read_dir(directory).ok()?.flatten() {
                let name = entry.file_name();
                let text = name.to_string_lossy();
                let short = super::short_name(&text.encode_utf16().collect::<Vec<_>>());
                // First entry wins, including collisions between long names and aliases.
                names
                    .entry(text.to_lowercase())
                    .or_insert_with(|| name.clone());
                if !short.is_empty() {
                    names
                        .entry(String::from_utf16_lossy(&short).to_lowercase())
                        .or_insert(name);
                }
            }
            entry = Some(Index {
                identity,
                modified,
                names,
            });
        }
        let entry = entry?;
        let result = entry
            .names
            .get(&name.to_lowercase())
            .map(|name| directory.join(name));
        cache.directories.push_back(entry);
        if cache.directories.len() > 256 {
            cache.directories.pop_front();
        }
        result
    })
}

/// Drop the index of a file's containing directory for an NT mutation.
pub(crate) fn invalidate(file: &Path) {
    if let Some(metadata) = file.parent().and_then(|parent| parent.metadata().ok()) {
        CACHE.with_borrow_mut(|cache| {
            cache
                .directories
                .retain(|entry| entry.identity != (metadata.dev(), metadata.ino()));
        });
    }
}

impl Cache {
    fn case_sensitive(&mut self, directory: &Path, _device: u64) -> bool {
        #[cfg(target_vendor = "apple")]
        return *self
            .volumes
            .entry(_device)
            .or_insert_with(|| case_sensitive(directory));
        #[cfg(not(target_vendor = "apple"))]
        case_sensitive(directory)
    }
}

#[cfg(target_vendor = "apple")]
fn case_sensitive(directory: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: zeroed output buffers and a valid NUL-terminated path.
    unsafe {
        let mut fs: libc::statfs = std::mem::zeroed();
        if libc::statfs(path.as_ptr(), &mut fs) != 0 {
            return true;
        }
        let mut attr: libc::attrlist = std::mem::zeroed();
        attr.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attr.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES;
        #[repr(C)]
        struct Caps {
            size: u32,
            caps: libc::vol_capabilities_attr_t,
        }
        let mut caps: Caps = std::mem::zeroed();
        if libc::getattrlist(
            fs.f_mntonname.as_ptr(),
            (&mut attr as *mut libc::attrlist).cast(),
            (&mut caps as *mut Caps).cast(),
            size_of::<Caps>(),
            0,
        ) != 0
        {
            return true;
        }
        let flags = libc::VOL_CAP_FMT_CASE_SENSITIVE | libc::VOL_CAP_FMT_CASE_PRESERVING;
        caps.size as usize == size_of::<Caps>()
            && caps.caps.valid[libc::VOL_CAPABILITIES_FORMAT] & flags == flags
            && caps.caps.capabilities[libc::VOL_CAPABILITIES_FORMAT]
                & libc::VOL_CAP_FMT_CASE_SENSITIVE
                != 0
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn case_sensitive(directory: &Path) -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(directory)
    else {
        return true;
    };
    let mut flags: libc::c_int = 0;
    // SAFETY: a live directory descriptor and writable flags, as in Wine's ioctl check.
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) } != -1
        && flags & 0x4000_0000 != 0
    {
        return false;
    }
    // SAFETY: a live descriptor and a valid output buffer.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    !(unsafe { libc::fstatfs(file.as_raw_fd(), &mut fs) } == 0
        && fs.f_type == 0x6573_5546
        && directory.join(".ciopfs").metadata().is_ok())
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn case_sensitive(_: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(name: &str) -> String {
        String::from_utf16_lossy(&super::super::short_name(
            &name.encode_utf16().collect::<Vec<_>>(),
        ))
    }

    fn nt(root: &Path, name: &str) -> Vec<u16> {
        format!(
            r"\??\unix{}",
            root.join(name).to_string_lossy().replace('/', "\\")
        )
        .encode_utf16()
        .collect()
    }

    // spec: WINE-SYS-12
    #[test]
    fn indexed_lookups_track_directory_changes() {
        let root = std::env::temp_dir().join(format!("adv-nt-index-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..3000 {
            std::fs::write(root.join(format!("Entry file {index}.txt")), []).unwrap();
        }
        let name = "Long file name for lookup.txt";
        std::fs::write(root.join(name), []).unwrap();
        assert_eq!(
            super::super::find(&root, &alias(name)),
            Some(root.join(name))
        );
        assert_eq!(
            find(&root, &alias(name).to_lowercase()),
            Some(root.join(name))
        );
        assert_eq!(
            super::super::find(&root, &name.to_uppercase())
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_lowercase(),
            name.to_lowercase()
        );
        assert_eq!(
            super::super::lookup(&root, &nt(&root, &alias(name))),
            Ok(super::super::Lookup::Found(root.join(name)))
        );
        assert_eq!(
            super::super::lookup(&root, &nt(&root, "missing.txt")),
            Ok(super::super::Lookup::Missing(root.join("missing.txt")))
        );
        assert_eq!(find(&root, "MISS~ABC.TXT"), None);
        let added = "Created behind the cache.txt";
        std::fs::write(root.join(added), []).unwrap();
        assert_eq!(find(&root, &alias(added)), Some(root.join(added)));
        std::fs::remove_file(root.join(added)).unwrap();
        assert_eq!(find(&root, &alias(added)), None);
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        rename_through_nt(&root, name);
        for index in 0..257 {
            let directory = root.join(index.to_string());
            std::fs::create_dir(&directory).unwrap();
            assert_eq!(find(&directory, "MISS~ABC"), None);
            if index == 255 {
                assert_eq!(find(&root, "MISS~ABC"), None);
            }
        }
        let identity = (
            root.metadata().unwrap().dev(),
            root.metadata().unwrap().ino(),
        );
        CACHE.with_borrow(|cache| {
            assert_eq!(cache.directories.len(), 256);
            assert!(
                cache
                    .directories
                    .iter()
                    .any(|entry| entry.identity == identity)
            );
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    // spec: WINE-SYS-12
    #[test]
    fn case_insensitive_misses_do_not_build_an_index() {
        let root = std::env::temp_dir().join(format!("adv-nt-miss-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Case probe"), []).unwrap();
        let sensitive = !root.join("CASE PROBE").exists();
        assert_eq!(case_sensitive(&root), sensitive);
        if !sensitive {
            assert_eq!(super::super::find(&root, "missing.txt"), None);
            assert_eq!(super::super::find(&root, "LONG~ABC.invalid"), None);
            CACHE.with_borrow(|cache| assert!(cache.directories.is_empty()));
            assert_eq!(super::super::find(&root, "MISS~ABC.TXT"), None);
            CACHE.with_borrow(|cache| assert_eq!(cache.directories.len(), 1));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_vendor = "apple", target_os = "android"))]
    fn rename_through_nt(root: &Path, name: &str) {
        use crate::{file, server};
        use adv_cpu::{
            Cpu,
            backend::{Guest, GuestExt, Prot},
        };
        use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};
        struct Runtime(adv_wineserver::Server);
        struct Time;
        impl adv_wineserver::Clock for Time {
            fn monotonic(&self) -> u64 {
                0
            }
            fn unix(&self) -> u64 {
                0
            }
        }
        impl server::Progress for Runtime {
            fn run_server(&mut self, _: server::Time) {
                self.0.step(&Time);
            }
        }
        let server = adv_wineserver::Server::start(
            &adv_wineserver::Config {
                prefix: root.to_owned(),
                server_dir: root.join("server"),
                install: std::env::var_os("ADV_WINE_DIST").unwrap().into(),
                debug: 0,
            },
            &Time,
        )
        .unwrap();
        let socket = server.socket_dir().unwrap();
        let mut runtime = Runtime(server);
        let (connection, first) =
            server::Connection::connect(&socket, &mut runtime, server::Time::default()).unwrap();
        let mut machine = Machine::new(
            Cpu::new(PARK_ADDR, true).unwrap(),
            crate::dispatch::Platform,
            Clock::virtual_time(),
        )
        .unwrap();
        let main = machine.spawn("rename", PARK_ADDR, &[], 0x10000).unwrap();
        machine.cpu.map(0x10000, 0x1000, Prot::RW).unwrap();
        machine.ext.insert(file::Files {
            dosdevices: root.join("dosdevices"),
        });
        let link = server::Link::new(
            connection,
            Box::new(runtime),
            server::Time::default(),
            main,
            first.tid,
            &mut machine.sys,
        );
        machine.ext.insert(link);
        machine
            .visit_context(Some(main), |env| {
                let handle = file::open_path(env, nt(root, name), 0x10000, 7, 0).unwrap();
                let destination = "Renamed through NT.txt";
                let units = nt(root, destination);
                env.cpu.write_u32(0x10000, 0).unwrap();
                env.cpu.write_u32(0x10004, 0).unwrap();
                env.cpu
                    .write_u32(0x10008, (units.len() * 2) as u32)
                    .unwrap();
                env.cpu
                    .write(
                        0x1000c,
                        &units
                            .iter()
                            .flat_map(|unit| unit.to_le_bytes())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                assert!(matches!(
                    file::set(
                        env,
                        &[handle, 0x10800, 0x10000, (12 + units.len() * 2) as u32, 10]
                    ),
                    Outcome::Return(0)
                ));
                assert!(CACHE.with_borrow(|cache| cache.directories.is_empty()));
                assert_eq!(find(root, &alias(name)), None);
                assert_eq!(
                    find(root, &alias(destination)),
                    Some(root.join(destination))
                );
                server::close(env, handle).unwrap();
            })
            .unwrap();
    }
}
