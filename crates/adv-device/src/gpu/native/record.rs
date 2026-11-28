//! Guest operations as command chunks, lowered on the dedicated `"adv-gfx-worker"`
//! CSMT thread. Parameter and bounds validation stay synchronous on the execution
//! thread; pipeline demand, ring packing, vertex recipe expansion, sampler lookup,
//! and multi-scissor deduplication run on `"adv-gfx-worker"`.

use super::super::{self as gpu, DrawState, Error, Filter, ProgrammableDraw, Rect, Vertex};
use super::pipelines::{Pipelines, fixed_sampling_flags, program_sampling_flags, slot};
use super::resources::Storage;
use super::stream::{Binding, Bytes, Command, Draw, Producer};
use super::{Depth, Gpu, Image, ImageId, Stats, Target, backing};
use crate::frame::Region;
use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

const MAX_UPLOAD: usize = 16 << 20;
const MAX_READBACKS: usize = 8;
const CONSTANT_SIZE: usize = std::mem::size_of::<gpu::ConstantBlock>();
const MAX_QUEUED_CMDS: usize = 4096;
const MAX_QUEUED_BYTES: usize = 64 << 20;
const MAX_IN_FLIGHT_FRAMES: usize = 3;
const WORKER_BATCH: usize = 64;
const MAX_RECYCLED_BYTES: usize = 8 << 20;

pub(super) struct Recording {
    white: Image,
    samplers: rustc_hash::FxHashMap<gpu::Sampler, adv_gpu::Sampler>,
}
impl Recording {
    pub fn new(device: &adv_gpu::Device, stream: &mut Producer) -> Result<Self, Error> {
        let image = device
            .create_image(&backing::image_desc(
                (1, 1),
                adv_gpu::Format::Bgra8Unorm,
                adv_gpu::resource::Usage::SAMPLED | adv_gpu::resource::Usage::COPY_DST,
            ))
            .map_err(gpu_error)?;
        let white = Image {
            storage: Storage::Fixed {
                view: image.view().clone(),
                image,
                level: None,
            },
            alpha: gpu::AlphaMode::Stored,
        };
        let bytes = stream
            .bytes(4, 4, |bytes| bytes.fill(255))
            .map_err(gpu_error)?;
        stream.push(Command::Upload {
            source: super::stream::UploadSource::Ring(bytes),
            pitch: 4,
            image: white.clone(),
            mip: 0,
            rect: adv_gpu::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        });
        Ok(Self::with_white(white))
    }

    fn with_white(white: Image) -> Self {
        Self {
            white,
            samplers: Default::default(),
        }
    }
}

pub(super) enum VertexPayload {
    Owned(Vec<Vertex>),
    Shared(Arc<Vec<Vertex>>),
}
impl VertexPayload {
    fn as_slice(&self) -> &[Vertex] {
        match self {
            Self::Owned(v) => v,
            Self::Shared(v) => v,
        }
    }
}

pub(super) enum UploadPayload {
    Owned(Vec<u8>),
    Shared(Arc<Vec<u8>>),
}
impl UploadPayload {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Shared(v) => v,
        }
    }
}

#[derive(Clone)]
pub(super) enum Clips {
    None,
    One(Rect),
    Inline(arrayvec::ArrayVec<Rect, 8>),
    Heap(Vec<Rect>),
}
#[allow(clippy::large_enum_variant)]
enum Areas {
    Inline(arrayvec::ArrayVec<(adv_gpu::Rect, Option<adv_gpu::Rect>), 8>),
    Heap(Vec<(adv_gpu::Rect, Option<adv_gpu::Rect>)>),
}
impl Areas {
    fn as_slice(&self) -> &[(adv_gpu::Rect, Option<adv_gpu::Rect>)] {
        match self {
            Self::Inline(v) => v,
            Self::Heap(v) => v,
        }
    }
    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
    fn without_opaque(&self) -> Self {
        match self {
            Self::Inline(v) => Self::Inline(v.iter().map(|&(a, _)| (a, None)).collect()),
            Self::Heap(v) => Self::Heap(v.iter().map(|&(a, _)| (a, None)).collect()),
        }
    }
}
impl Clips {
    fn from_slice(clips: Option<&[Rect]>, fallback: Option<Rect>) -> Self {
        match clips {
            Some([]) => Self::None,
            Some([one]) => Self::One(*one),
            Some(many) if many.len() <= 8 => Self::Inline(many.iter().copied().collect()),
            Some(many) => Self::Heap(many.to_vec()),
            None => match fallback {
                Some(clip) => Self::One(clip),
                None => Self::None,
            },
        }
    }
    fn areas(&self, target: &Target, state: &DrawState, vertices: Option<&[Vertex]>) -> Areas {
        let make = |clip: Option<Rect>| {
            let area = draw_area(target, state, clip);
            (area.w > 0 && area.h > 0)
                .then(|| (area, vertices.and_then(|v| opaque_quad(v, state, area))))
        };
        match self {
            Self::None => {
                let mut out = arrayvec::ArrayVec::new();
                if let Some(entry) = make(None) {
                    out.push(entry);
                }
                Areas::Inline(out)
            }
            Self::One(clip) => {
                let mut out = arrayvec::ArrayVec::new();
                if let Some(entry) = make(Some(*clip)) {
                    out.push(entry);
                }
                Areas::Inline(out)
            }
            Self::Inline(clips) => {
                let mut out = arrayvec::ArrayVec::new();
                for &clip in clips {
                    if let Some(entry) = make(Some(clip)) {
                        out.push(entry);
                    }
                }
                Areas::Inline(out)
            }
            Self::Heap(clips) => {
                let mut out = Vec::with_capacity(clips.len());
                for &clip in clips {
                    if let Some(entry) = make(Some(clip)) {
                        out.push(entry);
                    }
                }
                Areas::Heap(out)
            }
        }
    }
}

pub(super) type PipelineJobs = Arc<Mutex<HashMap<[u8; 32], super::pipelines::PipelineJob>>>;
#[cfg(test)]
type PipelineInspector = Box<dyn FnOnce(&mut Pipelines, &adv_gpu::Device) + Send>;
type FacadeInitReply = Result<(PipelineJobs, gpu::compile::CompileCounters), Error>;

pub(super) enum WorkerItem {
    CreateFacade {
        id: u64,
        shared_white: Option<u64>,
        failure: Arc<Mutex<Option<Error>>>,
        reply: mpsc::SyncSender<FacadeInitReply>,
    },
    DropFacade(u64),
    Clear {
        facade: u64,
        target: Target,
        area: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    DrawFixed {
        facade: u64,
        target: Target,
        vertices: VertexPayload,
        image: Option<Image>,
        state: DrawState,
        inclusive: Option<gpu::line::Inclusive>,
        clips: Clips,
    },
    DrawProgrammable {
        facade: u64,
        target: Target,
        draw: Arc<ProgrammableDraw>,
        state: DrawState,
        clips: Clips,
        line_ranges: Option<Arc<[std::ops::Range<u32>]>>,
    },
    DrawModern {
        facade: u64,
        render: Box<super::modern::Render>,
    },
    Expand {
        facade: u64,
        plan: Box<super::expand::Expand>,
        per_chunk: u32,
    },
    Dispatch {
        facade: u64,
        work: super::modern::Compute,
    },
    Resolve {
        facade: u64,
        source: adv_gpu::ImageView,
        destination: adv_gpu::ImageView,
    },
    TransferImage {
        facade: u64,
        command: Box<Command>,
    },
    GenerateMips {
        facade: u64,
        view: adv_gpu::ImageView,
    },
    Timestamp {
        facade: u64,
        slot: super::modern::QuerySlot,
    },
    ReadQuery {
        facade: u64,
        slot: super::modern::QuerySlot,
        destination: adv_gpu::Buffer,
        offset: u64,
    },
    Blit {
        facade: u64,
        source: Image,
        rectangle: Rect,
        destination: Image,
        region: Rect,
        filter: Filter,
        clip: Option<Rect>,
    },
    Copy {
        facade: u64,
        source: Image,
        rectangle: Rect,
        destination: Image,
        region: Rect,
        clip: Option<Rect>,
    },
    UploadBytes {
        facade: u64,
        image: Image,
        rectangle: Rect,
        pitch: u32,
        bytes: UploadPayload,
    },
    UploadStaged {
        facade: u64,
        image: Image,
        rectangle: Rect,
        staged: super::Staged,
    },
    DiscardDepth(Depth),
    PresentSwap {
        image: Image,
        damage: Region,
        serial: u64,
        video: Arc<[crate::frame::VideoPlane]>,
        windows: Arc<[crate::frame::WindowLayer]>,
    },
    PresentSoftware {
        base: crate::frame::Base,
        video: Arc<[crate::frame::VideoPlane]>,
        windows: Arc<[crate::frame::WindowLayer]>,
        serial: u64,
    },
    Flush,
    Readback {
        facade: u64,
        image: Image,
        regions: Vec<Rect>,
        worker: bool,
        transfer: Transfer,
    },
    SnapshotCopy {
        facade: u64,
        source: Image,
        destination: Image,
        regions: Vec<Rect>,
    },
    Capture(crate::frame::CaptureReply),
    SetPipelineCacheDir {
        facade: u64,
        directory: Option<std::path::PathBuf>,
        reply: mpsc::SyncSender<PipelineJobs>,
    },
    FlushPipelineCache {
        facade: u64,
        reply: mpsc::SyncSender<std::io::Result<()>>,
    },
    Stats {
        facade: u64,
        reply: mpsc::SyncSender<Stats>,
    },
    Drain {
        reply: mpsc::SyncSender<()>,
    },
    #[cfg(test)]
    InspectPipelines {
        facade: u64,
        f: PipelineInspector,
        reply: mpsc::SyncSender<()>,
    },
}

struct WorkerItemEntry {
    item: WorkerItem,
    bytes: usize,
}

#[derive(Default)]
struct WorkerQueue {
    full: VecDeque<Vec<WorkerItemEntry>>,
    free_batches: Vec<Vec<WorkerItemEntry>>,
    free_bytes: Vec<Vec<u8>>,
    free_bytes_total: usize,
    free_verts: Vec<Vec<Vertex>>,
    free_verts_total: usize,
    idle: bool,
    closed: bool,
}

pub(super) struct WorkerShared {
    queue: Mutex<WorkerQueue>,
    work_cv: Condvar,
    space_cv: Condvar,
    queued_cmds: AtomicUsize,
    queued_bytes: AtomicUsize,
    in_flight_frames: AtomicUsize,
    owner_closed: AtomicBool,
    wakes: Mutex<Vec<(u64, Arc<adv_sync::WakeEvent>)>>,
}

impl WorkerShared {
    pub(super) fn new() -> Self {
        Self {
            queue: Mutex::new(WorkerQueue::default()),
            work_cv: Condvar::new(),
            space_cv: Condvar::new(),
            queued_cmds: AtomicUsize::new(0),
            queued_bytes: AtomicUsize::new(0),
            in_flight_frames: AtomicUsize::new(0),
            owner_closed: AtomicBool::new(false),
            wakes: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn retire_frame(&self) {
        self.in_flight_frames.fetch_sub(1, Ordering::AcqRel);
        self.space_cv.notify_all();
        self.signal_wake();
    }

    pub(super) fn close_owner(&self) {
        self.owner_closed.store(true, Ordering::Release);
        self.space_cv.notify_all();
        self.signal_wake();
    }

    fn signal_wake(&self) {
        let wakes = self.wakes.lock().unwrap();
        for (_, wake) in wakes.iter() {
            wake.signal();
        }
    }

    pub(super) fn push_capture(&self, tx: crate::frame::CaptureReply) {
        let mut q = self.queue.lock().unwrap();
        if q.closed {
            return;
        }
        self.queued_cmds.fetch_add(1, Ordering::AcqRel);
        let mut batch = q.free_batches.pop().unwrap_or_default();
        batch.push(WorkerItemEntry {
            item: WorkerItem::Capture(tx),
            bytes: 0,
        });
        q.full.push_back(batch);
        if q.idle {
            self.work_cv.notify_one();
        }
    }

    fn recycle_bytes(&self, mut buf: Vec<u8>) {
        let cap = buf.capacity();
        if cap == 0 || cap > MAX_RECYCLED_BYTES / 2 {
            return;
        }
        buf.clear();
        let mut q = self.queue.lock().unwrap();
        if q.free_bytes_total + cap <= MAX_RECYCLED_BYTES && q.free_bytes.len() < 16 {
            q.free_bytes_total += cap;
            q.free_bytes.push(buf);
        }
    }

    fn recycle_verts(&self, mut buf: Vec<Vertex>) {
        let cap = buf.capacity() * std::mem::size_of::<Vertex>();
        if cap == 0 || cap > MAX_RECYCLED_BYTES / 2 {
            return;
        }
        buf.clear();
        let mut q = self.queue.lock().unwrap();
        if q.free_verts_total + cap <= MAX_RECYCLED_BYTES && q.free_verts.len() < 16 {
            q.free_verts_total += cap;
            q.free_verts.push(buf);
        }
    }
}

pub(super) struct WorkerHandle {
    shared: Arc<WorkerShared>,
    batch: RefCell<Vec<WorkerItemEntry>>,
    next_facade: AtomicU64,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    pub(super) fn start(
        device: adv_gpu::Device,
        producer: Producer,
        shared: Arc<WorkerShared>,
        owner_waker: Option<super::stream::Waker>,
    ) -> Result<Self, Error> {
        let worker_shared = shared.clone();
        let join = std::thread::Builder::new()
            .name("adv-gfx-worker".into())
            .spawn(move || {
                worker_loop(device, producer, worker_shared, owner_waker);
            })
            .map_err(|error| Error::Unavailable(error.to_string()))?;
        Ok(Self {
            shared,
            batch: RefCell::new(Vec::with_capacity(WORKER_BATCH)),
            next_facade: AtomicU64::new(1),
            join: Some(join),
        })
    }

    pub(super) fn shared(&self) -> Arc<WorkerShared> {
        self.shared.clone()
    }

    pub(super) fn set_wake(&self, facade: u64, wake: Option<Arc<adv_sync::WakeEvent>>) {
        let mut wakes = self.shared.wakes.lock().unwrap();
        if let Some(wake) = wake {
            if let Some((_, slot)) = wakes.iter_mut().find(|(id, _)| *id == facade) {
                *slot = wake;
            } else {
                wakes.push((facade, wake));
            }
        } else {
            wakes.retain(|(id, _)| *id != facade);
        }
    }

    pub(super) fn create_facade(
        &self,
        failure: Arc<Mutex<Option<Error>>>,
        shared_white: Option<u64>,
    ) -> Result<(u64, PipelineJobs, gpu::compile::CompileCounters), Error> {
        let id = self.next_facade.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(
            WorkerItem::CreateFacade {
                id,
                shared_white,
                failure,
                reply,
            },
            0,
        );
        let (jobs, counters) = rx
            .recv()
            .map_err(|_| Error::Device("adv-gfx-worker stopped".into()))??;
        Ok((id, jobs, counters))
    }

    pub(super) fn drop_facade(&self, id: u64) {
        self.push_boundary(WorkerItem::DropFacade(id), 0);
    }

    pub(super) fn take_bytes_buf(&self, len: usize) -> Vec<u8> {
        let mut q = self.shared.queue.lock().unwrap();
        if let Some(pos) = q.free_bytes.iter().position(|b| b.capacity() >= len) {
            let buf = q.free_bytes.swap_remove(pos);
            q.free_bytes_total -= buf.capacity();
            return buf;
        }
        drop(q);
        Vec::with_capacity(len)
    }

    pub(super) fn take_verts_buf(&self, len: usize) -> Vec<Vertex> {
        let mut q = self.shared.queue.lock().unwrap();
        if let Some(pos) = q.free_verts.iter().position(|b| b.capacity() >= len) {
            let buf = q.free_verts.swap_remove(pos);
            q.free_verts_total -= buf.capacity() * std::mem::size_of::<Vertex>();
            return buf;
        }
        drop(q);
        Vec::with_capacity(len)
    }

    pub(super) fn admit_cmd(&self, bytes: usize, nonblocking: bool) -> Result<(), Error> {
        let within_budget = || {
            let cmds = self.shared.queued_cmds.load(Ordering::Acquire);
            let queued_bytes = self.shared.queued_bytes.load(Ordering::Acquire);
            cmds < MAX_QUEUED_CMDS
                && (queued_bytes == 0 || queued_bytes.saturating_add(bytes) <= MAX_QUEUED_BYTES)
        };
        loop {
            if within_budget() {
                return Ok(());
            }
            self.flush_batch();
            if within_budget() {
                return Ok(());
            }
            if nonblocking {
                if self.shared.queue.lock().unwrap().closed {
                    return Err(Error::Device("adv-gfx-worker stopped".into()));
                }
                return Err(Error::Busy);
            }
            let q = self.shared.queue.lock().unwrap();
            if q.closed {
                return Err(Error::Device("adv-gfx-worker stopped".into()));
            }
            if within_budget() {
                return Ok(());
            }
            drop(
                self.shared
                    .space_cv
                    .wait_timeout(q, std::time::Duration::from_millis(1))
                    .unwrap(),
            );
        }
    }

    pub(super) fn admit_present(&self, nonblocking: bool) -> Result<(), Error> {
        loop {
            if self.shared.owner_closed.load(Ordering::Acquire) {
                return Err(Error::Device("GPU owner stopped".into()));
            }
            if self.shared.in_flight_frames.load(Ordering::Acquire) < MAX_IN_FLIGHT_FRAMES {
                return Ok(());
            }
            self.flush_batch();
            if self.shared.owner_closed.load(Ordering::Acquire) {
                return Err(Error::Device("GPU owner stopped".into()));
            }
            if self.shared.in_flight_frames.load(Ordering::Acquire) < MAX_IN_FLIGHT_FRAMES {
                return Ok(());
            }
            if nonblocking {
                if self.shared.queue.lock().unwrap().closed {
                    return Err(Error::Device("adv-gfx-worker stopped".into()));
                }
                return Err(Error::Busy);
            }
            let q = self.shared.queue.lock().unwrap();
            if q.closed {
                return Err(Error::Device("adv-gfx-worker stopped".into()));
            }
            if self.shared.owner_closed.load(Ordering::Acquire) {
                return Err(Error::Device("GPU owner stopped".into()));
            }
            if self.shared.in_flight_frames.load(Ordering::Acquire) < MAX_IN_FLIGHT_FRAMES {
                return Ok(());
            }
            drop(
                self.shared
                    .space_cv
                    .wait_timeout(q, std::time::Duration::from_millis(1))
                    .unwrap(),
            );
        }
    }

    pub(super) fn push(&self, item: WorkerItem, bytes: usize) {
        self.shared.queued_cmds.fetch_add(1, Ordering::AcqRel);
        self.shared.queued_bytes.fetch_add(bytes, Ordering::AcqRel);
        let mut batch = self.batch.borrow_mut();
        batch.push(WorkerItemEntry { item, bytes });
        if batch.len() >= WORKER_BATCH {
            drop(batch);
            self.flush_batch();
        }
    }

    pub(super) fn push_present(&self, item: WorkerItem) {
        self.shared.in_flight_frames.fetch_add(1, Ordering::AcqRel);
        self.push_boundary(item, 0);
    }

    pub(super) fn push_boundary(&self, item: WorkerItem, bytes: usize) {
        self.shared.queued_cmds.fetch_add(1, Ordering::AcqRel);
        self.shared.queued_bytes.fetch_add(bytes, Ordering::AcqRel);
        self.batch
            .borrow_mut()
            .push(WorkerItemEntry { item, bytes });
        self.flush_batch();
    }

    pub(super) fn flush_batch(&self) {
        let mut batch = self.batch.borrow_mut();
        if batch.is_empty() {
            return;
        }
        let mut q = self.shared.queue.lock().unwrap();
        if q.closed {
            batch.clear();
            return;
        }
        let next = q
            .free_batches
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(WORKER_BATCH));
        let full = std::mem::replace(&mut *batch, next);
        q.full.push_back(full);
        if q.idle {
            self.shared.work_cv.notify_one();
        }
    }

    pub(super) fn set_pipeline_cache_directory(
        &self,
        facade: u64,
        directory: Option<std::path::PathBuf>,
    ) -> Arc<Mutex<HashMap<[u8; 32], super::pipelines::PipelineJob>>> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(
            WorkerItem::SetPipelineCacheDir {
                facade,
                directory,
                reply,
            },
            0,
        );
        rx.recv().unwrap_or_default()
    }

