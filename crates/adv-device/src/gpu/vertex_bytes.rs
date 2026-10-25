//! Captured vertex storage. Neutral vertices and point expansion stay as recipes
//! until a backend requests contiguous bytes; command hashing consumes the same
//! byte sequence without allocating that backend payload.
use super::Vertex;
use std::{
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub struct VertexBytes(Arc<Source>);
#[derive(Clone)]
enum Source {
    Raw(Vec<u8>),
    Fixed {
        vertices: Arc<Vec<Vertex>>,
        coordinates: Option<(usize, Arc<Vec<[f32; 4]>>)>,
        packed: OnceLock<Vec<u8>>,
    },
    Points {
        source: VertexBytes,
        count: u32,
        stride: usize,
        source_stride: usize,
        width: usize,
        packed: OnceLock<Vec<u8>>,
    },
}
impl VertexBytes {
    pub fn fixed(
        vertices: Arc<Vec<Vertex>>,
        coordinates: Option<(usize, Arc<Vec<[f32; 4]>>)>,
    ) -> Self {
        Self(Arc::new(Source::Fixed {
            vertices,
            coordinates,
            packed: OnceLock::new(),
        }))
    }
    pub fn points(self, count: u32, stride: usize, width: usize) -> Self {
        Self(Arc::new(Source::Points {
            source: self,
            count,
            source_stride: stride,
            stride: if stride == 0 { width } else { stride },
            width,
            packed: OnceLock::new(),
        }))
    }
    pub fn len(&self) -> usize {
        match &*self.0 {
            Source::Raw(v) => v.len(),
            Source::Fixed {
                vertices,
                coordinates,
                ..
            } => vertices.len() * coordinates.as_ref().map_or(40, |(n, _)| 32 + n * 16),
            Source::Points { count, stride, .. } => *count as usize * 6 * stride,
        }
    }
    /// Captured payload size, counted once per command even when shared.
    pub fn retained_bytes(&self) -> usize {
        match &*self.0 {
            Source::Raw(v) => v.len(),
            Source::Fixed {
                vertices,
                coordinates,
                packed,
            } => {
                std::mem::size_of_val(vertices.as_slice())
                    + coordinates
                        .as_ref()
                        .map_or(0, |(_, v)| std::mem::size_of_val(v.as_slice()))
                    + packed.get().map_or(0, Vec::len)
            }
            Source::Points { source, packed, .. } => {
                source.retained_bytes() + packed.get().map_or(0, Vec::len)
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Read one attribute without materializing a deferred backend stream.
    pub fn read_vertex(
        &self,
        n: usize,
        stride: usize,
        offset: usize,
        output: &mut [u8],
    ) -> Option<()> {
        match &*self.0 {
            Source::Raw(v) => {
                let at = n * stride + offset;
                output.copy_from_slice(v.get(at..at + output.len())?);
            }
            Source::Fixed {
                vertices,
                coordinates,
                ..
            } => {
                vertices.get(n)?;
                let width = coordinates.as_ref().map_or(40, |(n, _)| 32 + n * 16);
                if offset + output.len() > width {
                    return None;
                }
                let mut at = 0;
                self.vertex(n, stride, width, &mut |part| {
                    let lo = offset.max(at);
                    let hi = (offset + output.len()).min(at + part.len());
                    if lo < hi {
                        output[lo - offset..hi - offset].copy_from_slice(&part[lo - at..hi - at]);
                    }
                    at += part.len();
                });
            }
            Source::Points {
                source,
                count,
                source_stride,
                stride,
                width,
                ..
            } => {
                if n >= *count as usize * 6 || offset + output.len() > *stride {
                    return None;
                }
                output.fill(0);
                if offset < *width {
                    let size = output.len().min(width - offset);
                    source.read_vertex(n / 6, *source_stride, offset, &mut output[..size])?;
                }
            }
        }
        Some(())
    }
    fn vertex(&self, n: usize, stride: usize, mut width: usize, write: &mut impl FnMut(&[u8])) {
        match &*self.0 {
            Source::Raw(v) => write(&v[n * stride..n * stride + width]),
            Source::Fixed {
                vertices,
                coordinates,
                ..
            } => {
                let v = &vertices[n];
                let mut part = |bytes: &[u8]| {
                    let take = width.min(bytes.len());
                    write(&bytes[..take]);
                    width -= take;
                };
                if let Some((count, coordinates)) = coordinates {
                    part(bytemuck::bytes_of(&v.position));
                    part(bytemuck::bytes_of(&v.color));
                    part(bytemuck::cast_slice(
                        &coordinates[n * count..(n + 1) * count],
                    ));
                } else {
                    part(bytemuck::bytes_of(v));
                }
            }
            Source::Points {
                source,
                source_stride,
                width,
                ..
            } => source.vertex(n / 6, *source_stride, *width, write),
        }
    }
    fn visit(&self, write: &mut impl FnMut(&[u8])) {
        match &*self.0 {
            Source::Raw(v) => write(v),
            Source::Fixed {
                vertices,
                coordinates,
                ..
            } => {
                if let Some((n, _)) = coordinates {
                    let stride = 32 + n * 16;
                    for n in 0..vertices.len() {
                        self.vertex(n, stride, stride, write);
                    }
                } else {
                    write(bytemuck::cast_slice(vertices.as_slice()));
                }
            }
            Source::Points {
                source,
                count,
                stride,
                source_stride,
                width,
                ..
            } => {
                for n in 0..*count as usize {
                    for _ in 0..6 {
                        source.vertex(n, *source_stride, *width, write);
                        let mut padding = stride - width;
                        while padding != 0 {
                            let n = padding.min(16);
                            write(&[0; 16][..n]);
                            padding -= n;
                        }
                    }
                }
            }
        }
    }
}
impl Default for VertexBytes {
    fn default() -> Self {
        Vec::new().into()
    }
}
impl From<Vec<u8>> for VertexBytes {
    fn from(v: Vec<u8>) -> Self {
        Self(Arc::new(Source::Raw(v)))
    }
}
impl Deref for VertexBytes {
    type Target = Vec<u8>;
    fn deref(&self) -> &Vec<u8> {
        match &*self.0 {
            Source::Raw(v) => v,
            Source::Fixed { packed, .. } | Source::Points { packed, .. } => {
                packed.get_or_init(|| {
                    let mut bytes = Vec::with_capacity(self.len());
                    self.visit(&mut |part| bytes.extend_from_slice(part));
                    bytes
                })
            }
        }
    }
}
impl DerefMut for VertexBytes {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        if !matches!(&*self.0, Source::Raw(_)) {
            self.0 = Arc::new(Source::Raw(self.as_slice().to_vec()));
        }
        let Source::Raw(v) = Arc::make_mut(&mut self.0) else {
            unreachable!()
        };
        v
    }
}
impl Hash for VertexBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        self.visit(&mut |bytes| state.write(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::DefaultHasher;
    fn hash(v: &impl Hash) -> u64 {
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    // spec: GFX-REC-1, GFX-REC-2
    #[test]
    fn recipes_hash_like_packed_bytes_and_clones_preserve_captured_inputs() {
        let v = Vertex {
            position: [0., -0., 0.5, 1.],
            color: [1.; 4],
            uv: [0.25, 0.75],
        };
        let mut bytes = VertexBytes::fixed(vec![v].into(), None);
        let before = bytes.clone();
        let expected = bytemuck::bytes_of(&v).to_vec();
        assert_eq!(hash(&bytes), hash(&expected));
        assert!(matches!(&*bytes.0, Source::Fixed { packed, .. } if packed.get().is_none()));
        bytes[0] = 9;
        assert_eq!(&**before, &expected);
        let points = before.points(1, 40, 36);
        let mut expanded = Vec::new();
        for _ in 0..6 {
            expanded.extend_from_slice(&expected[..36]);
            expanded.extend_from_slice(&[0; 4]);
        }
        assert_eq!(hash(&points), hash(&expanded));
        assert_eq!(&**points, &expanded);
    }
    // spec: GFX-REC-1
    #[test]
    fn extended_coordinates_and_zero_stride_points_pack_exactly() {
        let v = Vertex {
            position: [0.; 4],
            color: [1.; 4],
            uv: [2.; 2],
        };
        let coordinates = vec![[3.; 4], [4.; 4]];
        let bytes = VertexBytes::fixed(vec![v].into(), Some((2, coordinates.clone().into())));
        let mut expected = bytemuck::bytes_of(&v)[..32].to_vec();
        expected.extend_from_slice(bytemuck::cast_slice(&coordinates));
        assert_eq!(hash(&bytes), hash(&expected));
        assert_eq!(&**bytes, &expected);
        let points = VertexBytes::from(vec![1, 2, 3, 4]).points(2, 0, 4);
        let expected = [1, 2, 3, 4].repeat(12);
        assert_eq!(points.len(), expected.len());
        assert_eq!(points.retained_bytes(), 4);
        assert_eq!(hash(&points), hash(&expected));
        let mut attribute = [0; 4];
        points.read_vertex(11, 4, 0, &mut attribute).unwrap();
        assert_eq!(attribute, [1, 2, 3, 4]);
        assert_eq!(points.retained_bytes(), 4);
        assert_eq!(&**points, &expected);
    }
}
