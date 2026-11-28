//! Text records (WINE-UNIX-8) for device work: uploads, draws and copies that
//! move pixels holding text into device resources, presents that show them
//! in a window, and readbacks that return them to guest memory. Direct3D
//! resources are named by the content keys every frontend already computes;
//! DirectDraw surfaces and Direct3D 11 map shadows live in guest memory and
//! are named by their pixels. Where a path has no `Env`, a [`Noter`] taken
//! at its entry, or a DirectDraw [`SurfaceMoves`] queue drained by its front,
//! carries the notes.
#[cfg(feature = "gpu")]
use crate::record::{Kind, ResourceKey, damage};
use adv_device::surface::{PixelFormat, Rect, Surface};
#[cfg(feature = "gpu")]
use adv_machine::Ext;
use adv_machine::{
    Env,
    text_records::{self, Map, MoveKind, Plane, Store},
};

#[cfg(feature = "gpu")]
pub(crate) fn resource(key: ResourceKey) -> Store {
    use adv_machine::text_records::Resource;
    Store::Resource(Resource {
        allocation: key.0,
        subresource: key.1,
    })
}

fn corners(rect: Rect) -> [i32; 4] {
    [rect.x, rect.y, rect.x + rect.w, rect.y + rect.h].map(|v| v as i32)
}

/// Whether records are on, so a frontend need not build what it would note.
pub(crate) fn on(env: &Env<'_>) -> bool {
    text_records::records(env.ext).is_some()
}

/// What a recorded operation reads that its effects do not name.
#[cfg(feature = "gpu")]
#[derive(Clone, Copy, Default)]
pub(crate) struct Origin {
    /// The guest pixels an upload reads, starting at the written rectangle.
    pub upload: Option<Plane>,
    /// How a copy maps its destination to its source.
    pub copy: Option<Map>,
}

/// Note what a recorded operation moves into its color destinations.
#[cfg(feature = "gpu")]
pub(crate) fn recorded(env: &Env<'_>, effects: &damage::Input<'_>, origin: Origin) {
    if !on(env) {
        return;
    }
    let kind = match effects.kind {
        Kind::Upload => {
            if let Some(plane) = origin.upload {
                for &(target, _) in effects.writes.iter().filter(|(key, _)| key.2 == 0) {
                    uploaded(env, plane, target, effects.bounds);
                }
            }
            return;
        }
        Kind::Draw => MoveKind::Draw,
        Kind::Copy => MoveKind::DeviceCopy,
        Kind::Clear => return,
    };
    drew(
        env,
        kind,
        effects.bounds,
        effects.writes,
        effects.reads,
        |source| {
            effects
                .samples
                .iter()
                .find(|read| read.source == source)
                .map(|read| Map {
                    scale: read.mapping.scale,
                    offset: read.mapping.offset,
                })
                .or(origin.copy)
        },
    );
}

/// Note an upload of guest `plane`, from its origin, to `bounds` of `target`.
#[cfg(feature = "gpu")]
pub(crate) fn uploaded(env: &Env<'_>, plane: Plane, target: ResourceKey, bounds: Rect) {
    let rect = corners(bounds);
    text_records::moved(
        env,
        MoveKind::Upload,
        resource(target),
        rect,
        Store::Memory(plane),
        Some(Map::offset(-rect[0], -rect[1])),
    );
}

/// Note a draw or device copy within `bounds` from each color resource it
/// reads into each one it writes, through `map` of that source.
#[cfg(feature = "gpu")]
pub(crate) fn drew(
    env: &Env<'_>,
    kind: MoveKind,
    bounds: Rect,
    writes: &[(ResourceKey, bool)],
    reads: &[ResourceKey],
    map: impl Fn(ResourceKey) -> Option<Map>,
) {
    if !on(env) {
        return;
    }
    let rect = corners(bounds);
    for &(target, _) in writes.iter().filter(|(key, _)| key.2 == 0) {
        // A draw that blends reads its own target; that moves nothing.
        for &source in reads.iter().filter(|key| key.2 == 0 && **key != target) {
            let destination = resource(target);
            text_records::moved(env, kind, destination, rect, resource(source), map(source));
        }
    }
}

