//! Text records (WINE-UNIX-8) for device work: uploads, draws and copies that
//! move pixels holding text into device resources, and presents that show
//! them in a window. Direct3D resources are named by the frame recorder's
//! keys, which every frontend already computes for each operation;
//! DirectDraw surfaces live in guest memory and are named by their pixels.
#[cfg(feature = "gpu")]
use crate::record::{Kind, ResourceKey, damage};
use adv_device::surface::{PixelFormat, Rect, Surface};
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
    let rect = corners(effects.bounds);
    for &(target, _) in effects.writes.iter().filter(|(key, _)| key.2 == 0) {
        let destination = resource(target);
        match effects.kind {
            Kind::Upload => {
                if let Some(plane) = origin.upload {
                    let map = Map::offset(-rect[0], -rect[1]);
                    let source = Store::Memory(plane);
                    text_records::moved(
                        env,
                        MoveKind::Upload,
                        destination,
                        rect,
                        source,
                        Some(map),
                    );
                }
            }
            Kind::Draw | Kind::Copy => {
                let kind = if effects.kind == Kind::Draw {
                    MoveKind::Draw
                } else {
                    MoveKind::DeviceCopy
                };
                // A draw that blends reads its own target; that moves nothing.
                for &source in effects
                    .reads
                    .iter()
                    .filter(|key| key.2 == 0 && **key != target)
                {
                    let sampled = effects.samples.iter().find(|read| read.source == source);
                    let map = sampled
                        .map(|read| Map {
                            scale: read.mapping.scale,
                            offset: read.mapping.offset,
                        })
                        .or(origin.copy);
                    text_records::moved(env, kind, destination, rect, resource(source), map);
                }
            }
            Kind::Clear => {}
        }
    }
}

/// Note a present of `source` into `window`'s client area at `rect`.
#[cfg(feature = "gpu")]
pub(crate) fn presented(env: &Env<'_>, source: ResourceKey, window: u32, rect: Rect, map: Map) {
    let destination = Store::Window(window);
    let rect = corners(rect);
    text_records::moved(
        env,
        MoveKind::Present,
        destination,
        rect,
        resource(source),
        Some(map),
    );
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
