//! Temporary native matrices never escape the device adapter.
use super::*;
use crate::gdiplus::matrix::Matrix;
struct NativeMatrix(*mut c_void);
impl NativeMatrix {
    fn new(Matrix([a, b, c, d, x, y]): Matrix) -> Result<Self, i32> {
        let mut pointer = std::ptr::null_mut::<c_void>();
        status(unsafe {
            gp::GdipCreateMatrix2(a, b, c, d, x, y, (&mut pointer as *mut *mut c_void).cast())
        })?;
        if pointer.is_null() {
            return Err(3);
        }
        Ok(Self(pointer))
    }
}
impl Drop for NativeMatrix {
    fn drop(&mut self) {
        unsafe {
            gp::GdipDeleteMatrix(self.0.cast());
        }
    }
}
pub(super) fn get(graphics: *mut c_void) -> Result<Reply, i32> {
    let matrix = NativeMatrix::new(Matrix::IDENTITY)?;
    let mut values = [0.; 6];
    status(unsafe { gp::GdipGetWorldTransform(graphics.cast(), matrix.0.cast()) })?;
    status(unsafe { gp::GdipGetMatrixElements(matrix.0.cast(), values.as_mut_ptr()) })?;
    Ok(Reply::Matrix(Matrix(values)))
}
pub(super) fn set(graphics: *mut c_void, value: Matrix) -> Result<Reply, i32> {
    let matrix = NativeMatrix::new(value)?;
    status(unsafe { gp::GdipSetWorldTransform(graphics.cast(), matrix.0.cast()) })?;
    Ok(Reply::None)
}
pub(super) fn compose(graphics: *mut c_void, value: Matrix, prepend: bool) -> Result<Reply, i32> {
    let matrix = NativeMatrix::new(value)?;
    status(unsafe {
        gp::GdipMultiplyWorldTransform(graphics.cast(), matrix.0.cast(), i32::from(!prepend))
    })?;
    Ok(Reply::None)
}
