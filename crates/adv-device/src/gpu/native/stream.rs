//! The command stream from the execution thread to the GPU owner thread.
//!
//! The execution thread appends [`Command`]s to a chunk and writes their data
//! (vertices, constants, immediate texels) straight into its upload ring.
//! Replayable uploads hold pooled staging buffers until GPU completion.
//! Full chunks go to the owner thread, which encodes while the execution thread keeps
//! recording, so a frame costs the longer of the two, not their sum. Chunks
//! and their command storage come back for reuse: after warm-up, recording
//! allocates nothing. One lock per chunk, never per command; the owner thread
//! is woken only when it waits for work.

use super::Image;
use adv_gpu::encode::IndexFormat;
use adv_gpu::{Buffer, Rect, RenderPipeline, Ring, Sampler, Span};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

/// Commands per chunk: big enough that a hand-off costs nothing per command,
/// small enough that the owner starts encoding early in a frame.
const CHUNK: usize = 128;
/// Ring bytes reserved at a time for a chunk's small data.
const SPAN: u64 = 256 << 10;

/// Bytes in one of the chunk's ring spans.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bytes {
    pub span: u16,
    pub offset: u32,
    pub len: u32,
}

pub(crate) enum Binding {
    Uniform(Bytes),
    /// Vertex streams and other shader-read data in the ring.
    Storage(Bytes),
    /// A persistent buffer: static guest vertex or index data.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "persistent buffers are exercised by executor tests"
        )
    )]
    Buffer(Buffer, std::ops::Range<u64>),
    Sampled(Image),
    Sampler(Sampler),
}

pub(crate) struct Draw {
    pub pipeline: RenderPipeline,
    /// `Chunk::bindings[range]`: every slot the pipeline reads, by
    /// [`super::pipelines::slot`].
    pub bindings: std::ops::Range<u32>,
    pub index: Option<(Binding, IndexFormat)>,
    pub vertices: std::ops::Range<u32>,
    pub instances: std::ops::Range<u32>,
    pub base_vertex: i32,
    /// x, y, width, height, min depth, max depth.
    pub viewport: [f32; 6],
    pub scissor: Rect,
    pub stencil_reference: u32,
    pub blend_constant: [f32; 4],
    /// Pixels the draw may change: scissor ∩ viewport ∩ the executor's clip.
    pub area: Rect,
    /// See [`super::pass::OpKind::Draw`].
    pub writes_color: bool,
    pub uses_depth: bool,
    pub writes_depth: bool,
    pub opaque: Option<Rect>,
}

pub(crate) enum UploadSource {
    Ring(Bytes),
    Staged(super::Staged),
}

pub(crate) enum Command {
    /// Following clears and draws render to these attachments.
    Target {
        color: Image,
        depth: Option<super::Depth>,
    },
    Clear {
        area: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    /// `Chunk::draws[index]`.
    Draw(u32),
    /// Rows of `pitch` bytes into `image`'s `mip` at `rect`, in the image's
    /// own format: converting formats is the GPU's job.
    Upload {
        source: UploadSource,
        pitch: u32,
        image: Image,
        mip: u32,
        rect: Rect,
    },
    Copy {
        source: Image,
        rect: Rect,
        destination: Image,
        at: (u32, u32),
    },
    /// The executor's depth buffer is undefined from here
    /// (D3D9 `DISCARD_DEPTHSTENCIL`).
    DiscardDepth(super::Depth),
    /// Copy `rect` of `image` into `into` and submit; `done` runs when the
    /// bytes are readable.
    Readback {
        image: Image,
        rect: Rect,
        into: Buffer,
        done: Box<dyn FnOnce() + Send>,
    },
    /// Submit everything encoded so far (the producer needs ring space).
    Submit,
    /// End of a guest frame: submit and hand `image` to presentation.
    PresentSwap {
        image: Image,
        damage: crate::frame::Damage,
        serial: u64,
        video: Arc<[crate::frame::VideoPlane]>,
    },
    Present {
        base: crate::frame::Base,
        video: Arc<[crate::frame::VideoPlane]>,
        serial: u64,
    },
}

/// Commands, the draws they index, the draws' bindings and the ring spans
/// their [`Bytes`] point into. Draws and bindings live apart so that a
/// command stays small and a draw is not an allocation.
#[derive(Default)]
pub(crate) struct Chunk {
    pub commands: Vec<Command>,
    pub draws: Vec<Draw>,
    pub bindings: Vec<(u32, Binding)>,
    pub spans: Vec<Span>,
}

#[derive(Default)]
struct State {
    full: VecDeque<Chunk>,
    free: Vec<Chunk>,
    /// The owner thread waits for a chunk.
    idle: bool,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    work: Condvar,
    /// Signaled by the owner thread when submissions complete, so a producer
    /// whose ring is full can reserve again.
    retired: Condvar,
    completed: std::sync::atomic::AtomicU64,
}

pub(crate) fn channel(ring: Ring) -> (Producer, Consumer) {
    let shared = Arc::new(Shared::default());
    (
        Producer {
            shared: shared.clone(),
            chunk: Chunk::default(),
            ring,
            small: None,
        },
        Consumer { shared },
    )
}

/// The execution thread's end.
pub(crate) struct Producer {
    shared: Arc<Shared>,
    chunk: Chunk,
    ring: Ring,
    /// The chunk's span for small data and its next free byte.
    small: Option<(u16, u64)>,
}

impl Producer {
    /// Append a command. Chunks go to the owner when full, at frame and
    /// readback boundaries, and at a target change while the owner is idle.
    pub fn push(&mut self, command: Command) {
        let boundary = matches!(
            command,
            Command::Present { .. }
                | Command::PresentSwap { .. }
                | Command::Readback { .. }
                | Command::Submit
        );
        let target = matches!(command, Command::Target { .. });
        if target && !self.chunk.commands.is_empty() && self.shared.state.lock().unwrap().idle {
            self.hand_off();
        }
        self.chunk.commands.push(command);
        if boundary || self.chunk.commands.len() == CHUNK {
            self.hand_off();
        }
    }

