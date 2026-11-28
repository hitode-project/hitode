//! Text records (WINE-UNIX-8) for device work: uploads, draws and copies that
//! move pixels holding text into device resources, and presents that show
//! them in a window. Resources are named by the frame recorder's keys, which
//! every frontend already computes for each operation.
use crate::record::{Kind, ResourceKey, damage};
use adv_device::surface::Rect;
use adv_machine::{
    Env,
    text_records::{self, Map, MoveKind, Plane, Resource, Store},
};

pub(crate) fn resource(key: ResourceKey) -> Store {
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
#[derive(Clone, Copy, Default)]
pub(crate) struct Origin {
    /// The guest pixels an upload reads, starting at the written rectangle.
    pub upload: Option<Plane>,
    /// How a copy maps its destination to its source.
    pub copy: Option<Map>,
}

/// Note what a recorded operation moves into its color destinations.
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
