//! Offscreen vector/text/image rendering. Device objects contain no guest handles.
//! Windows uses its system GDI+ rasterizer; other hosts use owned CPU images
//! and zeno paths behind the same device objects.
use std::sync::Arc;
pub mod matrix;
#[cfg(not(windows))]
mod portable;
#[cfg(windows)]
mod windows;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Canvas,
    Graphics,
    Family,
    Font,
    Format,
    Brush,
    Path,
    Gradient,
}
pub struct Runtime {
    #[cfg(windows)]
    token: usize,
}
pub struct Object {
    pub kind: Kind,
    _runtime: Arc<Runtime>,
    #[cfg(windows)]
    pointer: *mut std::ffi::c_void,
    // Image graphics must keep their target alive after guest DisposeImage.
    _backing: Option<Arc<Object>>,
    #[cfg(windows)]
    canvas: Option<windows::Canvas>,
    #[cfg(not(windows))]
    data: std::sync::Mutex<portable::Data>,
}
pub enum Create<'a> {
    Image(u32, u32, &'a [u8]),
    Canvas(u32, u32, &'a [u8]),
    Graphics(Arc<Object>),
    Family(&'a [u16]),
    SansSerif,
    Font(&'a Object, f32, i32, i32),
    Format(i32, u16),
    GenericFormat(bool),
    Solid(u32),
    Path(i32),
    PathPoints(&'a [[f32; 2]], &'a [u8], i32),
    Gradient(&'a Object),
    Clone(&'a Object),
}
#[derive(Clone, Copy)]
pub enum GraphicsProperty {
    CompositingMode,
    Smoothing,
    TextHint,
}
pub enum Op<'a> {
    GraphicsProperty(GraphicsProperty, Option<i32>),
    PathEdit(PathEdit<'a>),
    PathData,
    Pixels,
    Upload(&'a [u8]),
    Size,
    Align(i32),
    LineAlign(i32),
    StringProperty(u8),
    StringFlags(i32),
    Hotkey(i32),
    Trimming(i32),
    Digits(Option<(u16, i32)>),
    Tabs(Option<(f32, &'a [f32])>),
    CharacterRanges(&'a [[i32; 2]]),
    PathFill(Option<i32>),
    ResetPath,
    PathLine([f32; 4]),
    PathArc([f32; 4], f32, f32),
    ClosePath,
    PathString {
        text: &'a [u16],
        family: &'a Object,
        style: i32,
        size: f32,
        rect: [f32; 4],
        format: Option<&'a Object>,
    },
    Bounds,
    PointCount,
    CenterColor(u32),
    Focus(f32, f32),
    Gamma(bool),
    Surround(&'a [u32]),
    FillPath(&'a Object, &'a Object),
    DrawImage(&'a Object, [f32; 4]),
    DrawString {
        text: &'a [u16],
        font: &'a Object,
        rect: [f32; 4],
        format: Option<&'a Object>,
        brush: &'a Object,
    },
    Measure {
        text: &'a [u16],
        font: &'a Object,
        rect: [f32; 4],
        format: Option<&'a Object>,
    },
    Clear(u32),
    Clip(Vec<[f32; 4]>),
    Smoothing(i32),
    TextHint(i32),
    PixelOffset(Option<i32>),
    BrushType,
    SolidColor(Option<u32>),
    World(Option<matrix::Matrix>),
    ComposeWorld(matrix::Matrix, bool),
}
pub enum Reply {
    PathData(Vec<[f32; 2]>, Vec<u8>),
    Digits(u16, i32),
    Tabs(f32, Vec<f32>),
    Matrix(matrix::Matrix),
    None,
    Pixels(Vec<u8>),
    Size(u32, u32),
    Bounds([f32; 4]),
    Count(i32),
    Measure([f32; 4], i32, i32),
}
pub enum PathEdit<'a> {
    Lines(&'a [[f32; 2]], bool),
    Beziers(&'a [[f32; 2]]),
    Curve {
        points: &'a [[f32; 2]],
        offset: i32,
        segments: i32,
        tension: f32,
        closed: bool,
    },
    Rectangles(&'a [[f32; 4]]),
    Ellipse([f32; 4]),
    Pie([f32; 4], f32, f32),
    Start,
    CloseAll,
    Marker(bool),
    Transform(matrix::Matrix),
}
impl Runtime {
    pub fn new() -> Result<Arc<Self>, i32> {
        #[cfg(windows)]
        {
            windows::startup()
        }
        #[cfg(not(windows))]
        {
            Ok(Arc::new(Self {}))
        }
    }
    pub fn create(self: &Arc<Self>, create: Create<'_>) -> Result<Arc<Object>, i32> {
        #[cfg(windows)]
        {
            windows::create(self, create)
        }
        #[cfg(not(windows))]
        {
            portable::create(self, create)
        }
    }
}
impl Object {
    pub fn execute(&self, operation: Op<'_>) -> Result<Reply, i32> {
        #[cfg(windows)]
        {
            windows::execute(self, operation)
        }
        #[cfg(not(windows))]
        {
            portable::execute(self, operation)
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        #[cfg(windows)]
        windows::shutdown(self.token);
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        #[cfg(windows)]
        windows::destroy(self);
    }
}