    /// `draw.bindings` is ignored and set to `bindings`.
    pub fn draw(&mut self, mut draw: Draw, bindings: impl IntoIterator<Item = (u32, Binding)>) {
        let start = self.chunk.bindings.len() as u32;
        self.chunk.bindings.extend(bindings);
        draw.bindings = start..self.chunk.bindings.len() as u32;
        self.chunk.draws.push(draw);
        let index = self.chunk.draws.len() as u32 - 1;
        self.push(Command::Draw(index));
    }

    /// Reserve `len` bytes for this chunk's commands and let `write` fill
    /// them. Small data shares one span per chunk; large uploads get their
    /// own. Waits for the GPU only when the whole ring is in flight.
    pub fn bytes(&mut self, len: u32, align: u32, write: impl FnOnce(&mut [u8])) -> Bytes {
        let (len64, align64) = (u64::from(len), u64::from(align));
        let (span, offset) = if len64 > SPAN / 4 {
            (self.span(len64, align64), 0)
        } else {
            let fits = self
                .small
                .map(|(span, cursor)| (span, cursor.next_multiple_of(align64)))
                .filter(|(_, offset)| offset + len64 <= SPAN);
            let (span, offset) = fits.unwrap_or_else(|| (self.span(SPAN, align64), 0));
            self.small = Some((span, offset + len64));
            (span, offset)
        };
        let bytes = self.chunk.spans[usize::from(span)].bytes();
        write(&mut bytes[offset as usize..(offset + len64) as usize]);
        Bytes {
            span,
            offset: offset as u32,
            len,
        }
    }

    fn span(&mut self, size: u64, align: u64) -> u16 {
        let span = loop {
            match self.ring.reserve(size, align) {
                Ok(span) => break span,
                Err(adv_gpu::Error::Busy) => {
                    // The ring is in flight or held by recorded commands: have
                    // the owner submit everything, then wait for completions.
                    self.wait();
                }
                Err(error) => panic!("upload ring: {error}"),
            }
        };
        self.chunk.spans.push(span);
        (self.chunk.spans.len() - 1) as u16
    }

    pub fn completed(&self) -> u64 {
        self.shared
            .completed
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn wait(&mut self) {
        self.push(Command::Submit);
        let state = self.shared.state.lock().unwrap();
        drop(
            self.shared
                .retired
                .wait_timeout(state, std::time::Duration::from_millis(1))
                .unwrap(),
        );
    }

    fn hand_off(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        let next = state.free.pop().unwrap_or_default();
        let full = std::mem::replace(&mut self.chunk, next);
        state.full.push_back(full);
        if state.idle {
            self.shared.work.notify_one();
        }
        drop(state);
        self.small = None;
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        if !self.chunk.commands.is_empty() {
            self.hand_off();
        }
        self.shared.state.lock().unwrap().closed = true;
        self.shared.work.notify_one();
    }
}

/// The owner thread's end.
pub(crate) struct Consumer {
    shared: Arc<Shared>,
}

impl Consumer {
    /// The next chunk, waiting while there is none; `None` once the producer
    /// is gone and every chunk was taken.
    pub fn next(&self) -> Option<Chunk> {
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(chunk) = state.full.pop_front() {
                state.idle = false;
                return Some(chunk);
            }
            if state.closed {
                return None;
            }
            state.idle = true;
            state = self.shared.work.wait(state).unwrap();
        }
    }

    /// The next chunk if one is ready, without waiting.
    pub fn try_next(&self) -> Option<Chunk> {
        self.shared.state.lock().unwrap().full.pop_front()
    }

    /// Return an encoded chunk. Its spans must already be retained by the
    /// encoder that read them.
    pub fn recycle(&self, mut chunk: Chunk) {
        debug_assert!(chunk.spans.is_empty(), "spans go to the encoder");
        chunk.commands.clear();
        chunk.draws.clear();
        chunk.bindings.clear();
        self.shared.state.lock().unwrap().free.push(chunk);
    }

    /// Submissions completed: a producer waiting for ring space may retry.
    pub fn retired(&self, completed: adv_gpu::Token) {
        self.shared
            .completed
            .store(completed.0, std::sync::atomic::Ordering::Release);
        self.shared.retired.notify_all();
    }
}

/// Shared only on the execution thread; borrowing does not take a lock.
#[derive(Clone)]
pub(crate) struct ExecutionStream(pub std::rc::Rc<std::cell::RefCell<Producer>>);
impl ExecutionStream {
    pub fn push(&mut self, command: Command) {
        self.0.borrow_mut().push(command);
    }
    pub fn bytes(&mut self, len: u32, align: u32, write: impl FnOnce(&mut [u8])) -> Bytes {
        self.0.borrow_mut().bytes(len, align, write)
    }
    pub fn draw(&mut self, draw: Draw, bindings: impl IntoIterator<Item = (u32, Binding)>) {
        self.0.borrow_mut().draw(draw, bindings);
    }
}
