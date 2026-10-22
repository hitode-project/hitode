//! Row-vector affine math, independent of guest objects and graphics backends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix(pub [f32; 6]);
impl Matrix {
    pub const IDENTITY: Self = Self([1., 0., 0., 1., 0., 0.]);
    pub fn finite(self) -> bool {
        self.0.iter().all(|x| x.is_finite())
    }
    pub fn point(self, [x, y]: [f32; 2], vector: bool) -> [f32; 2] {
        self.point64([f64::from(x), f64::from(y)], vector)
            .map(|x| x as f32)
    }
    pub fn point64(self, [x, y]: [f64; 2], vector: bool) -> [f64; 2] {
        let m = self.0.map(f64::from);
        [
            x * m[0] + y * m[2] + if vector { 0. } else { m[4] },
            x * m[1] + y * m[3] + if vector { 0. } else { m[5] },
        ]
    }
    /// Return the row-vector product, with translation composed after rotation.
    pub fn multiply(self, other: Self) -> Self {
        let a = self.0.map(f64::from);
        let b = other.0.map(f64::from);
        Self([
            (a[0] * b[0] + a[1] * b[2]) as f32,
            (a[0] * b[1] + a[1] * b[3]) as f32,
            (a[2] * b[0] + a[3] * b[2]) as f32,
            (a[2] * b[1] + a[3] * b[3]) as f32,
            (a[4] * b[0] + a[5] * b[2] + b[4]) as f32,
            (a[4] * b[1] + a[5] * b[3] + b[5]) as f32,
        ])
    }
    pub fn inverse(self) -> Option<Self> {
        let [a, b, c, d, x, y] = self.0.map(f64::from);
        let det = a * d - b * c;
        if det == 0. || !det.is_finite() {
            return None;
        }
        let result = Self([
            (d / det) as f32,
            (-b / det) as f32,
            (-c / det) as f32,
            (a / det) as f32,
            ((c * y - d * x) / det) as f32,
            ((b * x - a * y) / det) as f32,
        ]);
        result.finite().then_some(result)
    }
    pub fn invertible(self) -> bool {
        let [a, b, c, d, _, _] = self.0.map(f64::from);
        self.finite() && a * d != b * c
    }
    pub fn rectangle([x, y, w, h]: [f32; 4], points: [[f32; 2]; 3]) -> Option<Self> {
        if w == 0. || h == 0. {
            return None;
        }
        let [a, b, c] = points;
        let m11 = (f64::from(b[0]) - f64::from(a[0])) / f64::from(w);
        let m12 = (f64::from(b[1]) - f64::from(a[1])) / f64::from(w);
        let m21 = (f64::from(c[0]) - f64::from(a[0])) / f64::from(h);
        let m22 = (f64::from(c[1]) - f64::from(a[1])) / f64::from(h);
        let result = Self([
            m11 as f32,
            m12 as f32,
            m21 as f32,
            m22 as f32,
            (f64::from(a[0]) - f64::from(x) * m11 - f64::from(y) * m21) as f32,
            (f64::from(a[1]) - f64::from(x) * m12 - f64::from(y) * m22) as f32,
        ]);
        result.finite().then_some(result)
    }
}
