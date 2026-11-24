//! Explicit host presentation modes share their rectangle with pointer mapping.
pub const CUSTOM_ASPECT_MIN: u16 = 500;
pub const CUSTOM_ASPECT_MAX: u16 = 3000;
pub const CUSTOM_ZOOM_MIN: u16 = 500;
pub const CUSTOM_ZOOM_MAX: u16 = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scaling {
    #[default]
    Fit,
    Integer,
    Stretch,
    Custom {
        aspect: u16,
        zoom: u16,
    },
}
impl Scaling {
    pub(crate) fn rectangle(self, client: (f64, f64), canvas: (u32, u32)) -> [f64; 4] {
        let (cw, ch) = client;
        if cw <= 0.0 || ch <= 0.0 {
            return [0.0, 0.0, 0.0, 0.0];
        }
        match self {
            Self::Stretch => [0.0, 0.0, cw, ch],
            Self::Fit | Self::Integer => {
                let scale = (cw / f64::from(canvas.0)).min(ch / f64::from(canvas.1));
                let scale = if self == Self::Integer && scale >= 1.0 {
                    scale.floor()
                } else {
                    scale
                };
                let w = (f64::from(canvas.0) * scale).floor().max(1.0);
                let h = (f64::from(canvas.1) * scale).floor().max(1.0);
                [((cw - w) / 2.0).floor(), ((ch - h) / 2.0).floor(), w, h]
            }
            Self::Custom { aspect, zoom } => {
                let r = f64::from(aspect.clamp(CUSTOM_ASPECT_MIN, CUSTOM_ASPECT_MAX)) / 1000.0;
                let z = f64::from(zoom.clamp(CUSTOM_ZOOM_MIN, CUSTOM_ZOOM_MAX)) / 1000.0;
                let (fit_w, fit_h) = if cw / ch >= r {
                    (ch * r, ch)
                } else {
                    (cw, cw / r)
                };
                let w = (fit_w * z).floor().clamp(1.0, cw);
                let h = (fit_h * z).floor().clamp(1.0, ch);
                [((cw - w) / 2.0).floor(), ((ch - h) / 2.0).floor(), w, h]
            }
        }
    }
}