    pub(super) fn flush_pipeline_cache(&self, facade: u64) -> std::io::Result<()> {
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(WorkerItem::FlushPipelineCache { facade, reply }, 0);
        rx.recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("adv-gfx-worker stopped")))
    }

    pub(super) fn stats(&self, facade: u64) -> Stats {
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(WorkerItem::Stats { facade, reply }, 0);
        rx.recv().unwrap_or_default()
    }

    pub(super) fn drain(&self) {
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(WorkerItem::Drain { reply }, 0);
        let _ = rx.recv();
    }

    #[cfg(test)]
    pub(super) fn inspect_pipelines(
        &self,
        facade: u64,
        f: impl FnOnce(&mut Pipelines, &adv_gpu::Device) + Send + 'static,
    ) {
        let (reply, rx) = mpsc::sync_channel(1);
        self.push_boundary(
            WorkerItem::InspectPipelines {
                facade,
                f: Box::new(f),
                reply,
            },
            0,
        );
        rx.recv().expect("adv-gfx-worker stopped");
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.flush_batch();
        {
            let mut q = self.shared.queue.lock().unwrap();
            q.closed = true;
            self.shared.work_cv.notify_one();
            self.shared.space_cv.notify_all();
        }
        if let Some(join) = self.join.take() {
            join.join().expect("adv-gfx-worker panicked");
        }
    }
}

struct FacadeState {
    failure: Arc<Mutex<Option<Error>>>,
    pipelines: Pipelines,
    recording: Recording,
    stats: Stats,
}

struct WorkerExit(Arc<WorkerShared>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        if let Ok(mut q) = self.0.queue.lock() {
            q.closed = true;
        }
        self.0.space_cv.notify_all();
        self.0.signal_wake();
    }
}

struct WorkerState {
    device: adv_gpu::Device,
    stream: Producer,
    target: Option<(ImageId, Option<Depth>)>,
    /// Looked up several times per command, so it uses a fast hasher.
    facades: rustc_hash::FxHashMap<u64, FacadeState>,
    shared: Arc<WorkerShared>,
    owner_waker: Option<super::stream::Waker>,
    pending_cmds: usize,
    pending_bytes: usize,
}

fn worker_loop(
    device: adv_gpu::Device,
    stream: Producer,
    shared: Arc<WorkerShared>,
    owner_waker: Option<super::stream::Waker>,
) {
    let _exit = WorkerExit(shared.clone());
    let mut state = WorkerState {
        device,
        stream,
        target: None,
        facades: Default::default(),
        shared: shared.clone(),
        owner_waker,
        pending_cmds: 0,
        pending_bytes: 0,
    };
    loop {
        let batch = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if let Some(batch) = q.full.pop_front() {
                    q.idle = false;
                    break Some(batch);
                }
                if q.closed {
                    break None;
                }
                q.idle = true;
                q = shared.work_cv.wait(q).unwrap();
            }
        };
        let Some(mut batch) = batch else {
            break;
        };
        #[cfg(target_vendor = "apple")]
        objc2::rc::autoreleasepool(|_| state.process_batch(&mut batch));
        #[cfg(not(target_vendor = "apple"))]
        state.process_batch(&mut batch);

        batch.clear();
        {
            let mut q = shared.queue.lock().unwrap();
            if q.free_batches.len() < 16 {
                q.free_batches.push(batch);
            }
        }
        shared.space_cv.notify_all();
        shared.signal_wake();
    }
    // Drop facades (flushing pipeline disk archives) before Producer closes.
    state.facades.clear();
    drop(state.stream);
}

impl WorkerState {
    fn facade_failure(&self, facade: u64) -> Option<Error> {
        self.facades
            .get(&facade)
            .and_then(|f| f.failure.lock().unwrap().clone())
    }

    fn release_queued(&mut self) {
        let cmds = std::mem::take(&mut self.pending_cmds);
        let bytes = std::mem::take(&mut self.pending_bytes);
        if cmds > 0 {
            self.shared.queued_cmds.fetch_sub(cmds, Ordering::AcqRel);
        }
        if bytes > 0 {
            self.shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
        }
    }

