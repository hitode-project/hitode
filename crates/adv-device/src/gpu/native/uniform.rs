//! Reusable buffer windows in the resident part of the upload ring.
//! A resource can keep a window across frames while transient spans wrap.
//! Recordings and submissions keep their own leases on older windows.

use super::{BufferRange, Error, Gpu, record::gpu_error};
use adv_gpu::{
    Buffer, Device,
    resource::{BufferDesc, BufferSlice, BufferUsage, Memory},
};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

pub(super) const PAGE_BYTES: u64 = 8 << 20;

struct Block {
    buffer: Buffer,
    offset: u64,
}

type Free = Mutex<[Vec<Block>; u32::BITS as usize + 1]>;

pub(super) struct Pool {
    free: Arc<Free>,
    page: std::cell::RefCell<Block>,
}

impl Pool {
    pub(super) fn new(buffer: Buffer, offset: u64) -> Self {
        Self {
            free: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
            page: std::cell::RefCell::new(Block { buffer, offset }),
        }
    }

    fn allocate(&self, device: &Device, size: u32) -> Result<Allocation, Error> {
        let limits = &device.caps().limits;
        if u64::from(size) > limits.max_buffer_size {
            return Err(Error::Invalid(
                "uniform window exceeds the buffer size limit".into(),
            ));
        }
        let align = limits
            .uniform_offset_alignment
            .max(limits.storage_offset_alignment)
            .max(16);
        let size_class = u64::from(size.max(align)).next_power_of_two();
        let class = size_class.trailing_zeros() as usize;
        let block = self.free.lock().unwrap()[class].pop();
        let block = match block {
            Some(block) => block,
            None => {
                let mut page = self.page.borrow_mut();
                let extent = size_class.min(limits.max_buffer_size);
                let mut offset = page.offset.next_multiple_of(u64::from(align));
                if offset + extent > page.buffer.desc().size {
                    page.buffer = device
                        .create_buffer(&BufferDesc {
                            size: PAGE_BYTES.max(extent),
                            usage: BufferUsage::UNIFORM
                                | BufferUsage::INDEX
                                | BufferUsage::STORAGE
                                | BufferUsage::COPY_SRC
                                | BufferUsage::COPY_DST,
                            memory: Memory::Upload,
                            label: "resident upload ring",
                        })
                        .map_err(gpu_error)?;
                    offset = 0;
                }
                page.offset = offset + extent;
                Block {
                    buffer: page.buffer.clone(),
                    offset,
                }
            }
        };
        Ok(Allocation {
            block: Some(block),
            class,
            free: self.free.clone(),
        })
    }
}

struct Allocation {
    block: Option<Block>,
    class: usize,
    free: Arc<Free>,
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.free.lock().unwrap()[self.class].push(self.block.take().unwrap());
    }
}

#[derive(Clone)]
pub struct UniformWindow {
    allocation: Arc<Allocation>,
    range: Range<u64>,
}

impl UniformWindow {
    pub fn slice(&self, range: Range<u64>) -> Self {
        Self {
            allocation: self.allocation.clone(),
            range: self.range.start + range.start..self.range.start + range.end,
        }
    }

    fn binding(&self) -> BufferSlice<'_> {
        self.allocation
            .block
            .as_ref()
            .unwrap()
            .buffer
            .slice(self.range.clone())
    }
}

/// CPU-initialized storage used only for read-only shader and index bindings.
#[derive(Clone)]
pub struct CapturedWindow(UniformWindow);

impl CapturedWindow {
    /// # Safety
    /// This window must only be bound for GPU reads.
    pub unsafe fn bytes(&self) -> &[u8] {
        let slice = self.0.binding();
        let mapping = slice.buffer.mapped().unwrap().cast::<u8>();
        // SAFETY: The lease keeps these initialized bytes immutable. Captured
        // windows are bound for GPU reads only.
        unsafe {
            std::slice::from_raw_parts(
                mapping.as_ptr().add(slice.offset as usize),
                slice.size as usize,
            )
        }
    }
}

/// A native buffer range or a range whose allocation belongs to the ring.
#[derive(Clone)]
pub enum BufferWindow {
    Native(BufferRange),
    Uniform(UniformWindow),
    Captured(CapturedWindow),
}

impl From<BufferRange> for BufferWindow {
    fn from(value: BufferRange) -> Self {
        Self::Native(value)
    }
}

impl BufferWindow {
    pub(super) fn binding(&self) -> BufferSlice<'_> {
        match self {
            Self::Native(range) => range.buffer.slice(range.range.clone()),
            Self::Uniform(window) => window.binding(),
            Self::Captured(window) => window.0.binding(),
        }
    }

    pub(super) fn retain(&self, windows: &mut Vec<UniformWindow>) {
        match self {
            Self::Uniform(window) => windows.push(window.clone()),
            Self::Captured(window) => windows.push(window.0.clone()),
            Self::Native(_) => {}
        }
    }
}

impl Gpu {
    pub fn capture_window<E>(
        &self,
        size: u32,
        read: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<CapturedWindow, E>
    where
        E: From<Error>,
    {
        let padded = u64::from(size).max(1).next_multiple_of(4);
        let padded = u32::try_from(padded)
            .map_err(|_| Error::Invalid("capture exceeds the upload window".into()))?;
        self.uniform_window(padded, |out| {
            let (bytes, padding) = out.split_at_mut(size as usize);
            read(bytes)?;
            padding.fill(0);
            Ok(())
        })
        .map(CapturedWindow)
    }

    /// Initialize a window before publishing its lease to recorders.
    pub fn uniform_window<E>(
        &self,
        size: u32,
        write: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<UniformWindow, E>
    where
        E: From<Error>,
    {
        let allocation = self.execution.uniforms.allocate(&self.device, size)?;
        let block = allocation.block.as_ref().unwrap();
        let end = block.offset + u64::from(size);
        let mapping = block.buffer.mapped().unwrap();
        // SAFETY: The block is exclusive until every recorded or submitted
        // command using it releases its lease.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                mapping.cast::<u8>().as_ptr().add(block.offset as usize),
                size as usize,
            )
        };
        write(bytes)?;
        #[cfg(feature = "graphics-counters")]
        self.count_upload(u64::from(size));
        Ok(UniformWindow {
            range: block.offset..end,
            allocation: Arc::new(allocation),
        })
    }
}
