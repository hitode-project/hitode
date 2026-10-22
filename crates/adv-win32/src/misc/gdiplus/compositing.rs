//! Flat graphics properties read and mutate the existing raster object.
use super::*;
use adv_device::gdiplus::GraphicsProperty;
fn get(env: &mut Env<'_>, graphics: u32, output: GuestPtr<i32>, property: GraphicsProperty) -> i32 {
    if output.is_null()
        || adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
    {
        return INVALID;
    }
    properties::property(env, graphics, output, Op::GraphicsProperty(property, None))
}
#[win32(gdiplus,name="GdipGetCompositingMode",level=state)]
fn mode(env: &mut Env<'_>, graphics: GuestPtr<()>, output: GuestPtr<i32>) -> i32 {
    get(
        env,
        graphics.addr(),
        output,
        GraphicsProperty::CompositingMode,
    )
}
#[win32(gdiplus,name="GdipSetCompositingMode",level=implement)]
fn set_mode(env: &mut Env<'_>, graphics: GuestPtr<()>, mode: i32) -> Outcome {
    result(
        object(env, graphics.addr()).and_then(|o| {
            o.execute(Op::GraphicsProperty(
                GraphicsProperty::CompositingMode,
                Some(mode),
            ))
        }),
        env,
    )
}
#[win32(gdiplus,name="GdipGetSmoothingMode",level=state)]
fn smoothing(env: &mut Env<'_>, graphics: GuestPtr<()>, output: GuestPtr<i32>) -> i32 {
    get(env, graphics.addr(), output, GraphicsProperty::Smoothing)
}
#[win32(gdiplus,name="GdipGetTextRenderingHint",level=state)]
fn text_hint(env: &mut Env<'_>, graphics: GuestPtr<()>, output: GuestPtr<i32>) -> i32 {
    get(env, graphics.addr(), output, GraphicsProperty::TextHint)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [mode_ABI, set_mode_ABI, smoothing_ABI, text_hint_ABI] {
        registry.register(api);
    }
}