    fn process_batch(&mut self, batch: &mut Vec<WorkerItemEntry>) {
        for entry in batch.drain(..) {
            self.pending_cmds += 1;
            self.pending_bytes += entry.bytes;
            if let Err((facade, error)) = self.execute(entry.item)
                && let Some(state) = self.facades.get(&facade)
            {
                let mut slot = state.failure.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(error);
                }
            }
        }
        self.release_queued();
    }

    fn execute(&mut self, item: WorkerItem) -> Result<(), (u64, Error)> {
        match item {
            WorkerItem::CreateFacade {
                id,
                shared_white,
                failure,
                reply,
            } => {
                let recording = match shared_white {
                    Some(source) => Ok(Recording::with_white(
                        self.facades[&source].recording.white.clone(),
                    )),
                    None => Recording::new(&self.device, &mut self.stream),
                };
                let result = recording.map(|recording| {
                    let pipelines = Pipelines::default();
                    let jobs = pipelines.jobs();
                    let counters = pipelines.counters();
                    self.facades.insert(
                        id,
                        FacadeState {
                            failure,
                            pipelines,
                            recording,
                            stats: Stats::default(),
                        },
                    );
                    (jobs, counters)
                });
                self.release_queued();
                let _ = reply.send(result);
            }
            WorkerItem::DropFacade(id) => {
                self.facades.remove(&id);
            }
            WorkerItem::Clear {
                facade,
                target,
                area,
                color,
                depth,
                stencil,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                self.select(facade, &target);
                self.stream.push(Command::Clear {
                    area: native_rect(area),
                    color,
                    depth,
                    stencil,
                });
            }
            WorkerItem::DrawFixed {
                facade,
                target,
                vertices,
                image,
                state,
                inclusive,
                clips,
            } => {
                if self.facade_failure(facade).is_some() {
                    if let VertexPayload::Owned(buf) = vertices {
                        self.shared.recycle_verts(buf);
                    }
                    return Ok(());
                }
                let result = self.execute_draw_fixed(
                    facade,
                    &target,
                    vertices.as_slice(),
                    image.as_ref(),
                    &state,
                    inclusive,
                    &clips,
                );
                if let VertexPayload::Owned(buf) = vertices {
                    self.shared.recycle_verts(buf);
                }
                result.map_err(|error| (facade, error))?;
            }
            WorkerItem::DrawProgrammable {
                facade,
                target,
                draw,
                state,
                clips,
                line_ranges,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                self.execute_draw_programmable(
                    facade,
                    &target,
                    &draw,
                    &state,
                    &clips,
                    line_ranges.as_deref().unwrap_or_default(),
                )
                .map_err(|error| (facade, error))?;
            }
            WorkerItem::DrawModern { facade, render } => {
                if self.facade_failure(facade).is_none() {
                    let state = self.facades.get_mut(&facade).unwrap();
                    let pipeline = state
                        .pipelines
                        .modern(&self.device, &render.draw)
                        .map_err(|error| (facade, error))?;
                    self.stream.push(Command::DrawModern(render, pipeline));
                    state.stats.draws += 1;
                }
            }
            WorkerItem::Expand {
                facade,
                plan,
                per_chunk,
            } => {
                if self.facade_failure(facade).is_none() {
                    let state = self.facades.get_mut(&facade).unwrap();
                    let pipelines = plan
                        .bins
                        .iter()
                        .map(|bin| state.pipelines.modern(&self.device, &bin.draw))
                        .collect::<Result<_, _>>()
                        .map_err(|error| (facade, error))?;
                    self.stream
                        .push(Command::Expand(Box::new(super::expand::Work {
                            plan: *plan,
                            per_chunk,
                            pipelines,
                        })));
                    state.stats.draws += 1;
                }
            }
            WorkerItem::Dispatch { facade, work } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(Command::Dispatch(Box::new(work)));
                }
            }
            WorkerItem::Resolve {
                facade,
                source,
                destination,
            } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(Command::Resolve {
                        source,
                        destination,
                    });
                }
            }
            WorkerItem::TransferImage { facade, command } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(*command);
                }
            }
            WorkerItem::GenerateMips { facade, view } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(Command::GenerateMips(view));
                }
            }
            WorkerItem::Timestamp { facade, slot } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(Command::Timestamp(slot));
                }
            }
            WorkerItem::ReadQuery {
                facade,
                slot,
                destination,
                offset,
            } => {
                if self.facade_failure(facade).is_none() {
                    self.stream.push(Command::ReadQuery {
                        slot,
                        destination,
                        offset,
                    });
                }
            }
            WorkerItem::Blit {
                facade,
                source,
                rectangle,
                destination,
                region,
                filter,
                clip,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                self.blit_validated(
                    facade,
                    &source,
                    rectangle,
                    &destination,
                    region,
                    filter,
                    clip,
                )
                .map_err(|error| (facade, error))?;
            }
            WorkerItem::Copy {
                facade,
                source,
                rectangle,
                destination,
                region,
                clip,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                self.copy_validated(facade, &source, rectangle, &destination, region, clip);
            }
            WorkerItem::UploadBytes {
                facade,
                image,
                rectangle,
                pitch,
                bytes,
            } => {
                if self.facade_failure(facade).is_some() {
                    if let UploadPayload::Owned(buf) = bytes {
                        self.shared.recycle_bytes(buf);
                    }
                    return Ok(());
                }
                let row = upload_row_bytes(&image, rectangle);
                let src = bytes.as_slice();
                let ring_bytes = self.stream.bytes(row * rectangle.h, 256, |out| {
                    if pitch == row && src.len() >= (row * rectangle.h) as usize {
                        out.copy_from_slice(&src[..(row * rectangle.h) as usize]);
                    } else {
                        for y in 0..rectangle.h {
                            let s = y as usize * pitch as usize;
                            let d = y as usize * row as usize;
                            out[d..d + row as usize].copy_from_slice(&src[s..s + row as usize]);
                        }
                    }
                });
                if let UploadPayload::Owned(buf) = bytes {
                    self.shared.recycle_bytes(buf);
                }
                let ring_bytes = ring_bytes.map_err(|error| (facade, gpu_error(error)))?;
                self.record_upload(
                    &image,
                    rectangle,
                    row,
                    super::stream::UploadSource::Ring(ring_bytes),
                );
            }
            WorkerItem::UploadStaged {
                facade,
                image,
                rectangle,
                staged,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                let pitch = staged.pitch();
                self.record_upload(
                    &image,
                    rectangle,
                    pitch,
                    super::stream::UploadSource::Staged(staged),
                );
            }
            WorkerItem::DiscardDepth(depth) => {
                self.stream.push(Command::DiscardDepth(depth));
            }
            WorkerItem::PresentSwap {
                image,
                damage,
                serial,
                video,
                windows,
            } => {
                // Frame retirement happens on "GPU owner" after presentation
                // so the unbounded stream cannot accumulate frames ahead of display.
                self.stream.push(Command::PresentSwap {
                    image,
                    damage,
                    serial,
                    video,
                    windows,
                });
            }
            WorkerItem::PresentSoftware {
                base,
                video,
                windows,
                serial,
            } => {
                self.stream.push(Command::Present {
                    base,
                    video,
                    windows,
                    serial,
                });
            }
            WorkerItem::Flush => {
                self.stream.push(Command::Submit);
            }
            WorkerItem::Readback {
                facade,
                image,
                regions,
                worker,
                mut transfer,
            } => {
                self.release_queued();
                if let Some(error) = self.facade_failure(facade) {
                    transfer.finish(Err(error));
                    return Ok(());
                }
                self.execute_readback(facade, &image, &regions, worker, transfer);
            }
            WorkerItem::SnapshotCopy {
                facade,
                source,
                destination,
                regions,
            } => {
                if self.facade_failure(facade).is_some() {
                    return Ok(());
                }
                for &area in &regions {
                    if readback_copyable(&source)
                        && color_format(&source) == color_format(&destination)
                    {
                        self.copy_validated(facade, &source, area, &destination, area, None);
                    } else {
                        self.blit_validated(
                            facade,
                            &source,
                            area,
                            &destination,
                            area,
                            Filter::Nearest,
                            None,
                        )
                        .map_err(|error| (facade, error))?;
                    }
                }
            }
            WorkerItem::Capture(tx) => {
                self.stream.flush_chunk();
                self.release_queued();
                if let Some(waker) = &self.owner_waker {
                    waker.enqueue_capture(tx);
                }
            }
            WorkerItem::SetPipelineCacheDir {
                facade,
                directory,
                reply,
            } => {
                let jobs = self
                    .facades
                    .get_mut(&facade)
                    .map(|f| f.pipelines.open_cache(&self.device, directory.as_deref()))
                    .unwrap_or_default();
                self.release_queued();
                let _ = reply.send(jobs);
            }
            WorkerItem::FlushPipelineCache { facade, reply } => {
                let result = if let Some(f) = self.facades.get_mut(&facade) {
                    f.pipelines.flush_cache()
                } else {
                    Ok(())
                };
                self.release_queued();
                let _ = reply.send(result);
            }
            WorkerItem::Stats { facade, reply } => {
                let stats = self.facades.get(&facade).map_or(Stats::default(), |f| {
                    let mut stats = f.stats;
                    let cache = f.pipelines.stats();
                    stats.pipeline_cache_hits += cache.pipeline_cache_hits;
                    stats.inexact_sample_mask_pipelines = cache.inexact_sample_mask_pipelines;
                    stats.per_sample_uav_pipelines = cache.per_sample_uav_pipelines;
                    stats.anticipated_pipeline_draws += cache.anticipated_pipeline_draws;
                    stats.compile_waits += cache.compile_waits;
                    stats.compile_wait_ns += cache.compile_wait_ns;
                    stats.prebuild_hits += cache.prebuild_hits;
                    stats.prebuild_misses += cache.prebuild_misses;
                    stats.persisted_pipelines = cache.persisted_pipelines;
                    stats.discarded_pipelines = cache.discarded_pipelines;
                    stats
                });
                self.release_queued();
                let _ = reply.send(stats);
            }
            WorkerItem::Drain { reply } => {
                self.stream.flush_chunk();
                self.release_queued();
                let _ = reply.send(());
            }
            #[cfg(test)]
            WorkerItem::InspectPipelines { facade, f, reply } => {
                if let Some(state) = self.facades.get_mut(&facade) {
                    f(&mut state.pipelines, &self.device);
                }
                self.release_queued();
                let _ = reply.send(());
            }
        }
        Ok(())
    }

    fn data_alignment(&self) -> u32 {
        self.device
            .caps()
            .limits
            .uniform_offset_alignment
            .max(self.device.caps().limits.storage_offset_alignment)
            .max(4)
    }

    fn select(&mut self, _facade: u64, target: &Target) {
        let same = self.target.as_ref().is_some_and(|(image, depth)| {
            *image == target.image.id()
                && super::resources::same_depth(depth.as_ref(), target.depth.as_ref())
        });
        if !same {
            self.stream.push(Command::Target {
                colors: smallvec::smallvec![target.image.clone()],
                depth: target.depth.clone(),
            });
            self.target = Some((target.image.id(), target.depth.clone()));
        }
    }

    fn sampler(&mut self, facade: u64, mut spec: gpu::Sampler) -> Result<adv_gpu::Sampler, Error> {
        if spec.u == gpu::Address::Border {
            spec.u = gpu::Address::Clamp;
        }
        if spec.v == gpu::Address::Border {
            spec.v = gpu::Address::Clamp;
        }
        let f = self.facades.get_mut(&facade).unwrap();
        if let Some(sampler) = f.recording.samplers.get(&spec) {
            return Ok(sampler.clone());
        }
        let filter = |f| {
            if f == Filter::Linear {
                adv_gpu::resource::Filter::Linear
            } else {
                adv_gpu::resource::Filter::Nearest
            }
        };
        let address = |a| match a {
            gpu::Address::Clamp | gpu::Address::Border => adv_gpu::resource::Address::Clamp,
            gpu::Address::Repeat => adv_gpu::resource::Address::Repeat,
            gpu::Address::Mirror => adv_gpu::resource::Address::Mirror,
        };
        let sampler = self
            .device
            .sampler(&adv_gpu::resource::SamplerDesc {
                min: filter(spec.min),
                mag: filter(spec.mag),
                mip: filter(spec.mip.unwrap_or(Filter::Nearest)),
                address: [
                    address(spec.u),
                    address(spec.v),
                    adv_gpu::resource::Address::Clamp,
                ],
                border: adv_gpu::resource::BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: (spec.first_level.min(255) * 256) as u16,
                lod_max: if spec.mip.is_some() {
                    u16::MAX
                } else {
                    (spec.first_level.min(255) * 256) as u16
                },
                anisotropy: 1,
                compare: None,
            })
            .map_err(gpu_error)?;
        f.recording.samplers.insert(spec, sampler.clone());
        Ok(sampler)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_draw_fixed(
        &mut self,
        facade: u64,
        target: &Target,
        vertices: &[Vertex],
        image: Option<&Image>,
        state: &DrawState,
        inclusive: Option<gpu::line::Inclusive>,
        clips: &Clips,
    ) -> Result<(), Error> {
        let areas = clips.areas(target, state, Some(vertices));
        if areas.is_empty() {
            return Ok(());
        }
        self.select(facade, target);
        let colors = [color_format(&target.image)];
        let depth = target
            .depth
            .as_ref()
            .map(|d| backing::depth_format(d.format()));
        let f = self.facades.get_mut(&facade).unwrap();
        let pipeline = f.pipelines.fixed(
            &self.device,
            state,
            &colors,
            depth,
            fixed_sampling_flags(image),
        )?;
        let endpoint = if inclusive.is_some() {
            let mut point = *state;
            point.topology = gpu::Topology::Points;
            Some(f.pipelines.fixed(
                &self.device,
                &point,
                &colors,
                depth,
                fixed_sampling_flags(image),
            )?)
        } else {
            None
        };
        match inclusive {
            None => {
                self.fixed_draw_clips(facade, vertices, image, state, pipeline, areas.as_slice())
            }
            Some(gpu::line::Inclusive::Strip { terminal }) => {
                self.fixed_draw_clips(facade, vertices, image, state, pipeline, areas.as_slice())?;
                if terminal {
                    let mut point = *state;
                    point.topology = gpu::Topology::Points;
                    let point_areas = areas.without_opaque();
                    self.fixed_draw_clips(
                        facade,
                        &vertices[vertices.len() - 1..],
                        image,
                        &point,
                        endpoint.unwrap(),
                        point_areas.as_slice(),
                    )?;
                }
                Ok(())
            }
            Some(gpu::line::Inclusive::List) => {
                let point_areas = areas.without_opaque();
                for pair in vertices.as_chunks::<2>().0 {
                    self.fixed_draw_clips(
                        facade,
                        pair,
                        image,
                        state,
                        pipeline.clone(),
                        point_areas.as_slice(),
                    )?;
                    if !gpu::line::degenerate(&pair[0], &pair[1], state.viewport) {
                        let mut point = *state;
                        point.topology = gpu::Topology::Points;
                        self.fixed_draw_clips(
                            facade,
                            &pair[1..],
                            image,
                            &point,
                            endpoint.as_ref().unwrap().clone(),
                            point_areas.as_slice(),
                        )?;
                    }
                }
                Ok(())
            }
        }
    }

    fn fixed_draw_clips(
        &mut self,
        facade: u64,
        vertices: &[Vertex],
        image: Option<&Image>,
        state: &DrawState,
        pipeline: adv_gpu::RenderPipeline,
        areas: &[(adv_gpu::Rect, Option<adv_gpu::Rect>)],
    ) -> Result<(), Error> {
        let texture = image
            .unwrap_or(&self.facades[&facade].recording.white)
            .clone();
        let sampler = self.sampler(facade, state.sampler)?;
        let parameters = fixed_parameters(state, image);
        let alignment = self.data_alignment();
        let vertex_offset = 32usize.next_multiple_of(alignment as usize);
        let len = vertex_offset + std::mem::size_of_val(vertices);
        self.stream.ensure_room(areas.len());
        let data = self
            .stream
            .bytes(len as u32, alignment, |bytes| {
                bytes[..32].copy_from_slice(bytemuck::cast_slice(&parameters));
                bytes[vertex_offset..].copy_from_slice(bytemuck::cast_slice(vertices));
                #[cfg(target_vendor = "apple")]
                if matches!(
                    state.topology,
                    gpu::Topology::Lines | gpu::Topology::LineStrip
                ) {
                    for (bytes, vertex) in bytes[vertex_offset..]
                        .as_chunks_mut::<40>()
                        .0
                        .iter_mut()
                        .zip(vertices)
                    {
                        let y =
                            vertex.position[1] + vertex.position[3] / (64. * state.viewport.height);
                        bytes[4..8].copy_from_slice(&y.to_ne_bytes());
                    }
                }
            })
            .map_err(gpu_error)?;
        let base = summary(state, pipeline, areas[0].0, 0..vertices.len() as u32);
        self.stream.draw_clips(
            base,
            [
                (slot::FRAGMENT_UNIFORMS, Binding::Uniform(part(data, 0, 32))),
                (
                    slot::STREAMS,
                    Binding::Storage(part(
                        data,
                        vertex_offset as u32,
                        (len - vertex_offset) as u32,
                    )),
                ),
                (slot::TEXTURES, Binding::Sampled(texture)),
                (slot::SAMPLERS, Binding::Sampler(sampler)),
            ],
            areas,
        );
        let f = self.facades.get_mut(&facade).unwrap();
        f.stats.draws += areas.len() as u64;
        f.stats.vertices += vertices.len() as u64 * areas.len() as u64;
        Ok(())
    }

    fn execute_draw_programmable(
        &mut self,
        facade: u64,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
        clips: &Clips,
        line_ranges: &[std::ops::Range<u32>],
    ) -> Result<(), Error> {
        let areas = clips.areas(target, state, None);
        let areas = areas.as_slice();
        if areas.is_empty() {
            return Ok(());
        }
        let count = draw
            .indices
            .as_ref()
            .map_or(draw.vertex_count as usize, |i| i.len());
        self.select(facade, target);
        let sampling: arrayvec::ArrayVec<_, 20> =
            draw.textures.iter().map(program_sampling_flags).collect();
        let depth = target
            .depth
            .as_ref()
            .map(|d| backing::depth_format(d.format()));
        let colors = [color_format(&target.image)];
        let pipelines = &mut self.facades.get_mut(&facade).unwrap().pipelines;
        let pipeline =
            pipelines.programmable(&self.device, draw, state, &colors, depth, &sampling)?;
        let endpoint = if line_ranges.iter().any(|range| range.len() == 1) {
            let mut point = *state;
            point.topology = gpu::Topology::Points;
            Some(pipelines.programmable(&self.device, draw, &point, &colors, depth, &sampling)?)
        } else {
            None
        };
        let alignment = self.data_alignment() as usize;
        let parameters_offset = CONSTANT_SIZE;
        let pixel_offset = (CONSTANT_SIZE + 640).next_multiple_of(alignment);
        let mut cursor = (pixel_offset + CONSTANT_SIZE).next_multiple_of(alignment);
        let mut stream_ranges = arrayvec::ArrayVec::<(u32, u32), 16>::new();
        for stream in &draw.streams {
            stream_ranges.push((cursor as u32, stream.bytes.len().next_multiple_of(4) as u32));
            cursor = (cursor + stream.bytes.len().next_multiple_of(4)).next_multiple_of(alignment);
        }
        let index_offset = cursor;
        cursor += draw.indices.as_ref().map_or(0, |i| i.len() * 4);
        // Keep draw ranges and clips with their shared ring span.
        let draws = areas.len() * line_ranges.len().max(1);
        self.stream.ensure_room(draws);
        let data = self
            .stream
            .bytes(cursor as u32, alignment as u32, |bytes| {
                // Programs read only their used registers (GFX-REC-7), so the
                // rest of each window keeps whatever the ring held.
                for (stage, constants) in [&draw.vertex_constants, &draw.pixel_constants]
                    .into_iter()
                    .enumerate()
                {
                    let base = if stage == 0 { 0 } else { pixel_offset };
                    for register in draw.program.constants[stage]
                        .ranges()
                        .flat_map(|range| range.start / 16..range.end / 16)
                    {
                        bytes[base + register * 16..base + (register + 1) * 16]
                            .copy_from_slice(bytemuck::bytes_of(&constants.register(register)));
                    }
                }
                bytes[parameters_offset..parameters_offset + 640]
                    .copy_from_slice(bytemuck::bytes_of(&draw.parameters));
                for (stream, &(offset, len)) in draw.streams.iter().zip(&stream_ranges) {
                    let (offset, end) = (offset as usize, (offset + len) as usize);
                    let written = offset + stream.bytes.len();
                    stream.bytes.copy_to(&mut bytes[offset..written]);
                    // The last word of a stream is read whole.
                    bytes[written..end].fill(0);
                }
                if let Some(indices) = &draw.indices {
                    bytes[index_offset..].copy_from_slice(bytemuck::cast_slice(indices.as_slice()));
                }
            })
            .map_err(gpu_error)?;
        let mut bindings = arrayvec::ArrayVec::<(u32, Binding), 58>::new();
        bindings.push((
            slot::VERTEX_UNIFORMS,
            Binding::Uniform(part(data, 0, (CONSTANT_SIZE + 640) as u32)),
        ));
        bindings.push((
            slot::FRAGMENT_UNIFORMS,
            Binding::Uniform(part(data, pixel_offset as u32, CONSTANT_SIZE as u32)),
        ));
        for (i, &(offset, len)) in stream_ranges.iter().enumerate() {
            bindings.push((
                slot::STREAMS + i as u32,
                Binding::Storage(part(data, offset, len)),
            ));
        }
        let mut pixel = 0;
        let mut vertex = 16;
        for (texture, &vertex_stage) in draw.textures.iter().zip(&draw.program.textures) {
            let ordinal = if vertex_stage {
                let n = vertex;
                vertex += 1;
                n
            } else {
                let n = pixel;
                pixel += 1;
                n
            };
            let image = texture
                .image
                .as_ref()
                .unwrap_or(&self.facades[&facade].recording.white)
                .clone();
            let sampler = self.sampler(facade, texture.sampler)?;
            bindings.push((slot::TEXTURES + ordinal, Binding::Sampled(image)));
            bindings.push((slot::SAMPLERS + ordinal, Binding::Sampler(sampler)));
        }
        let mut command = summary(state, pipeline, areas[0].0, 0..count as u32);
        command.instances = 0..draw.instance_count;
        command.uses_depth |= draw.program.writes_depth;
        command.writes_depth |= draw.program.writes_depth;
        command.index = draw.indices.as_ref().map(|i| {
            (
                Binding::Storage(part(data, index_offset as u32, (i.len() * 4) as u32)),
                adv_gpu::encode::IndexFormat::U32,
            )
        });
        self.stream
            .draw_ranges(command, bindings, areas, line_ranges, endpoint.as_ref());
        let f = self.facades.get_mut(&facade).unwrap();
        f.stats.draws += draws as u64;
        let vertices = if line_ranges.is_empty() {
            count
        } else {
            line_ranges.iter().map(std::ops::Range::len).sum()
        };
        f.stats.vertices += (vertices * areas.len()) as u64;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_validated(
        &mut self,
        facade: u64,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
        clip: Option<Rect>,
    ) -> Result<(), Error> {
        let coordinates = |src: u32, dst: u32| {
            if filter == Filter::Nearest || dst <= src {
                (0., 1.)
            } else if src == 1 {
                (0.5, 0.5)
            } else {
                (
                    0.,
                    dst as f32 * (src - 1) as f32 / (src as f32 * (dst - 1) as f32),
                )
            }
        };
        let (u0, u1) = if rectangle.w == region.w {
            (
                rectangle.x as f32 / source.width() as f32,
                (rectangle.x + rectangle.w) as f32 / source.width() as f32,
            )
        } else {
            coordinates(rectangle.w, region.w)
        };
        let (v0, v1) = if rectangle.h == region.h {
            (
                rectangle.y as f32 / source.height() as f32,
                (rectangle.y + rectangle.h) as f32 / source.height() as f32,
            )
        } else {
            coordinates(rectangle.h, region.h)
        };
        let x0 = region.x as f32 * 2. / destination.width() as f32 - 1.;
        let x1 = (region.x + region.w) as f32 * 2. / destination.width() as f32 - 1.;
        let y0 = 1. - region.y as f32 * 2. / destination.height() as f32;
        let y1 = 1. - (region.y + region.h) as f32 * 2. / destination.height() as f32;
        let v = |x, y, u, v| Vertex {
            position: [x, y, 0., 1.],
            color: [1.; 4],
            uv: [u, v],
        };
        let vertices = [
            v(x0, y0, u0, v0),
            v(x1, y0, u1, v0),
            v(x1, y1, u1, v1),
            v(x0, y0, u0, v0),
            v(x1, y1, u1, v1),
            v(x0, y1, u0, v1),
        ];
        let target = Target {
            image: destination.clone(),
            depth: None,
        };
        let mut state = DrawState::new(destination.width(), destination.height());
        state.scissor = Some(region);
        state.sampler.min = filter;
        state.sampler.mag = filter;
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        let area = draw_area(&target, &state, clip);
        if area.w == 0 || area.h == 0 {
            return Ok(());
        }
        let pipeline = self.facades.get_mut(&facade).unwrap().pipelines.blit(
            &self.device,
            filter,
            color_format(destination),
            fixed_sampling_flags(Some(source)),
        )?;
        self.select(facade, &target);
        let opaque = opaque_quad(&vertices, &state, area);
        self.fixed_draw_clips(
            facade,
            &vertices,
            Some(source),
            &state,
            pipeline,
            &[(area, opaque)],
        )
    }

    fn copy_validated(
        &mut self,
        facade: u64,
        source: &Image,
        mut rectangle: Rect,
        destination: &Image,
        mut region: Rect,
        clip: Option<Rect>,
    ) {
        if let Some(clip) = clip {
            let clipped = region.clipped(clip);
            if clipped.empty() {
                return;
            }
            rectangle.x += clipped.x - region.x;
            rectangle.y += clipped.y - region.y;
            rectangle.w = clipped.w;
            rectangle.h = clipped.h;
            region = clipped;
        }
        self.stream.push(Command::Copy {
            source: source.clone(),
            rect: native_rect(rectangle),
            destination: destination.clone(),
            at: (region.x, region.y),
        });
        if let Some(f) = self.facades.get_mut(&facade) {
            f.stats.image_copy_bytes += u64::from(rectangle.w) * u64::from(rectangle.h) * 4;
        }
    }

    fn record_upload(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        source: super::stream::UploadSource,
    ) {
        let mip = match &image.storage {
            Storage::Fixed { level, .. } => level.unwrap_or(0),
            Storage::Swap(_) => 0,
        };
        self.stream.push(Command::Upload {
            source,
            pitch,
            image: image.clone(),
            mip,
            rect: adv_gpu::Rect {
                x: rectangle.x,
                y: rectangle.y,
                w: rectangle.w,
                h: rectangle.h,
            },
        });
    }

    fn execute_readback(
        &mut self,
        facade: u64,
        image: &Image,
        regions: &[Rect],
        worker: bool,
        mut transfer: Transfer,
    ) {
        let scratch;
        let image = if readback_copyable(image) {
            image
        } else {
            let desc = backing::image_desc(
                (image.width(), image.height()),
                adv_gpu::Format::Bgra8Unorm,
                adv_gpu::resource::Usage::SAMPLED
                    | adv_gpu::resource::Usage::COPY_DST
                    | adv_gpu::resource::Usage::COLOR
                    | adv_gpu::resource::Usage::COPY_SRC,
            );
            let raw = match self.device.create_image(&desc) {
                Ok(img) => img,
                Err(error) => {
                    transfer.finish(Err(gpu_error(error)));
                    return;
                }
            };
            scratch = Image {
                storage: Storage::Fixed {
                    view: raw.view().clone(),
                    image: raw,
                    level: None,
                },
                alpha: image.alpha_mode(),
            };
            for &area in regions {
                if let Err(error) =
                    self.blit_validated(facade, image, area, &scratch, area, Filter::Nearest, None)
                {
                    transfer.finish(Err(error));
                    return;
                }
            }
            &scratch
        };
        let size = regions
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
            .sum();
        let buffer = match self.device.create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "guest readback",
        }) {
            Ok(buffer) => buffer,
            Err(error) => {
                transfer.finish(Err(gpu_error(error)));
                return;
            }
        };
        let mut mapping = MappedPixels {
            buffer: buffer.clone(),
            format: color_format(image),
            opaque: image.alpha_mode() == gpu::AlphaMode::Opaque,
            reservation: None,
        };
        let device = self.device.clone();
        self.stream.push(Command::Readback {
            image: image.clone(),
            regions: regions.iter().copied().map(native_rect).collect(),
            into: buffer,
            done: Box::new(move || {
                let result = if let Some(lost) = device.lost() {
                    Err(Error::Device(lost))
                } else if transfer.completion.cancelled.load(Ordering::Acquire) {
                    Err(Error::Device("GPU readback cancelled".into()))
                } else if worker {
                    mapping.reservation = transfer.reservation.take();
                    Ok(ReadPixels::Mapping(mapping))
                } else {
                    Ok(ReadPixels::Bytes(mapping.copy()))
                };
                transfer.finish(result);
            }),
        });
        if let Some(f) = self.facades.get_mut(&facade) {
            f.stats.readbacks += 1;
        }
        #[cfg(feature = "graphics-counters")]
        self.stream.count_readback(size);
    }
}

