//! What the machine needs to know about the guest process, independent of how images are
//! loaded (that is the OS layer's job): initializers to run before the entry point, static
//! TLS templates for every new context, and where images lie for diagnostics.

/// A guest function to call during process (or LoadLibrary) initialization.
#[derive(Clone, Debug)]
pub struct InitCall {
    pub target: u32,
    pub args: Vec<u32>,
    /// For logs: "tls callback of X" / "DllMain of X" / "entry of X".
    pub what: String,
    /// Optional process status if this initializer returns zero. The image/OS
    /// frontend chooses the policy; the machine never interprets API names.
    pub failure_exit: Option<u32>,
    /// Optional frontend observation immediately before entering guest code.
    pub begin: Option<(InitBegin, usize)>,
}

pub type InitBegin = fn(&mut crate::Env<'_>, usize);

/// Static TLS of one image (`__declspec(thread)` data).
#[derive(Clone, Debug)]
pub struct TlsImage {
    pub index: u32,
    pub template: Vec<u8>,
    pub zero_fill: u32,
    pub active: bool,
}

/// A mapped guest image, as backtraces and profiles name addresses.
#[derive(Clone, Debug)]
pub struct Image {
    pub name: String,
    pub base: u32,
    pub size: u32,
}

impl Image {
    pub fn contains(&self, addr: u32) -> bool {
        addr.wrapping_sub(self.base) < self.size
    }
}

/// The mapped guest images, in load order.
#[derive(Default)]
pub struct Images {
    list: Vec<Image>,
}

impl Images {
    pub fn add(&mut self, image: Image) {
        self.list.push(image);
    }

    pub fn remove(&mut self, base: u32) {
        self.list.retain(|image| image.base != base);
    }

    pub fn containing(&self, addr: u32) -> Option<&Image> {
        self.list.iter().find(|i| i.contains(addr))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Image> {
        self.list.iter()
    }
}

/// A waitable terminal result, independent of the machine/address space lifetime.
#[derive(Debug, Default)]
pub struct Completion(std::sync::atomic::AtomicU64);
impl Completion {
    pub fn exit_code(&self) -> Option<u32> {
        let value = self.0.load(std::sync::atomic::Ordering::Acquire);
        (value >> 32 == 1).then_some(value as u32)
    }
    /// A reported termination status may precede the last live thread's exit.
    /// Waits and supervisor liveness must continue to use `exit_code`.
    pub fn exit_status(&self) -> Option<u32> {
        let value = self.0.load(std::sync::atomic::Ordering::Acquire);
        (value >> 32 != 0).then_some(value as u32)
    }
    /// Publish the first termination status without signaling completion.
    pub fn begin_termination(&self, code: u32) -> bool {
        self.0
            .compare_exchange(
                0,
                (2u64 << 32) | u64::from(code),
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
    pub fn complete(&self, code: u32) {
        let mut previous = self.0.load(std::sync::atomic::Ordering::Acquire);
        loop {
            if previous >> 32 == 1 {
                return;
            }
            let code = if previous >> 32 == 2 {
                previous as u32
            } else {
                code
            };
            match self.0.compare_exchange(
                previous,
                (1u64 << 32) | u64::from(code),
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(value) => previous = value,
            }
        }
    }
}

#[cfg(test)]
mod completion_tests {
    use super::Completion;

    #[test]
    fn pending_termination_reports_the_first_status_without_signaling() {
        let completion = Completion::default();
        assert_eq!(completion.exit_code(), None);
        assert_eq!(completion.exit_status(), None);
        assert!(completion.begin_termination(37));
        assert_eq!(completion.exit_status(), Some(37));
        assert_eq!(completion.exit_code(), None);
        assert!(!completion.begin_termination(42));
        completion.complete(99);
        assert_eq!(completion.exit_code(), Some(37));
        assert_eq!(completion.exit_status(), Some(37));
        completion.complete(42);
        assert_eq!(completion.exit_code(), Some(37));

        let normal = Completion::default();
        normal.complete(78);
        assert_eq!(normal.exit_code(), Some(78));
        assert!(!normal.begin_termination(42));
        assert_eq!(normal.exit_status(), Some(78));
    }
}

#[derive(Debug)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub completion: Completion,
}
impl ProcessIdentity {
    pub fn new(pid: u32) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            pid,
            completion: Completion::default(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct ThreadIdentity {
    pub process: std::sync::Arc<ProcessIdentity>,
    pub context: crate::CtxId,
    pub tid: u32,
    pub completion: std::sync::Arc<Completion>,
}
