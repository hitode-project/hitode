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
}

/// Static TLS of one image (`__declspec(thread)` data).
#[derive(Clone, Debug)]
pub struct TlsImage {
    pub index: u32,
    pub template: Vec<u8>,
    pub zero_fill: u32,
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

    pub fn containing(&self, addr: u32) -> Option<&Image> {
        self.list.iter().find(|i| i.contains(addr))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Image> {
        self.list.iter()
    }
}