fn upload_row_bytes(image: &Image, r: Rect) -> u32 {
    let bpp = match &image.storage {
        Storage::Fixed { image, .. } => match image.desc().format {
            adv_gpu::Format::R8Unorm => 1,
            adv_gpu::Format::B5g6r5Unorm
            | adv_gpu::Format::Bgr5a1Unorm
            | adv_gpu::Format::Bgra4Unorm
            | adv_gpu::Format::Abgr4Unorm => 2,
            adv_gpu::Format::Bgra8Unorm => 4,
            _ => unreachable!("executor image format"),
        },
        Storage::Swap(_) => 4,
    };
    r.w * bpp
}

struct Completion {
    result: Mutex<Option<Result<ReadPixels, Error>>>,
    ready: Condvar,
    cancelled: AtomicBool,
    finished: AtomicBool,
}
enum ReadPixels {
    Bytes(Vec<u8>),
    Mapping(MappedPixels),
}
struct MappedPixels {
    buffer: adv_gpu::Buffer,
    format: adv_gpu::Format,
    opaque: bool,
    reservation: Option<Reservation>,
}
impl MappedPixels {
    fn copy(self) -> Vec<u8> {
        // The driver-owned mapping is read only after its copy completed.
        let mapping = self.buffer.mapped().expect("readback mapping");
        let mut pixels = unsafe { mapping.as_ref() }.to_vec();
        if self.format == adv_gpu::Format::Rgba8Unorm {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        }
        if self.opaque {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }
        pixels
    }
}
/// An explicitly requested image transfer. Completion owns the mapped buffer
/// until its submission finishes, independently of the guest's handle.
pub struct Readback {
    pub width: u32,
    pub height: u32,
    completion: Arc<Completion>,
    job_taken: bool,
    worker_wake: Option<Arc<adv_sync::WakeEvent>>,
}
impl Readback {
    pub fn cancel(&self) {
        self.completion.cancelled.store(true, Ordering::Release);
    }
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, Error>> {
        let mut result = self.completion.result.lock().unwrap();
        if matches!(*result, Some(Ok(ReadPixels::Mapping(_)))) {
            return None;
        }
        result.take().map(|r| {
            r.map(|pixels| match pixels {
                ReadPixels::Bytes(bytes) => bytes,
                ReadPixels::Mapping(_) => unreachable!(),
            })
        })
    }
    pub fn take_completion_job(&mut self) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        self.take_completion_job_with(Ok)
    }
    /// Convert completed regions on the worker before publishing the bytes.
    pub fn take_completion_job_with(
        &mut self,
        transform: impl FnOnce(Vec<u8>) -> Result<Vec<u8>, Error> + Send + 'static,
    ) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        if std::mem::replace(&mut self.job_taken, true) {
            return None;
        }
        let completion = self.completion.clone();
        let wake = self.worker_wake.take();
        Some(Box::new(move || {
            let mut result = completion.result.lock().unwrap();
            while !completion.finished.load(Ordering::Acquire) {
                result = completion.ready.wait(result).unwrap();
            }
            if matches!(*result, Some(Ok(ReadPixels::Mapping(_)))) {
                let Some(Ok(ReadPixels::Mapping(mapping))) = result.take() else {
                    unreachable!()
                };
                drop(result);
                let bytes = transform(mapping.copy()).map(ReadPixels::Bytes);
                *completion.result.lock().unwrap() = Some(bytes);
                if let Some(wake) = wake {
                    wake.signal();
                }
            }
        }))
    }
}