/// Note a present of `source` into `window`'s client area at `rect`.
pub(crate) fn presented(env: &Env<'_>, source: Store, window: u32, rect: Rect, map: Map) {
    let destination = Store::Window(window);
    let rect = corners(rect);
    text_records::moved(env, MoveKind::Present, destination, rect, source, Some(map));
}

/// Text records' handle where a handler has split its `Env`: the calling
/// guest thread, taken only while records are on.
#[cfg(feature = "gpu")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Noter {
    thread: u32,
}

#[cfg(feature = "gpu")]
impl Noter {
    pub(crate) fn new(env: &Env<'_>) -> Option<Self> {
        on(env).then(|| Self {
            thread: text_records::thread(env),
        })
    }

    pub(crate) fn moved(
        self,
        ext: &Ext,
        kind: MoveKind,
        destination: Store,
        rect: Rect,
        source: Store,
        map: Option<Map>,
    ) {
        if let Some(mut records) = text_records::records(ext) {
            records.moved(self.thread, kind, destination, corners(rect), source, map);
        }
    }
}

/// The map from a destination rectangle to the source rectangle it reads.
pub(crate) fn rect_map(destination: Rect, source: Rect) -> Option<Map> {
    if destination.w == 0 || destination.h == 0 {
        return None;
    }
    let scale = [
        f64::from(source.w) / f64::from(destination.w),
        f64::from(source.h) / f64::from(destination.h),
    ];
    Some(Map {
        scale,
        offset: [
            f64::from(source.x) - f64::from(destination.x) * scale[0],
            f64::from(source.y) - f64::from(destination.y) * scale[1],
        ],
    })
}

/// The guest pixels of a DirectDraw surface, top row first.
pub(crate) fn surface(surface: &Surface) -> Store {
    let [pitch, rows] = [surface.pitch, surface.height].map(|v| v as i32);
    let (bits, stride) = if surface.bottom_up {
        let last = surface
            .pixels
            .wrapping_add((rows.max(1) - 1) as u32 * surface.pitch);
        (last, -pitch)
    } else {
        (surface.pixels, pitch)
    };
    Store::Memory(Plane {
        bits,
        stride,
        bpp: match surface.format {
            PixelFormat::Indexed4 => 4,
            PixelFormat::Indexed1 => 1,
            format => format.bytes() * 8,
        },
        origin: [0, 0],
        size: [surface.width as i32, rows],
    })
}

/// A move a DirectDraw call made, which its front notes once the call returns.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceMove {
    pub kind: MoveKind,
    /// None for the screen, which the front names by the window it shows.
    pub destination: Option<Store>,
    pub rect: Rect,
    pub source: Store,
    pub map: Option<Map>,
}

impl SurfaceMove {
    /// The screen showing `rect` of `surface`, the primary, unscaled.
    pub(crate) fn shown(surface: &Surface, rect: Rect) -> Self {
        Self {
            kind: MoveKind::Present,
            destination: None,
            rect,
            source: self::surface(surface),
            map: Some(Map::offset(0, 0)),
        }
    }

    /// Note the move, naming the screen by `window`; without one it is dropped.
    pub(crate) fn note(self, env: &Env<'_>, window: Option<u32>) {
        let Some(destination) = self.destination.or(window.map(Store::Window)) else {
            return;
        };
        text_records::moved(
            env,
            self.kind,
            destination,
            corners(self.rect),
            self.source,
            self.map,
        );
    }
}

/// The moves of the DirectDraw call in progress, kept only while records are on.
#[derive(Default)]
pub(crate) struct SurfaceMoves(Option<Vec<SurfaceMove>>);

impl SurfaceMoves {
    /// Begin a call, keeping its moves only when records are on.
    pub(crate) fn begin(&mut self, on: bool) {
        self.0 = on.then(Vec::new);
    }

    pub(crate) fn on(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn push(&mut self, moved: SurfaceMove) {
        if let Some(moves) = &mut self.0 {
            moves.push(moved);
        }
    }

    /// End the call: note its moves, naming the screen by `window`.
    pub(crate) fn end(&mut self, env: &Env<'_>, window: Option<u32>) {
        for moved in self.0.take().into_iter().flatten() {
            moved.note(env, window);
        }
    }
}
