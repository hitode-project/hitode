//! Explicit host presentation modes share their rectangle with pointer mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scaling {
    #[default]
    Fit,
    Integer,
    Stretch,
}
impl Scaling {
    pub(crate) fn rectangle(self, client: (f64, f64), canvas: (u32, u32)) -> [f64; 4] {
        let (cw, ch) = client;
        if self == Self::Stretch {
            return [0.0, 0.0, cw, ch];
        }
        let scale = (cw / f64::from(canvas.0)).min(ch / f64::from(canvas.1));
        let scale = if self == Self::Integer && scale >= 1.0 {
            scale.floor()
        } else {
            scale
        };
        let w = (f64::from(canvas.0) * scale).floor();
        let h = (f64::from(canvas.1) * scale).floor();
        [((cw - w) / 2.0).floor(), ((ch - h) / 2.0).floor(), w, h]
    }
}