impl Gpu {
    pub(super) fn attachment_dimensions(&self, width: u32, height: u32) -> Result<(), Error> {
        let limit = self.device.caps().limits.max_image_2d;
        if width == 0 || height == 0 || width.max(height) > limit {
            return Err(invalid("invalid attachment dimensions"));
        }
        self.check()
    }
    pub fn clear(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
    ) -> Result<(), Error> {
        self.clear_with_stencil(target, color, depth, None)
    }
    pub fn clear_region(
        &mut self,
        target: &Target,
        rectangle: Rect,
        mut color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        rectangle_in(&target.image, rectangle)?;
        if color.is_none() && depth.is_none() && stencil.is_none()
            || color.is_some_and(|c| c.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v)))
            || depth.is_some_and(|d| !d.is_finite() || !(0. ..=1.).contains(&d))
            || depth.is_some() && target.depth.is_none()
            || stencil.is_some()
                && target
                    .depth
                    .as_ref()
                    .is_none_or(|d| !d.format().has_stencil())
        {
            return Err(invalid("invalid clear value or attachment"));
        }
        let area = self.clip.map_or(rectangle, |clip| rectangle.clipped(clip));
        if area.empty() {
            return Ok(());
        }
        if target.image.alpha_mode() == gpu::AlphaMode::Opaque
            && let Some(color) = &mut color
        {
            color[3] = 1.;
        }
        self.check()?;
        self.execution.worker.admit_cmd(0, self.wake.get())?;
        self.execution.worker.push(
            WorkerItem::Clear {
                facade: self.facade_id,
                target: target.clone(),
                area,
                color,
                depth,
                stencil: stencil.map(|s| s & 255),
            },
            0,
        );
        Ok(())
    }
    pub fn clear_with_stencil(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        self.clear_region(target, whole(&target.image), color, depth, stencil)
    }
    pub fn draw(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        image: Option<&Image>,
        state: DrawState,
    ) -> Result<(), Error> {
        self.draw_inner(target, vertices, None, image, state, None)
    }
    /// Fixed-function draw taking a shared immutable vertex buffer and optional
    /// disjoint damage clips (deduplicating ring packing across all clips).
    pub fn draw_arc_clips(
        &mut self,
        target: &Target,
        vertices: Arc<Vec<Vertex>>,
        image: Option<&Image>,
        state: DrawState,
        clips: Option<&[Rect]>,
    ) -> Result<(), Error> {
        let slice_ref = vertices.clone();
        self.draw_inner(target, &slice_ref, Some(vertices), image, state, clips)
    }
    fn draw_inner(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        shared: Option<Arc<Vec<Vertex>>>,
        image: Option<&Image>,
        mut state: DrawState,
        clips: Option<&[Rect]>,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        validate_state(target, &state)?;
        validate_count(vertices.len(), state.topology)?;
        let byte_len = std::mem::size_of_val(vertices);
        if byte_len > MAX_UPLOAD
            || vertices.iter().any(|v| {
                v.position
                    .iter()
                    .chain(&v.color)
                    .chain(&v.uv)
                    .any(|v| !v.is_finite())
            })
        {
            return Err(invalid("invalid vertex data or upload size"));
        }
        if image.is_some_and(|image| image.shares_storage(&target.image)) {
            return Err(invalid("render target cannot sample itself"));
        }
        normalize(target, &mut state);
        let inclusive = gpu::line::prepare(vertices, image, &state)?;
        let clips = Clips::from_slice(clips, self.clip);
        let clip_count = clips.areas(target, &state, None).as_slice().len() as u64;
        if clip_count == 0 {
            return Ok(());
        }
        let draw_count = match inclusive {
            None => clip_count,
            Some(gpu::line::Inclusive::Strip { terminal }) => {
                clip_count * (1 + u64::from(terminal))
            }
            Some(gpu::line::Inclusive::List) => {
                let mut total = 0u64;
                for pair in vertices.as_chunks::<2>().0 {
                    total += clip_count;
                    if !gpu::line::degenerate(&pair[0], &pair[1], state.viewport) {
                        total += clip_count;
                    }
                }
                total
            }
        };
        self.check()?;
        self.execution.worker.admit_cmd(byte_len, self.wake.get())?;
        let payload = if let Some(shared) = shared {
            VertexPayload::Shared(shared)
        } else {
            let mut buf = self.execution.worker.take_verts_buf(vertices.len());
            buf.extend_from_slice(vertices);
            VertexPayload::Owned(buf)
        };
        self.execution.worker.push(
            WorkerItem::DrawFixed {
                facade: self.facade_id,
                target: target.clone(),
                vertices: payload,
                image: image.cloned(),
                state,
                inclusive,
                clips,
            },
            byte_len,
        );
        self.draws += draw_count;
        Ok(())
    }

    pub fn draw_programmable(
        &mut self,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
    ) -> Result<(), Error> {
        self.draw_programmable_inner(target, draw, None, state, None, None)
    }
    /// Programmable draw taking a shared `Arc<ProgrammableDraw>` and optional
    /// disjoint damage clips (deduplicating ring packing & bindings across all clips).
    pub fn draw_programmable_arc_clips(
        &mut self,
        target: &Target,
        draw: Arc<ProgrammableDraw>,
        state: &DrawState,
        clips: Option<&[Rect]>,
    ) -> Result<(), Error> {
        self.draw_programmable_line_ranges(target, draw, state, clips, None)
    }

    /// Encode inclusive line ranges and their endpoint points from one upload.
    pub fn draw_programmable_line_ranges(
        &mut self,
        target: &Target,
        draw: Arc<ProgrammableDraw>,
        state: &DrawState,
        clips: Option<&[Rect]>,
        line_ranges: Option<Arc<[std::ops::Range<u32>]>>,
    ) -> Result<(), Error> {
        let draw_ref = draw.clone();
        self.draw_programmable_inner(target, &draw_ref, Some(draw), state, clips, line_ranges)
    }

    fn draw_programmable_inner(
        &mut self,
        target: &Target,
        draw: &ProgrammableDraw,
        shared: Option<Arc<ProgrammableDraw>>,
        state: &DrawState,
        clips: Option<&[Rect]>,
        line_ranges: Option<Arc<[std::ops::Range<u32>]>>,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        validate_state(target, state)?;
        let count = draw
            .indices
            .as_ref()
            .map_or(draw.vertex_count as usize, |i| i.len());
        validate_count(count, state.topology)?;
        if draw.instance_count == 0 || count > 0x300000 {
            return Err(invalid("invalid programmable primitive count"));
        }
        if draw.streams.len() > 16
            || draw.textures.len() != draw.program.textures.len()
            || draw
                .program
                .textures
                .iter()
                .filter(|&&vertex| vertex)
                .count()
                > 4
            || draw
                .program
                .textures
                .iter()
                .filter(|&&vertex| !vertex)
                .count()
                > 16
        {
            return Err(invalid("invalid programmable bindings"));
        }
        if draw.program.source().len() > gpu::Program::MAX_SOURCE_LEN {
            return Err(invalid("shader source exceeds limit"));
        }
        let last = draw
            .indices
            .as_ref()
            .and_then(|i| i.maximum())
            .unwrap_or(draw.vertex_count.saturating_sub(1));
        if last >= draw.vertex_count {
            return Err(invalid("index outside vertex span"));
        }
        let mut locations = 0u16;
        let mut total = 0usize;
        for stream in &draw.streams {
            if stream.stride > 2048
                || !stream.stride.is_multiple_of(4)
                || stream.attributes.is_empty()
            {
                return Err(invalid("invalid vertex stride"));
            }
            for a in &stream.attributes {
                let end = a
                    .offset
                    .checked_add(a.format.size())
                    .ok_or_else(|| invalid("attribute offset overflow"))?;
                if a.location >= 16
                    || locations & (1 << a.location) != 0
                    || !a.offset.is_multiple_of(4)
                    || stream.stride != 0 && end > stream.stride
                    || u64::from(if stream.instance {
                        draw.instance_count - 1
                    } else {
                        last
                    }) * u64::from(stream.stride)
                        + u64::from(end)
                        > stream.bytes.len() as u64
                {
                    return Err(invalid("attribute outside vertex span"));
                }
                locations |= 1 << a.location;
            }
            total += stream.bytes.len().next_multiple_of(4);
        }
        if total > MAX_UPLOAD
            || draw
                .indices
                .as_ref()
                .is_some_and(|i| i.len() > MAX_UPLOAD / 4)
        {
            return Err(invalid("programmable upload exceeds limit"));
        }
        for binding in &draw.textures {
            if binding
                .image
                .as_ref()
                .is_some_and(|image| image.shares_storage(&target.image))
            {
                return Err(invalid("render target cannot sample itself"));
            }
        }
        let mut state = *state;
        normalize(target, &mut state);
        let clips = Clips::from_slice(clips, self.clip);
        let clip_count = clips.areas(target, &state, None).as_slice().len() as u64;
        if clip_count == 0 {
            return Ok(());
        }
        gpu::validate_program(&draw.program)?;
        self.check()?;
        let payload_bytes = total + draw.indices.as_ref().map_or(0, |i| i.len() * 4);
        self.execution
            .worker
            .admit_cmd(payload_bytes, self.wake.get())?;
        let draw = shared.unwrap_or_else(|| Arc::new(draw.clone()));
        self.execution.worker.push(
            WorkerItem::DrawProgrammable {
                facade: self.facade_id,
                target: target.clone(),
                draw,
                state,
                clips,
                line_ranges,
            },
            payload_bytes,
        );
        self.draws += clip_count;
        Ok(())
    }

    pub fn blit(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        rectangle_in(source, rectangle)?;
        rectangle_in(destination, region)?;
        if source.shares_storage(destination) {
            return Err(invalid("image cannot blit onto itself"));
        }
        if (rectangle.w, rectangle.h) == (region.w, region.h) {
            let copy_source = match &source.storage {
                Storage::Fixed { image, .. } => image
                    .desc()
                    .usage
                    .contains(adv_gpu::resource::Usage::COPY_SRC),
                Storage::Swap(_) => true,
            };
            return if color_format(source) == color_format(destination)
                && copy_source
                && !(source.alpha_mode() == gpu::AlphaMode::Opaque
                    && destination.alpha_mode() == gpu::AlphaMode::Stored)
            {
                self.enqueue_copy(source, rectangle, destination, region)
            } else {
                self.enqueue_blit(source, rectangle, destination, region, Filter::Nearest)
            };
        }
        if rectangle != whole(source) || filter != Filter::Linear {
            return Err(Error::Unavailable(
                "scaled blits require full-source LINEAR filtering".into(),
            ));
        }
        self.enqueue_blit(source, rectangle, destination, region, filter)
    }

    /// Copy an output image to a different extent with point sampling.
    pub fn point_copy(&mut self, source: &Image, destination: &Image) -> Result<(), Error> {
        let (input, output) = (whole(source), whole(destination));
        if (input.w, input.h) == (output.w, output.h) {
            self.blit(source, input, destination, output, Filter::Nearest)
        } else {
            self.enqueue_blit(source, input, destination, output, Filter::Nearest)
        }
    }

    fn enqueue_blit(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
    ) -> Result<(), Error> {
        self.check()?;
        self.execution.worker.admit_cmd(0, self.wake.get())?;
        let area = self.clip.map_or(region, |clip| region.clipped(clip));
        self.execution.worker.push(
            WorkerItem::Blit {
                facade: self.facade_id,
                source: source.clone(),
                rectangle,
                destination: destination.clone(),
                region,
                filter,
                clip: self.clip,
            },
            0,
        );
        if !area.empty() {
            self.draws += 1;
        }
        Ok(())
    }
    pub fn copy(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        rectangle_in(source, rectangle)?;
        let region = Rect {
            x,
            y,
            w: rectangle.w,
            h: rectangle.h,
        };
        rectangle_in(destination, region)?;
        if source.id() == destination.id() {
            return Err(invalid("image cannot copy onto itself"));
        }
        if color_format(source) != color_format(destination) {
            let color_blit = |image: &Image| {
                image.packed_attachment_format().is_some()
                    || matches!(
                        color_format(image),
                        adv_gpu::Format::Rgba8Unorm | adv_gpu::Format::Bgra8Unorm
                    )
            };
            if !(color_blit(source) && color_blit(destination)) {
                return Err(invalid("copy formats differ"));
            }
            return self.enqueue_blit(source, rectangle, destination, region, Filter::Nearest);
        }
        self.enqueue_copy(source, rectangle, destination, region)
    }
    fn enqueue_copy(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
    ) -> Result<(), Error> {
        self.check()?;
        self.execution.worker.admit_cmd(0, self.wake.get())?;
        self.execution.worker.push(
            WorkerItem::Copy {
                facade: self.facade_id,
                source: source.clone(),
                rectangle,
                destination: destination.clone(),
                region,
                clip: self.clip,
            },
            0,
        );
        Ok(())
    }
    pub fn present(
        &mut self,
        image: &Image,
        damage: Region,
        serial: u64,
        video: std::sync::Arc<[crate::frame::VideoPlane]>,
        windows: std::sync::Arc<[crate::frame::WindowLayer]>,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        self.check()?;
        if !self.wake.get() {
            self.execution.worker.admit_present(false)?;
        }
        self.present_admitted(image, damage, serial, video, windows);
        Ok(())
    }

    /// Check the shared present queue without waiting on its condition variable.
    pub fn try_admit_present(&self) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        self.check()?;
        self.execution.worker.admit_present(true)
    }

    /// Apply nonblocking worker admission for a presentation transaction.
    pub fn with_nonblocking_admission<T>(&mut self, submit: impl FnOnce(&mut Self) -> T) -> T {
        let wake = self.wake.replace(true);
        let result = submit(self);
        self.wake.set(wake);
        result
    }

    pub fn present_admitted(
        &mut self,
        image: &Image,
        damage: Region,
        serial: u64,
        video: std::sync::Arc<[crate::frame::VideoPlane]>,
        windows: std::sync::Arc<[crate::frame::WindowLayer]>,
    ) {
        self.execution.worker.push_present(WorkerItem::PresentSwap {
            image: image.clone(),
            damage,
            serial,
            video,
            windows,
        });
    }
    /// D3D9 D3DPRESENTFLAG_DISCARD_DEPTHSTENCIL ends the depth contents.
    pub fn discard_depth(&mut self, depth: &Depth) {
        self.execution
            .worker
            .push(WorkerItem::DiscardDepth(depth.clone()), 0);
    }
    pub fn flush(&mut self) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        self.check()?;
        self.execution.worker.push_boundary(WorkerItem::Flush, 0);
        Ok(())
    }
    pub fn readback(
        &mut self,
        image: &Image,
        wake: Option<Arc<adv_sync::WakeEvent>>,
    ) -> Result<Readback, Error> {
        self.readback_inner(image, &[whole(image)], wake, false, None)
    }

    pub fn readback_regions(
        &mut self,
        image: &Image,
        regions: &[Rect],
        wake: Option<Arc<adv_sync::WakeEvent>>,
    ) -> Result<Readback, Error> {
        self.readback_inner(image, regions, wake, true, None)
    }

    /// Observe the completed pixels on the GPU owner without waiting on the caller.
    pub fn observe_readback(
        &mut self,
        image: &Image,
        observe: impl FnOnce(Result<Vec<u8>, Error>) + Send + 'static,
    ) -> Result<(), Error> {
        self.readback_inner(image, &[whole(image)], None, false, Some(Box::new(observe)))?;
        Ok(())
    }

    fn readback_inner(
        &mut self,
        image: &Image,
        regions: &[Rect],
        wake: Option<Arc<adv_sync::WakeEvent>>,
        worker: bool,
        observe: Option<ReadbackObserver>,
    ) -> Result<Readback, Error> {
        for &rect in regions {
            rectangle_in(image, rect)?;
        }
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        self.check()?;
        self.readbacks
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.readback_limit).then_some(n + 1)
            })
            .map_err(|_| Error::Busy)?;
        let reservation = Reservation(self.readbacks.clone());
        let completion = Arc::new(Completion {
            result: Mutex::new(None),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let transfer = Transfer {
            completion: completion.clone(),
            reservation: Some(reservation),
            wake: wake.clone(),
            device: self.device.clone(),
            observe,
        };
        self.execution.worker.push_boundary(
            WorkerItem::Readback {
                facade: self.facade_id,
                image: image.clone(),
                regions: regions.to_vec(),
                worker,
                transfer,
            },
            0,
        );
        Ok(Readback {
            width: image.width(),
            height: image.height(),
            completion,
            job_taken: false,
            worker_wake: wake.filter(|_| worker),
        })
    }
    /// Preserve the regions at this point in the stream when CPU staging is busy.
    pub fn snapshot_readback_regions(
        &mut self,
        image: &Image,
        regions: &[Rect],
    ) -> Result<Image, Error> {
        let scratch = self
            .target(image.width(), image.height(), false)?
            .with_alpha_mode(image.alpha_mode())
            .image;
        self.execution.worker.push(
            WorkerItem::SnapshotCopy {
                facade: self.facade_id,
                source: image.clone(),
                destination: scratch.clone(),
                regions: regions.to_vec(),
            },
            0,
        );
        Ok(scratch)
    }
    pub fn set_readback_limit(&mut self, limit: usize) -> Result<(), Error> {
        if limit == 0 || limit > MAX_READBACKS {
            return Err(invalid("invalid readback budget"));
        }
        if self.readbacks.load(Ordering::Acquire) > limit {
            return Err(Error::Busy);
        }
        self.readback_limit = limit;
        Ok(())
    }
    pub fn poll(&self) -> Result<(), Error> {
        self.check()
    }
}
struct Reservation(Arc<AtomicUsize>);
fn readback_copyable(image: &Image) -> bool {
    matches!(
        color_format(image),
        adv_gpu::Format::Bgra8Unorm | adv_gpu::Format::Rgba8Unorm
    ) && match &image.storage {
        Storage::Fixed { image, .. } => image
            .desc()
            .usage
            .contains(adv_gpu::resource::Usage::COPY_SRC),
        Storage::Swap(_) => true,
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
type ReadbackObserver = Box<dyn FnOnce(Result<Vec<u8>, Error>) + Send>;

pub(super) struct Transfer {
    observe: Option<ReadbackObserver>,
    completion: Arc<Completion>,
    reservation: Option<Reservation>,
    wake: Option<Arc<adv_sync::WakeEvent>>,
    device: adv_gpu::Device,
}
impl Transfer {
    fn finish(&mut self, result: Result<ReadPixels, Error>) {
        if let Some(observe) = self.observe.take() {
            self.completion.finished.store(true, Ordering::Release);
            self.reservation.take();
            observe(result.map(|pixels| match pixels {
                ReadPixels::Bytes(bytes) => bytes,
                ReadPixels::Mapping(_) => unreachable!(),
            }));
            return;
        }
        let mut output = self.completion.result.lock().unwrap();
        *output = Some(result);
        self.reservation.take();
        if let Some(wake) = &self.wake {
            wake.signal();
        }
        self.completion.finished.store(true, Ordering::Release);
        drop(output);
        self.completion.ready.notify_all();
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        if !self.completion.finished.load(Ordering::Acquire) {
            self.finish(Err(Error::Device(self.device.lost().unwrap_or_else(
                || "GPU owner stopped before readback completed".into(),
            ))));
        }
    }
}

pub(super) fn gpu_error(error: adv_gpu::Error) -> Error {
    match error {
        adv_gpu::Error::Unavailable(m) => Error::Unavailable(m),
        adv_gpu::Error::Invalid(m) => Error::Invalid(m),
        adv_gpu::Error::Busy => Error::Busy,
        error => Error::Device(error.to_string()),
    }
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn whole(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    }
}
fn native_rect(r: Rect) -> adv_gpu::Rect {
    adv_gpu::Rect {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
    }
}
fn rectangle_in(image: &Image, r: Rect) -> Result<(), Error> {
    if r.empty()
        || r.x.checked_add(r.w).is_none_or(|x| x > image.width())
        || r.y.checked_add(r.h).is_none_or(|y| y > image.height())
    {
        return Err(invalid("rectangle is outside image"));
    }
    Ok(())
}
pub(super) fn color_format(image: &Image) -> adv_gpu::Format {
    match &image.storage {
        Storage::Fixed { image, .. } => image.desc().format,
        Storage::Swap(_) => backing::swap_format(),
    }
}
fn validate_count(count: usize, topology: gpu::Topology) -> Result<(), Error> {
    let valid = match topology {
        gpu::Topology::Points => count > 0,
        gpu::Topology::Lines => count > 0 && count.is_multiple_of(2),
        gpu::Topology::LineStrip => count >= 2,
        gpu::Topology::Triangles => count > 0 && count.is_multiple_of(3),
        gpu::Topology::TriangleStrip => count >= 3,
    };
    if !valid {
        return Err(invalid("invalid primitive count"));
    }
    Ok(())
}
fn validate_state(target: &Target, state: &DrawState) -> Result<(), Error> {
    let v = state.viewport;
    if [
        v.x,
        v.y,
        v.width,
        v.height,
        v.minimum_depth,
        v.maximum_depth,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || v.x < 0.
        || v.y < 0.
        || v.width <= 0.
        || v.height <= 0.
        || v.x + v.width > target.image.width() as f32
        || v.y + v.height > target.image.height() as f32
        || v.minimum_depth < 0.
        || v.maximum_depth > 1.
        || v.minimum_depth > v.maximum_depth
        || state.color_write & !15 != 0
        || !state.alpha_reference.is_finite()
        || !(0. ..=1.).contains(&state.alpha_reference)
        || (state.depth.is_some() || state.depth_write) && target.depth.is_none()
        || state.stencil.is_some()
            && target
                .depth
                .as_ref()
                .is_none_or(|d| !d.format().has_stencil())
    {
        return Err(invalid("invalid drawing state or viewport"));
    }
    if let Some(r) = state.scissor {
        rectangle_in(&target.image, r)?;
    }
    Ok(())
}
pub(super) fn normalize(target: &Target, state: &mut DrawState) {
    if let Some(s) = &mut state.stencil {
        s.read_mask &= 255;
        s.write_mask &= 255;
    }
    if target.image.alpha_mode() == gpu::AlphaMode::Opaque
        && let Some(blend) = &mut state.blend
    {
        let factor = |f| match f {
            gpu::Factor::DestinationAlpha => gpu::Factor::One,
            gpu::Factor::OneMinusDestinationAlpha | gpu::Factor::SourceAlphaSaturated => {
                gpu::Factor::Zero
            }
            f => f,
        };
        blend.source = factor(blend.source);
        blend.destination = factor(blend.destination);
    }
}
fn draw_area(target: &Target, state: &DrawState, clip: Option<Rect>) -> adv_gpu::Rect {
    let v = state.viewport;
    let viewport = Rect {
        x: v.x.floor() as u32,
        y: v.y.floor() as u32,
        w: (v.x + v.width).ceil() as u32 - v.x.floor() as u32,
        h: (v.y + v.height).ceil() as u32 - v.y.floor() as u32,
    };
    let area = state
        .scissor
        .unwrap_or_else(|| whole(&target.image))
        .clipped(viewport);
    native_rect(clip.map_or(area, |clip| area.clipped(clip)))
}
fn summary(
    state: &DrawState,
    pipeline: adv_gpu::RenderPipeline,
    area: adv_gpu::Rect,
    vertices: std::ops::Range<u32>,
) -> Draw {
    let v = state.viewport;
    let stencil_writes = state.stencil.is_some_and(|s| {
        s.write_mask != 0
            && [s.front, s.back].iter().any(|s| {
                [s.fail, s.depth_fail, s.pass]
                    .iter()
                    .any(|op| *op != gpu::StencilOp::Keep)
            })
    });
    let blend = state.blend_constant.unwrap_or(0);
    Draw {
        pipeline,
        bindings: 0..0,
        index: None,
        vertices,
        instances: 0..1,
        base_vertex: 0,
        viewport: [
            v.x,
            v.y,
            v.width,
            v.height,
            v.minimum_depth,
            v.maximum_depth,
        ],
        scissor: area,
        stencil_reference: state.stencil_reference & 255,
        blend_constant: [0, 8, 16, 24].map(|shift| ((blend >> shift) & 255) as f32 / 255.),
        area,
        writes_color: state.color_write != 0,
        uses_depth: state.depth.is_some() || state.depth_write || state.stencil.is_some(),
        writes_depth: state.depth_write || stencil_writes,
        opaque: None,
    }
}
fn part(bytes: Bytes, offset: u32, len: u32) -> Bytes {
    Bytes {
        span: bytes.span,
        offset: bytes.offset + offset,
        len,
    }
}
fn fixed_parameters(state: &DrawState, image: Option<&Image>) -> [u32; 8] {
    let op = |op| match op {
        gpu::TextureOp::Color => 0,
        gpu::TextureOp::Texture => 1,
        gpu::TextureOp::Multiply => 2,
        gpu::TextureOp::Add => 3,
        gpu::TextureOp::Subtract => 4,
        gpu::TextureOp::Multiply2x => 5,
        gpu::TextureOp::Multiply4x => 6,
    };
    let compare = match state.alpha_test {
        gpu::Compare::Never => 0,
        gpu::Compare::Less => 1,
        gpu::Compare::Equal => 2,
        gpu::Compare::LessEqual => 3,
        gpu::Compare::Greater => 4,
        gpu::Compare::NotEqual => 5,
        gpu::Compare::GreaterEqual => 6,
        gpu::Compare::Always => 7,
    };
    let mut sampler = state.sampler;
    sampler.first_level = sampler
        .first_level
        .min(image.map_or(1, Image::mip_levels) - 1);
    let s = gpu::sampler_parameters(sampler);
    [
        op(state.texture_op),
        op(state.alpha_texture_op),
        compare,
        state.alpha_reference.to_bits(),
        s[0],
        s[1],
        s[2],
        s[3],
    ]
}

/// Prove full coverage only for two axis-aligned triangles with a shared
/// diagonal, or their four-vertex strip. Coverage lost to clipping or tests
/// never admits a discard load.
fn opaque_quad(
    vertices: &[Vertex],
    state: &DrawState,
    area: adv_gpu::Rect,
) -> Option<adv_gpu::Rect> {
    if state.blend.is_some()
        || state.alpha_blend.is_some()
        || state.color_write != 15
        || state.alpha_test != gpu::Compare::Always
        || state
            .depth
            .is_some_and(|compare| compare != gpu::Compare::Always)
        || state.stencil.is_some_and(|stencil| {
            stencil.front.compare != gpu::Compare::Always
                || stencil.back.compare != gpu::Compare::Always
        })
        || state.cull != gpu::Cull::None
    {
        return None;
    }
    let indices: &[[usize; 3]] = match (state.topology, vertices.len()) {
        (gpu::Topology::Triangles, 6) => &[[0, 1, 2], [3, 4, 5]],
        (gpu::Topology::TriangleStrip, 4) => &[[0, 1, 2], [2, 1, 3]],
        _ => return None,
    };
    if vertices
        .iter()
        .any(|v| v.position[3] != 1. || !(0. ..=1.).contains(&v.position[2]))
    {
        return None;
    }
    let v = state.viewport;
    let xy = |i: usize| {
        [
            (vertices[i].position[0] + 1.) * v.width * 0.5 + v.x,
            (1. - vertices[i].position[1]) * v.height * 0.5 + v.y,
        ]
    };
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for i in 0..vertices.len() {
        let p = xy(i);
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    if lo[0] >= hi[0] || lo[1] >= hi[1] {
        return None;
    }
    let mut masks = [0u8; 2];
    for (t, triangle) in indices.iter().enumerate() {
        for &i in triangle {
            let p = xy(i);
            let x = if p[0] == lo[0] {
                0
            } else if p[0] == hi[0] {
                1
            } else {
                return None;
            };
            let y = if p[1] == lo[1] {
                0
            } else if p[1] == hi[1] {
                1
            } else {
                return None;
            };
            let bit = 1 << (y * 2 + x);
            if masks[t] & bit != 0 {
                return None;
            }
            masks[t] |= bit;
        }
    }
    let shared = masks[0] & masks[1];
    if masks[0] | masks[1] != 15
        || !matches!(shared, 0b1001 | 0b0110)
        || lo[0] > area.x as f32
        || lo[1] > area.y as f32
        || hi[0] < (area.x + area.w) as f32
        || hi[1] < (area.y + area.h) as f32
    {
        return None;
    }
    Some(area)
}

#[cfg(test)]
mod tests {
    use super::super::resources::tests::{executor, read};
    use super::*;

    fn quad(color: [f32; 4]) -> [Vertex; 6] {
        [
            (-1., -1.),
            (1., -1.),
            (-1., 1.),
            (-1., 1.),
            (1., -1.),
            (1., 1.),
        ]
        .map(|(x, y)| Vertex {
            position: [x, y, 0., 1.],
            color,
            uv: [(x + 1.) * 0.5, (1. - y) * 0.5],
        })
    }
    fn program() -> ProgrammableDraw {
        ProgrammableDraw {
            instance_count: 1,
            program: gpu::Program::new("struct C { f: array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C; @vertex fn vertex_main(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> { let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return vec4(p[i],0.,1.); } @fragment fn fragment_main()->@location(0) vec4<f32> { return c.f[0]; }".into(), vec![]),
            streams: vec![], indices: None, vertex_count: 3,
            vertex_constants: Default::default(), pixel_constants: Default::default(),
            textures: vec![], parameters: [[0; 4]; 40],
        }
    }

    // spec: GFX-REC-1
    #[test]
    fn invalid_program_is_rejected_before_reaching_the_worker() {
        let Some(mut gpu) = executor() else { return };
        let peer = gpu.fork().unwrap();
        let target = gpu.target(4, 4, false).unwrap();
        let mut draw = program();
        draw.program = gpu::Program::new("invalid WGSL".into(), vec![]);
        assert!(matches!(
            gpu.draw_programmable(&target, &draw, &DrawState::new(4, 4)),
            Err(Error::Invalid(_))
        ));
        gpu.drain_worker();
        assert!(gpu.check().is_ok());
        assert!(peer.check().is_ok());
    }

    // Same scene and exact pixels as renderer/program/tests.rs.
    // spec: GFX-BUFFER-2, GFX-CACHE-1
    #[test]
    fn facade_fixed_and_programmable_share_ordered_uploads() {
        let Some(mut gpu) = executor() else { return };
        let target = gpu.target(3, 1, false).unwrap();
        gpu.clear(&target, Some([0.; 4]), None).unwrap();
        let mut state = DrawState::new(3, 1);
        let mut draw = program();
        draw.pixel_constants.floats[0] = [0., 1., 0., 1.];
        gpu.draw(&target, &quad([1., 0., 0., 1.]), None, state)
            .unwrap();
        state.scissor = Some(Rect {
            x: 1,
            y: 0,
            w: 2,
            h: 1,
        });
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        state.scissor = Some(Rect {
            x: 2,
            y: 0,
            w: 1,
            h: 1,
        });
        state.alpha_reference = 0.5;
        gpu.draw(&target, &quad([0., 0., 1., 1.]), None, state)
            .unwrap();
        assert_eq!(
            read(&mut gpu, &target.image),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
        assert_eq!(gpu.stats().pipeline_cache_hits, 1);
        assert_eq!(gpu.stats().prebuild_misses, 2);
        assert_eq!(gpu.stats().compile_waits, 2);
        let stats = gpu.stats();
        gpu.flush_pipeline_cache().unwrap();
        assert_eq!(gpu.stats(), stats);

        let texture = gpu.image(1, 1).unwrap();
        gpu.upload(&texture, whole(&texture), 4, &[0, 0, 255, 255])
            .unwrap();
        state = DrawState::new(3, 1);
        state.texture_op = gpu::TextureOp::Texture;
        gpu.draw(&target, &quad([1.; 4]), Some(&texture), state)
            .unwrap();
        gpu.upload(&texture, whole(&texture), 4, &[0, 255, 0, 255])
            .unwrap();
        state.scissor = Some(Rect {
            x: 1,
            y: 0,
            w: 2,
            h: 1,
        });
        gpu.draw(&target, &quad([1.; 4]), Some(&texture), state)
            .unwrap();
        assert_eq!(
            read(&mut gpu, &target.image),
            [0, 0, 255, 255, 0, 255, 0, 255, 0, 255, 0, 255]
        );
    }

    // Authored padded 2x2 texture with exact nearest pixels.
    // Exercise both blit pipelines with the same authored colors.
    // spec: GFX-SAMPLER-4, GFX-REC-1
    #[test]
    fn facade_textured_draw_and_blits_preserve_exact_pixels() {
        let Some(mut gpu) = executor() else { return };
        let target = gpu.target(2, 2, false).unwrap();
        let image = gpu.image(2, 2).unwrap();
        gpu.upload(
            &image,
            whole(&image),
            12,
            &[
                0, 0, 255, 255, 0, 255, 0, 255, 19, 19, 19, 19, 255, 0, 0, 255, 255, 255, 255, 255,
            ],
        )
        .unwrap();
        let expected = [
            0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut state = DrawState::new(2, 2);
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        gpu.draw(&target, &quad([1.; 4]), Some(&image), state)
            .unwrap();
        assert_eq!(read(&mut gpu, &target.image), expected);
        gpu.clear(&target, Some([0.; 4]), None).unwrap();
        gpu.blit(
            &image,
            whole(&image),
            &target.image,
            whole(&target.image),
            Filter::Nearest,
        )
        .unwrap();
        assert_eq!(read(&mut gpu, &target.image), expected);
        let copied = gpu.target(2, 2, false).unwrap();
        gpu.blit(
            &target.image,
            whole(&target.image),
            &copied.image,
            whole(&copied.image),
            Filter::Nearest,
        )
        .unwrap();
        assert_eq!(read(&mut gpu, &copied.image), expected);
        let solid = gpu.image(1, 1).unwrap();
        gpu.upload(&solid, whole(&solid), 4, &expected[..4])
            .unwrap();
        gpu.blit(
            &solid,
            whole(&solid),
            &target.image,
            whole(&target.image),
            Filter::Linear,
        )
        .unwrap();
        assert_eq!(read(&mut gpu, &target.image), expected[..4].repeat(4));
    }

    // spec: GFX-SAMPLER-1, GFX-SAMPLER-4, GFX-CACHE-1
    #[test]
    fn facade_sampling_flags_follow_each_bound_image() {
        let Some(mut gpu) = executor() else { return };
        let target = gpu.target(1, 1, false).unwrap();
        let mut state = DrawState::new(1, 1);
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        let mut draw = program();
        draw.program = gpu::Program::new(
            r#"
@group(1) @binding(0) var image:texture_2d<f32>;
@group(1) @binding(1) var image_sampler:sampler;
@vertex fn vertex_main(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {
    let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
    return vec4(p[i],0.,1.);
}
@fragment fn fragment_main()->@location(0) vec4<f32> {
    return textureSampleLevel(image,image_sampler,vec2(0.5),0.0);
}"#
            .into(),
            vec![false],
        );
        for (format, input, expected) in [
            (
                gpu::TexelFormat::Bgra8,
                &[11, 22, 33, 0][..],
                [11, 22, 33, 0],
            ),
            (
                gpu::TexelFormat::Bgrx8,
                &[11, 22, 33, 0][..],
                [11, 22, 33, 255],
            ),
            (gpu::TexelFormat::A8, &[77][..], [0, 0, 0, 77]),
        ] {
            let image = gpu.texture(1, 1, 1, format).unwrap();
            gpu.upload(&image, whole(&image), input.len() as u32, input)
                .unwrap();
            gpu.draw(&target, &quad([1.; 4]), Some(&image), state)
                .unwrap();
            assert_eq!(read(&mut gpu, &target.image), expected);
            draw.textures = vec![gpu::ProgramTexture {
                image: Some(image.clone()),
                sampler: state.sampler,
            }];
            gpu.draw_programmable(&target, &draw, &state).unwrap();
            assert_eq!(read(&mut gpu, &target.image), expected);
            gpu.blit(
                &image,
                whole(&image),
                &target.image,
                whole(&target.image),
                Filter::Nearest,
            )
            .unwrap();
            assert_eq!(read(&mut gpu, &target.image), expected);
        }
    }

    // spec: GFX-SAMPLER-1, GFX-SAMPLER-4
    #[test]
    fn facade_programmable_samplers_and_adapters_follow_both_stages() {
        let Some(mut gpu) = executor() else { return };
        let target = gpu.target(1, 1, false).unwrap();
        let alpha = gpu.texture(1, 1, 1, gpu::TexelFormat::A8).unwrap();
        let opaque = gpu.texture(1, 1, 1, gpu::TexelFormat::Bgrx8).unwrap();
        gpu.upload(&alpha, whole(&alpha), 1, &[77]).unwrap();
        gpu.upload(&opaque, whole(&opaque), 4, &[11, 22, 33, 0])
            .unwrap();
        let mut draw = program();
        draw.program = gpu::Program::new(
            r#"
@group(1) @binding(0) var vs_image:texture_2d<f32>;
@group(1) @binding(1) var vs_sampler:sampler;
@group(1) @binding(2) var ps_image:texture_2d<f32>;
@group(1) @binding(3) var ps_sampler:sampler;
struct V { @builtin(position) position:vec4<f32>, @location(0) alpha:f32 }
@vertex fn vertex_main(@builtin(vertex_index) i:u32)->V {
    let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
    let value=textureSampleLevel(vs_image,vs_sampler,vec2(0.5),0.0);
    return V(vec4(p[i],0.,1.),value.a);
}
@fragment fn fragment_main(v:V)->@location(0) vec4<f32> {
    let value=textureSampleLevel(ps_image,ps_sampler,vec2(0.5),0.0);
    return vec4(v.alpha,value.g,value.b,value.a);
}"#
            .into(),
            vec![true, false],
        );
        draw.textures = [alpha, opaque]
            .map(|image| gpu::ProgramTexture {
                image: Some(image),
                sampler: DrawState::new(1, 1).sampler,
            })
            .to_vec();
        gpu.draw_programmable(&target, &draw, &DrawState::new(1, 1))
            .unwrap();
        assert_eq!(read(&mut gpu, &target.image), [11, 22, 77, 255]);
    }

    // spec: GFX-REC-1, GFX-PASS-1
    #[test]
    fn facade_clear_copy_readback_completion() {
        let mut gpu = match Gpu::request(!cfg!(any(target_vendor = "apple", target_os = "android")))
        {
            Ok(gpu) => gpu,
            Err(Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                return;
            }
            Err(error) => panic!("device: {error}"),
        };
        let make_image = |gpu: &Gpu| {
            let image = gpu
                .device
                .create_image(&backing::image_desc(
                    (4, 4),
                    adv_gpu::Format::Bgra8Unorm,
                    adv_gpu::resource::Usage::COLOR
                        | adv_gpu::resource::Usage::COPY_SRC
                        | adv_gpu::resource::Usage::COPY_DST
                        | adv_gpu::resource::Usage::SAMPLED,
                ))
                .unwrap();
            Image {
                storage: Storage::Fixed {
                    view: image.view().clone(),
                    image,
                    level: None,
                },
                alpha: gpu::AlphaMode::Stored,
            }
        };
        let source = make_image(&gpu);
        let destination = make_image(&gpu);
        let target = Target {
            image: source.clone(),
            depth: None,
        };
        let mut instanced = ProgrammableDraw {
            instance_count: 0,
            program: gpu::Program::new(Arc::from(""), Vec::new()),
            streams: vec![gpu::VertexStream {
                instance: true,
                bytes: vec![0; 16].into(),
                stride: 16,
                attributes: vec![gpu::Attribute {
                    location: 0,
                    offset: 0,
                    format: gpu::AttributeFormat::Float4,
                }],
            }],
            indices: None,
            vertex_count: 3,
            vertex_constants: Default::default(),
            pixel_constants: Default::default(),
            textures: Vec::new(),
            parameters: [[0; 4]; 40],
        };
        let state = DrawState::new(4, 4);
        assert!(matches!(
            gpu.draw_programmable(&target, &instanced, &state),
            Err(Error::Invalid(_))
        ));
        instanced.instance_count = 2;
        assert!(matches!(
            gpu.draw_programmable(&target, &instanced, &state),
            Err(Error::Invalid(_))
        ));
        let depth = gpu.depth(4, 4, gpu::DepthFormat::Float32).unwrap();
        gpu.clear(
            &Target {
                image: source.clone(),
                depth: Some(depth.clone()),
            },
            None,
            Some(1.),
        )
        .unwrap();
        gpu.discard_depth(&depth);
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        gpu.copy(&source, whole(&source), &destination, 0, 0)
            .unwrap();
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        let mut transfer = gpu.readback(&destination, Some(wake.clone())).unwrap();
        transfer.take_completion_job().unwrap()();
        assert!(transfer.take_completion_job().is_none());
        let pixels = transfer.poll().unwrap().unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 255, 255])
        );
        assert!(transfer.poll().is_none());
        assert!(wake.wait(Some(std::time::Duration::ZERO)).unwrap());
        let image = make_image(&gpu);
        let target = Target {
            image: image.clone(),
            depth: None,
        };
        gpu.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
        let mut desc = backing::image_desc(
            (4, 4),
            adv_gpu::Format::Bgra8Unorm,
            adv_gpu::resource::Usage::COLOR
                | adv_gpu::resource::Usage::COPY_SRC
                | adv_gpu::resource::Usage::COPY_DST,
        );
        desc.mips = 2;
        let mips = gpu.device.create_image(&desc).unwrap();
        let level = |n| Image {
            storage: Storage::Fixed {
                image: mips.clone(),
                view: mips
                    .create_view(&adv_gpu::resource::ViewDesc {
                        format: None,
                        dimension: None,
                        mips: n..n + 1,
                        layers: 0..1,
                        plane: 0,
                    })
                    .unwrap(),
                level: Some(n),
            },
            alpha: gpu::AlphaMode::Stored,
        };
        let (first, second) = (level(0), level(1));
        gpu.clear(
            &Target {
                image: first.clone(),
                depth: None,
            },
            Some([1., 0., 0., 1.]),
            None,
        )
        .unwrap();
        gpu.copy(
            &first,
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            &second,
            0,
            0,
        )
        .unwrap();
        let mut mip_readback = gpu.readback(&second, None).unwrap();
        mip_readback.take_completion_job().unwrap()();
        assert!(
            mip_readback
                .poll()
                .unwrap()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 255, 255])
        );
        let mut surfaces =
            crate::surface::Surfaces::new(4, 4, 0, 4096, crate::surface::Snapshot::default())
                .unwrap();
        let id = surfaces.insert_native(target).unwrap();
        surfaces.present_native(id, None, &mut gpu).unwrap();
        assert_eq!(surfaces.stats.presentations, 1);
        let mut transfer = surfaces.readback_native(id, &mut gpu).unwrap();
        transfer.take_completion_job().unwrap()();
        assert!(
            transfer
                .poll()
                .unwrap()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 255, 0, 255])
        );
    }

    // spec: GFX-REC-1, GFX-REC-5
    #[test]
    fn opacity_requires_full_coverage_and_no_fragment_rejection() {
        let v = |x, y| Vertex {
            position: [x, y, 0., 1.],
            color: [1.; 4],
            uv: [0.; 2],
        };
        let vertices = [
            v(-1., 1.),
            v(1., 1.),
            v(1., -1.),
            v(-1., 1.),
            v(1., -1.),
            v(-1., -1.),
        ];
        let mut state = DrawState::new(4, 4);
        let area = adv_gpu::Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        assert_eq!(opaque_quad(&vertices, &state, area), Some(area));
        state.alpha_test = gpu::Compare::Greater;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        state.alpha_test = gpu::Compare::Always;
        state.color_write = 7;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        state.color_write = 15;
        let mut vertices = vertices;
        vertices[5].position[0] = 0.;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        assert!(validate_count(2, gpu::Topology::Triangles).is_err());
    }

    // spec: GFX-REC-5
    #[test]
    fn multi_clip_draws_share_ring_data_and_render_exact_disjoint_regions() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let target = gpu.target(4, 4, false).unwrap();
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        let clips = [
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Rect {
                x: 2,
                y: 0,
                w: 1,
                h: 1,
            },
            Rect {
                x: 1,
                y: 2,
                w: 1,
                h: 1,
            },
            Rect {
                x: 3,
                y: 3,
                w: 1,
                h: 1,
            },
        ];
        // Fixed-function multi-clip draw (red)
        let verts = Arc::new(quad([1., 0., 0., 1.]).to_vec());
        let state = DrawState::new(4, 4);
        gpu.draw_arc_clips(&target, verts, None, state, Some(&clips))
            .unwrap();
        let pixels = read(&mut gpu, &target.image);
        for y in 0..4u32 {
            for x in 0..4u32 {
                let at = ((y * 4 + x) * 4) as usize;
                let hit = clips.iter().any(|c| c.x == x && c.y == y);
                let expected = if hit {
                    [0, 0, 255, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(&pixels[at..at + 4], &expected, "fixed pixel ({x},{y})");
            }
        }
        assert_eq!(gpu.stats().draws, 4);

        // Programmable multi-clip draw (green)
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        let mut draw = program();
        draw.pixel_constants = gpu::ConstantSnapshot::capture(draw.program.constants[1], |reg| {
            if reg == 0 {
                [0.0f32, 1.0, 0.0, 1.0].map(f32::to_bits)
            } else {
                [0; 4]
            }
        });
        gpu.draw_programmable_arc_clips(&target, Arc::new(draw), &state, Some(&clips))
            .unwrap();
        let pixels = read(&mut gpu, &target.image);
        for y in 0..4u32 {
            for x in 0..4u32 {
                let at = ((y * 4 + x) * 4) as usize;
                let hit = clips.iter().any(|c| c.x == x && c.y == y);
                let expected = if hit {
                    [0, 255, 0, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(
                    &pixels[at..at + 4],
                    &expected,
                    "programmable pixel ({x},{y})"
                );
            }
        }
        assert_eq!(gpu.stats().draws, 8);
    }

    // spec: GFX-REC-1
    #[test]
    fn vertex_and_upload_slices_snapshot_before_caller_mutation() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let target = gpu.target(2, 2, false).unwrap();
        let image = gpu.image(2, 2).unwrap();
        let whole = Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        // Immediately mutate caller upload bytes after upload() returns.
        let mut texels = [10u8, 20, 30, 255].repeat(4);
        gpu.upload(&image, whole, 8, &texels).unwrap();
        texels.fill(0xee);
        // Immediately mutate caller vertex slice after draw() returns.
        let mut verts = quad([1., 1., 1., 1.]);
        let mut state = DrawState::new(2, 2);
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        gpu.draw(&target, &verts, Some(&image), state).unwrap();
        for v in &mut verts {
            v.position = [0.; 4];
            v.color = [0.; 4];
        }
        let pixels = read(&mut gpu, &target.image);
        assert_eq!(pixels, [10, 20, 30, 255].repeat(4));
    }

    // spec: DEV-FRAME-1, GFX-REC-1
    #[test]
    fn rapid_present_burst_preserves_monotonic_order_and_backpressure_signals_wake() {
        struct OrderedPresenter {
            serials: mpsc::Sender<u64>,
        }
        impl crate::frame::Presenter for OrderedPresenter {
            fn presentable(&self, _: u32, _: u32) -> bool {
                false
            }
            fn present(&mut self, _: &mut adv_gpu::Queue, frame: crate::frame::Frame) {
                let _ = self.serials.send(frame.serial);
            }
            fn service(&mut self, _: &mut adv_gpu::Queue, _: Option<&crate::frame::Waker>) {}
        }
        let (device, queue) = match adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            ..Default::default()
        }) {
            Ok(opened) => opened,
            Err(adv_gpu::Error::Unavailable(_))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                return;
            }
            Err(error) => panic!("open device: {error}"),
        };
        let (tx, rx) = mpsc::channel();
        let mut gpu =
            Gpu::request_with_presenter(device, queue, Box::new(OrderedPresenter { serials: tx }))
                .unwrap();
        let target = gpu.presentation_target(2, 2, false).unwrap();
        let damage = Region::whole((2, 2));
        for serial in 1..=50u64 {
            gpu.clear(&target, Some([0., 0., 1., 1.]), None).unwrap();
            gpu.present(
                &target.image,
                damage.clone(),
                serial,
                Arc::from([]),
                Arc::from([]),
            )
            .unwrap();
        }
        let mut received = Vec::with_capacity(50);
        for _ in 0..50 {
            received.push(rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap());
        }
        assert_eq!(received, (1..=50).collect::<Vec<_>>());
        // Wait for frame 50's owner-side retirement before saturating the counter.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while gpu
            .execution
            .worker
            .shared
            .in_flight_frames
            .load(Ordering::Acquire)
            != 0
        {
            assert!(
                std::time::Instant::now() < deadline,
                "frame 50 did not retire"
            );
            std::thread::yield_now();
        }

        // Verify backpressure with WakeEvent: artificially saturate in_flight_frames,
        // verify admit_present returns Error::Busy without blocking, and wake signals on drain.
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        gpu.set_wake(Some(wake.clone()));
        gpu.execution
            .worker
            .shared
            .in_flight_frames
            .store(MAX_IN_FLIGHT_FRAMES, Ordering::Release);
        assert!(matches!(gpu.admit_present(), Err(Error::Busy)));
        gpu.execution
            .worker
            .shared
            .in_flight_frames
            .store(0, Ordering::Release);
        gpu.admit_present().unwrap();
        gpu.present(&target.image, damage, 51, Arc::from([]), Arc::from([]))
            .unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap(),
            51
        );
        gpu.set_wake(None);
    }

    // spec: DEV-FRAME-1
    #[test]
    fn cross_api_native_and_software_presents_preserve_order_and_capture_barrier() {
        struct CaptureOrderPresenter {
            events: mpsc::Sender<&'static str>,
            serials: mpsc::Sender<u64>,
        }
        impl crate::frame::Presenter for CaptureOrderPresenter {
            fn presentable(&self, _: u32, _: u32) -> bool {
                false
            }
            fn present(&mut self, _: &mut adv_gpu::Queue, frame: crate::frame::Frame) {
                let _ = self.serials.send(frame.serial);
                let _ = self.events.send("present");
            }
            fn service(&mut self, _: &mut adv_gpu::Queue, wake: Option<&crate::frame::Waker>) {
                if let Some(wake) = wake {
                    for reply in wake.captures() {
                        let _ = self.events.send("capture");
                        let _ = reply.send(Ok(crate::frame::Captured {
                            width: 1,
                            height: 1,
                            bgra: vec![0, 0, 0, 255],
                            serial: 6,
                        }));
                    }
                }
            }
        }
        let (device, queue) = match adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            ..Default::default()
        }) {
            Ok(opened) => opened,
            Err(adv_gpu::Error::Unavailable(_))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                return;
            }
            Err(error) => panic!("open device: {error}"),
        };
        let (ev_tx, ev_rx) = mpsc::channel();
        let (ser_tx, ser_rx) = mpsc::channel();
        let mut gpu = Gpu::request_with_presenter(
            device.clone(),
            queue,
            Box::new(CaptureOrderPresenter {
                events: ev_tx,
                serials: ser_tx,
            }),
        )
        .unwrap();
        let target = gpu.presentation_target(1, 1, false).unwrap();
        let damage = Region::whole((1, 1));
        for i in 1..=6u64 {
            if i % 2 == 1 {
                gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
                gpu.present(
                    &target.image,
                    damage.clone(),
                    i,
                    Arc::from([]),
                    Arc::from([]),
                )
                .unwrap();
            } else {
                let slot =
                    crate::frame::Slot::new(&device, 1, 1, 4, crate::surface::PixelFormat::Bgra32)
                        .unwrap();
                gpu.execution
                    .worker
                    .push_present(WorkerItem::PresentSoftware {
                        base: crate::frame::Base::Software {
                            slot,
                            format: crate::surface::PixelFormat::Bgra32,
                            palette: None,
                            damage: damage.clone(),
                        },
                        video: Arc::from([]),
                        windows: Arc::from([]),
                        serial: i,
                    });
            }
        }
        // Issue capture right after presenting serial 6: capture MUST arrive AFTER all 6 presents.
        let capture_rx = gpu.waker().capture();
        let captured = capture_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert_eq!((captured.width, captured.height), (1, 1));
        let mut serials = Vec::new();
        for _ in 0..6 {
            serials.push(
                ser_rx
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .unwrap(),
            );
        }
        assert_eq!(serials, vec![1, 2, 3, 4, 5, 6]);
        let mut events = Vec::new();
        for _ in 0..7 {
            events.push(
                ev_rx
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .unwrap(),
            );
        }
        assert_eq!(
            events,
            vec![
                "present", "present", "present", "present", "present", "present", "capture"
            ]
        );
        // Verify subsequent commands work after waker().capture() completes (no queued_cmds underflow).
        gpu.drain_worker();
        assert_eq!(
            gpu.execution
                .worker
                .shared
                .queued_cmds
                .load(Ordering::Acquire),
            0
        );
        let read_target = gpu.target(2, 2, false).unwrap();
        gpu.clear(&read_target, Some([0., 1., 0., 1.]), None)
            .unwrap();
        let verts = Arc::new(quad([0., 0., 1., 1.]).to_vec());
        gpu.draw_arc_clips(&read_target, verts, None, DrawState::new(2, 2), None)
            .unwrap();
        assert_eq!(
            read(&mut gpu, &read_target.image),
            [255, 0, 0, 255].repeat(4)
        );
    }

    // spec: GFX-REC-5
    #[test]
    fn multi_clip_draws_handle_more_than_eight_clips() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let target = gpu.target(8, 8, false).unwrap();
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        // 16 disjoint 1x1 clips in a checkerboard pattern (> 8 inline SmallVec capacity).
        let mut clips = Vec::new();
        for y in (0..8u32).step_by(2) {
            for x in (0..8u32).step_by(2) {
                clips.push(Rect { x, y, w: 1, h: 1 });
            }
        }
        assert_eq!(clips.len(), 16);
        let state = DrawState::new(8, 8);
        let verts = Arc::new(quad([1., 0., 0., 1.]).to_vec());
        gpu.draw_arc_clips(&target, verts, None, state, Some(&clips))
            .unwrap();
        let pixels = read(&mut gpu, &target.image);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let at = ((y * 8 + x) * 4) as usize;
                let expected = if x % 2 == 0 && y % 2 == 0 {
                    [0, 0, 255, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(&pixels[at..at + 4], &expected, "fixed 16-clip ({x},{y})");
            }
        }
        assert_eq!(gpu.stats().draws, 16);
    }

    // spec: GFX-REC-1
    #[test]
    fn worker_failure_is_per_facade_skips_queued_work_and_clears_on_reset() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let mut peer = gpu.fork().unwrap();
        let target = gpu.target(2, 2, false).unwrap();
        gpu.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
        gpu.drain_worker();

        // Simulate a per-facade worker error (e.g. backend shader build failure).
        *gpu.failure.lock().unwrap() = Some(Error::Unavailable("simulated pipeline error".into()));
        assert!(matches!(gpu.check(), Err(Error::Unavailable(_))));
        // Peer facade on the same worker stays unaffected.
        assert!(peer.check().is_ok());
        peer.clear(&target, Some([0., 0., 1., 1.]), None).unwrap();
        assert_eq!(read(&mut peer, &target.image), [255, 0, 0, 255].repeat(4));

        // Reset clears recoverable per-facade worker errors while preserving draw ability.
        gpu.clear_worker_failure();
        assert!(gpu.check().is_ok());
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        assert_eq!(read(&mut gpu, &target.image), [0, 0, 255, 255].repeat(4));
    }

    // spec: GFX-REC-1
    #[test]
    fn set_wake_backpressure_is_scoped_per_facade_and_blit_tracks_draw_count() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let peer = gpu.fork().unwrap();
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        gpu.set_wake(Some(wake));
        gpu.execution
            .worker
            .shared
            .in_flight_frames
            .store(MAX_IN_FLIGHT_FRAMES, Ordering::Release);
        assert!(matches!(gpu.admit_present(), Err(Error::Busy)));
        // Peer has no wake registered, so admit_present waits rather than returning Busy
        // once the frame budget is released.
        let shared = peer.execution.worker.shared.clone();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            shared.in_flight_frames.store(0, Ordering::Release);
            shared.space_cv.notify_all();
        });
        peer.admit_present().unwrap();
        releaser.join().unwrap();
        gpu.set_wake(None);

        // Verify draw_count() agrees with stats().draws across scaled blits.
        let src = gpu.image(1, 1).unwrap();
        let dst = gpu.target(2, 2, false).unwrap();
        gpu.upload(&src, whole(&src), 4, &[1, 2, 3, 255]).unwrap();
        gpu.blit(
            &src,
            whole(&src),
            &dst.image,
            whole(&dst.image),
            Filter::Linear,
        )
        .unwrap();
        assert_eq!(gpu.draw_count(), gpu.stats().draws);
    }

    // spec: GFX-REC-1
    #[test]
    fn drain_worker_releases_queued_counters_before_returning() {
        let Some(mut gpu) = executor() else {
            return;
        };
        let target = gpu.target(2, 2, false).unwrap();
        for _ in 0..200 {
            gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
            gpu.drain_worker();
            assert_eq!(
                gpu.execution
                    .worker
                    .shared
                    .queued_cmds
                    .load(Ordering::Acquire),
                0
            );
            assert_eq!(
                gpu.execution
                    .worker
                    .shared
                    .queued_bytes
                    .load(Ordering::Acquire),
                0
            );
        }
    }

    // spec: DEV-FRAME-1, GFX-REC-1
    #[test]
    fn in_flight_frame_budget_waits_for_gpu_owner_presentation() {
        struct GatedPresenter {
            entered: mpsc::Sender<u64>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl crate::frame::Presenter for GatedPresenter {
            fn presentable(&self, _: u32, _: u32) -> bool {
                false
            }
            fn present(&mut self, _: &mut adv_gpu::Queue, frame: crate::frame::Frame) {
                let _ = self.entered.send(frame.serial);
                let _ = self.release.lock().unwrap().recv();
            }
            fn service(&mut self, _: &mut adv_gpu::Queue, _: Option<&crate::frame::Waker>) {}
        }
        let (device, queue) = match adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            ..Default::default()
        }) {
            Ok(opened) => opened,
            Err(adv_gpu::Error::Unavailable(_))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                return;
            }
            Err(error) => panic!("open device: {error}"),
        };
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut gpu = Gpu::request_with_presenter(
            device,
            queue,
            Box::new(GatedPresenter {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
        )
        .unwrap();
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        gpu.set_wake(Some(wake.clone()));
        let target = gpu.presentation_target(2, 2, false).unwrap();
        let damage = Region::whole((2, 2));

        for serial in 1..=(MAX_IN_FLIGHT_FRAMES as u64) {
            gpu.admit_present().unwrap();
            gpu.clear(&target, Some([0., 0., 1., 1.]), None).unwrap();
            gpu.present_admitted(
                &target.image,
                damage.clone(),
                serial,
                Arc::from([]),
                Arc::from([]),
            );
        }
        // Draining the worker moves every PresentSwap into the owner's stream,
        // yet the owner is still blocked presenting frame 1.
        gpu.drain_worker();
        assert_eq!(
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            1
        );
        // TestGate holds its submissions mutex while owner::run executes a
        // chunk under test-support, so check worker admission directly while
        // the presenter is blocked inside frame 1.
        assert_eq!(
            gpu.execution
                .worker
                .shared
                .in_flight_frames
                .load(Ordering::Acquire),
            MAX_IN_FLIGHT_FRAMES
        );
        assert!(matches!(
            gpu.execution.worker.admit_present(true),
            Err(Error::Busy)
        ));

        let _ = wake.wait(Some(std::time::Duration::ZERO));
        let gen_before = wake.generation();
        release_tx.send(()).unwrap();
        assert!(wake.wait(Some(std::time::Duration::from_secs(10))).unwrap());
        assert_ne!(wake.generation(), gen_before);
        gpu.execution.worker.admit_present(true).unwrap();

        for _ in 1..MAX_IN_FLIGHT_FRAMES {
            release_tx.send(()).unwrap();
        }
        gpu.admit_present().unwrap();
        gpu.set_wake(None);
    }
}
