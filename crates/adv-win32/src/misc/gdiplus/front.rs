//! SDK-checked flat GDI+ fronts. All pointer contents are copied into owned DTOs.
#![allow(clippy::too_many_arguments)]
use super::*;
#[win32(gdiplus,name="GdiplusStartup",level=state)]
fn startup(
    env: &mut Env<'_>,
    out: GuestPtr<u32>,
    input: GuestPtr<()>,
    output: GuestPtr<()>,
) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let mut bytes = [0; 16];
    if env.cpu.read(input.addr(), &mut bytes).is_err() {
        return Outcome::Return(2);
    }
    let word = |n: usize| u32::from_le_bytes(bytes[n * 4..n * 4 + 4].try_into().unwrap());
    if word(0) != 1 {
        return Outcome::Return(17);
    }
    if word(1) != 0 || word(2) != 0 {
        return fail(
            env,
            "gdiplus.dll!GdiplusStartup",
            "debug callbacks and background-thread suppression hooks are unavailable",
        );
    }
    if !output.is_null() && env.cpu.write(output.addr(), &[0; 8]).is_err() {
        return Outcome::Return(2);
    }
    let runtime = match Runtime::new() {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    publish(env, out.addr(), Ok(Value::Token(runtime)))
}
#[win32(gdiplus,name="GdiplusShutdown",level=state)]
fn shutdown(env: &mut Env<'_>, token: u32) -> Outcome {
    if matches!(value(env, token), Some(Value::Token(_))) {
        remove(env, token);
    }
    Outcome::Return(0)
}
#[win32(gdiplus,name="GdipAlloc",level=state)]
fn alloc(env: &mut Env<'_>, size: u32) -> GuestPtr<()> {
    GuestPtr::new(task_alloc(env, size))
}
#[win32(gdiplus,name="GdipFree",level=state)]
fn free(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    super::super::ole_memory::free_value(env, pointer.addr())
}
#[win32(gdiplus,name="GdipCreateFromHDC",level=state)]
fn from_hdc(env: &mut Env<'_>, dc: Handle, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let (_, s) = surface(env, dc.0)?;
        let pixels = surface_pixels(env, &s)?;
        let runtime = runtime(env)?;
        let object = runtime.create(Create::Canvas(s.width, s.height, &pixels))?;
        let image = Arc::new(Image {
            object,
            width: s.width,
            height: s.height,
            scan: None,
        });
        let object = runtime.create(Create::Graphics(Arc::clone(&image.object)))?;
        Ok(Value::Graphics(Graphics {
            object,
            image,
            dc: Some(dc.0),
        }))
    })();
    if matches!(value, Err(6)) {
        return fail(
            env,
            "gdiplus.dll!GdipCreateFromHDC",
            "HDC has no drawable shared backing or supported mapping",
        );
    }
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateFromHDC2",level=state)]
fn from_hdc2(env: &mut Env<'_>, dc: Handle, device: Handle, out: GuestPtr<()>) -> Outcome {
    if device.0 != 0 {
        clear(env, out.addr());
        return fail(
            env,
            "gdiplus.dll!GdipCreateFromHDC2",
            "external device handles are unavailable",
        );
    }
    from_hdc(env, dc, out)
}
#[win32(gdiplus,name="GdipCreateBitmapFromScan0",level=state)]
fn scan0(
    env: &mut Env<'_>,
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    pointer: GuestPtr<()>,
    out: GuestPtr<()>,
) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        if width <= 0
            || height <= 0
            || u64::from(width as u32) * u64::from(height as u32) * 4 > MAX_BYTES as u64
        {
            return Err(2);
        }
        if ![0x21808, 0x22009, 0x26200a, 0xe200b].contains(&format) {
            return Err(6);
        }
        let scan = (!pointer.is_null()).then_some(Scan {
            pointer: pointer.addr(),
            stride,
            format,
        });
        let pixels = match scan {
            Some(scan) => image_pixels(env, width as u32, height as u32, scan)?,
            None => vec![0; width as usize * height as usize * 4],
        };
        let object = runtime(env)?.create(Create::Image(width as u32, height as u32, &pixels))?;
        Ok(Value::Image(Arc::new(Image {
            object,
            width: width as u32,
            height: height as u32,
            scan,
        })))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateBitmapFromStream",level=state)]
fn bitmap_stream(env: &mut Env<'_>, stream: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    read_stream(env, stream.addr(), out.addr(), false)
}
#[win32(gdiplus,name="GdipCreateBitmapFromStreamICM",level=safe_fail)]
fn bitmap_icm(env: &mut Env<'_>, stream: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    read_stream(env, stream.addr(), out.addr(), true)
}
#[win32(gdiplus,name="GdipGetImageGraphicsContext",level=state)]
fn image_graphics(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let image = image(env, handle.addr())?;
        refresh_image(env, &image)?;
        let object = runtime(env)?.create(Create::Graphics(Arc::clone(&image.object)))?;
        Ok(Value::Graphics(Graphics {
            object,
            image,
            dc: None,
        }))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateFontFamilyFromName",level=state)]
fn family(
    env: &mut Env<'_>,
    name: WideStr,
    collection: GuestPtr<()>,
    out: GuestPtr<()>,
) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    if !collection.is_null() {
        return fail(
            env,
            "gdiplus.dll!GdipCreateFontFamilyFromName",
            "private GDI+ font collections are unavailable",
        );
    }
    let value = (|| {
        let mut name = name.read(env.cpu, 256).map_err(|_| 2)?;
        name.push(0);
        Ok(Value::Object(runtime(env)?.create(Create::Family(&name))?))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipGetGenericFontFamilySansSerif",level=state)]
fn sans(env: &mut Env<'_>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = runtime(env)
        .and_then(|r| r.create(Create::SansSerif))
        .map(Value::Object);
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateFont",level=state)]
fn font(
    env: &mut Env<'_>,
    family: GuestPtr<()>,
    size: f32,
    style: i32,
    unit: i32,
    out: GuestPtr<()>,
) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let family = object(env, family.addr())?;
        Ok(Value::Object(
            runtime(env)?.create(Create::Font(&family, size, style, unit))?,
        ))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateSolidFill",level=state)]
fn solid(env: &mut Env<'_>, color: u32, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = runtime(env)
        .and_then(|r| r.create(Create::Solid(color)))
        .map(Value::Object);
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreatePath",level=state)]
fn path(env: &mut Env<'_>, mode: i32, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = runtime(env)
        .and_then(|r| r.create(Create::Path(mode)))
        .map(Value::Object);
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreatePathGradientFromPath",level=state)]
fn gradient(env: &mut Env<'_>, path: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let path = object(env, path.addr())?;
        Ok(Value::Object(
            runtime(env)?.create(Create::Gradient(&path))?,
        ))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCreateStringFormat",level=state)]
fn string_format(env: &mut Env<'_>, flags: i32, language: u32, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = runtime(env)
        .and_then(|r| r.create(Create::Format(flags, language as u16)))
        .map(Value::Object);
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCloneImage",level=state)]
fn clone_image(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let i = image(env, handle.addr())?;
        refresh_image(env, &i)?;
        let object = runtime(env)?.create(Create::Clone(&i.object))?;
        Ok(Value::Image(Arc::new(Image {
            object,
            width: i.width,
            height: i.height,
            scan: None,
        })))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipCloneBrush",level=state)]
fn clone_brush(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<()>) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = (|| {
        let o = object(env, handle.addr())?;
        if !matches!(o.kind, Kind::Brush | Kind::Gradient) {
            return Err(2);
        }
        Ok(Value::Object(runtime(env)?.create(Create::Clone(&o))?))
    })();
    publish(env, out.addr(), value)
}
#[win32(gdiplus,name="GdipGetImageWidth",level=state)]
fn width(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<u32>) -> Outcome {
    image_size(env, handle.addr(), out.addr(), false)
}
#[win32(gdiplus,name="GdipGetImageHeight",level=state)]
fn height(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<u32>) -> Outcome {
    image_size(env, handle.addr(), out.addr(), true)
}
fn image_size(env: &mut Env<'_>, handle: u32, out: u32, height: bool) -> Outcome {
    let i = match image(env, handle) {
        Ok(i) => i,
        Err(v) => return Outcome::Return(v as u32),
    };
    Outcome::Return(
        if out != 0
            && env
                .cpu
                .write_u32(out, if height { i.height } else { i.width })
                .is_ok()
        {
            0
        } else {
            2
        },
    )
}
#[win32(gdiplus,name="GdipSetStringFormatAlign",level=state)]
fn align(env: &mut Env<'_>, handle: GuestPtr<()>, align: i32) -> Outcome {
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::Align(align))))
}
#[win32(gdiplus,name="GdipSetStringFormatLineAlign",level=state)]
fn line_align(env: &mut Env<'_>, handle: GuestPtr<()>, align: i32) -> Outcome {
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::LineAlign(align))))
}
#[win32(gdiplus,name="GdipAddPathLineI",level=state)]
fn line_i(env: &mut Env<'_>, handle: GuestPtr<()>, x1: i32, y1: i32, x2: i32, y2: i32) -> Outcome {
    result(
        object(env, handle.addr())
            .and_then(|o| o.execute(Op::PathLine([x1 as f32, y1 as f32, x2 as f32, y2 as f32]))),
    )
}
#[win32(gdiplus,name="GdipAddPathLine",level=state)]
fn line(env: &mut Env<'_>, handle: GuestPtr<()>, x1: f32, y1: f32, x2: f32, y2: f32) -> Outcome {
    if [x1, y1, x2, y2].iter().any(|v| !v.is_finite()) {
        return Outcome::Return(2);
    }
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::PathLine([x1, y1, x2, y2]))))
}
#[win32(gdiplus,name="GdipAddPathArcI",level=state)]
fn arc_i(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    start: f32,
    sweep: f32,
) -> Outcome {
    arc(
        env,
        handle,
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        start,
        sweep,
    )
}
#[win32(gdiplus,name="GdipAddPathArc",level=state)]
fn arc(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    start: f32,
    sweep: f32,
) -> Outcome {
    if [x, y, width, height, start, sweep]
        .iter()
        .any(|v| !v.is_finite())
    {
        return Outcome::Return(2);
    }
    result(
        object(env, handle.addr())
            .and_then(|o| o.execute(Op::PathArc([x, y, width, height], start, sweep))),
    )
}
#[win32(gdiplus,name="GdipClosePathFigure",level=state)]
fn close_path(env: &mut Env<'_>, handle: GuestPtr<()>) -> Outcome {
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::ClosePath)))
}
#[win32(gdiplus,name="GdipAddPathString",level=state)]
fn path_string(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    text: WideStr,
    length: i32,
    family: GuestPtr<()>,
    style: i32,
    size: f32,
    rect: GuestPtr<()>,
    fmt: GuestPtr<()>,
) -> Outcome {
    let result_value = (|| {
        let o = object(env, handle.addr())?;
        let text = units(env, text.0.addr(), length)?;
        let family = object(env, family.addr())?;
        let r = rectangle(env, rect.addr())?;
        let fmt = format(env, fmt.addr())?;
        o.execute(Op::PathString {
            text: &text,
            family: &family,
            style,
            size,
            rect: r,
            format: fmt.as_deref(),
        })
    })();
    result(result_value)
}
#[win32(gdiplus,name="GdipGetPathWorldBounds",level=state)]
fn path_bounds(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    out: GuestPtr<()>,
    matrix: GuestPtr<()>,
    pen: GuestPtr<()>,
) -> Outcome {
    if !matrix.is_null() || !pen.is_null() {
        return fail(
            env,
            "gdiplus.dll!GdipGetPathWorldBounds",
            "explicit matrix and stroke bounds are unavailable",
        );
    }
    match object(env, handle.addr()).and_then(|o| o.execute(Op::Bounds)) {
        Ok(Reply::Bounds(r)) => Outcome::Return(if put_rect(env, out.addr(), r) { 0 } else { 2 }),
        Err(v) => Outcome::Return(v as u32),
        _ => Outcome::Return(2),
    }
}
#[win32(gdiplus,name="GdipGetPathGradientPointCount",level=state)]
fn gradient_points(env: &mut Env<'_>, handle: GuestPtr<()>, out: GuestPtr<i32>) -> Outcome {
    match object(env, handle.addr()).and_then(|o| {
        if o.kind != Kind::Gradient {
            return Err(2);
        }
        o.execute(Op::PointCount)
    }) {
        Ok(Reply::Count(n)) => {
            Outcome::Return(if !out.is_null() && out.write(env.cpu, n).is_ok() {
                0
            } else {
                2
            })
        }
        Err(v) => Outcome::Return(v as u32),
        _ => Outcome::Return(2),
    }
}
#[win32(gdiplus,name="GdipSetPathGradientCenterColor",level=state)]
fn center(env: &mut Env<'_>, handle: GuestPtr<()>, color: u32) -> Outcome {
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::CenterColor(color))))
}
#[win32(gdiplus,name="GdipSetPathGradientFocusScales",level=state)]
fn focus(env: &mut Env<'_>, handle: GuestPtr<()>, x: f32, y: f32) -> Outcome {
    if !x.is_finite() || !y.is_finite() {
        return Outcome::Return(2);
    }
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::Focus(x, y))))
}
#[win32(gdiplus,name="GdipSetPathGradientGammaCorrection",level=state)]
fn gamma(env: &mut Env<'_>, handle: GuestPtr<()>, enabled: i32) -> Outcome {
    result(object(env, handle.addr()).and_then(|o| o.execute(Op::Gamma(enabled != 0))))
}
#[win32(gdiplus,name="GdipSetPathGradientSurroundColorsWithCount",level=state)]
fn surround(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    colors: GuestPtr<u32>,
    count: GuestPtr<i32>,
) -> Outcome {
    let n = match count.read(env.cpu) {
        Ok(n) if (1..=65536).contains(&n) => n as usize,
        _ => return Outcome::Return(2),
    };
    let mut bytes = vec![0; n * 4];
    if env.cpu.read(colors.addr(), &mut bytes).is_err() {
        return Outcome::Return(2);
    }
    let colors: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    match object(env, handle.addr()).and_then(|o| o.execute(Op::Surround(&colors))) {
        Ok(Reply::Count(n)) => Outcome::Return(if count.write(env.cpu, n).is_ok() {
            0
        } else {
            2
        }),
        Err(v) => Outcome::Return(v as u32),
        _ => Outcome::Return(2),
    }
}
#[win32(gdiplus,name="GdipDrawImage",level=state)]
fn draw_image(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    handle: GuestPtr<()>,
    x: f32,
    y: f32,
) -> Outcome {
    let i = match image(env, handle.addr()) {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    if let Err(v) = refresh_image(env, &i) {
        return Outcome::Return(v as u32);
    }
    draw(
        env,
        graphics.addr(),
        Op::DrawImage(&i.object, [x, y, i.width as f32, i.height as f32]),
    )
}
#[win32(gdiplus,name="GdipDrawImageRectI",level=state)]
fn draw_rect_i(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    handle: GuestPtr<()>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Outcome {
    draw_rect(
        env,
        graphics,
        handle,
        x as f32,
        y as f32,
        width as f32,
        height as f32,
    )
}
#[win32(gdiplus,name="GdipDrawImageRect",level=state)]
fn draw_rect(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    handle: GuestPtr<()>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Outcome {
    if [x, y, width, height].iter().any(|x| !x.is_finite()) {
        return Outcome::Return(2);
    }
    let i = match image(env, handle.addr()) {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    if let Err(v) = refresh_image(env, &i) {
        return Outcome::Return(v as u32);
    }
    draw(
        env,
        graphics.addr(),
        Op::DrawImage(&i.object, [x, y, width, height]),
    )
}
#[win32(gdiplus,name="GdipFillPath",level=state)]
fn fill_path(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    brush: GuestPtr<()>,
    path: GuestPtr<()>,
) -> Outcome {
    let b = match object(env, brush.addr()) {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    let p = match object(env, path.addr()) {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    draw(env, graphics.addr(), Op::FillPath(&b, &p))
}
#[win32(gdiplus,name="GdipDrawString",level=state)]
fn draw_string(
    env: &mut Env<'_>,
    graphics: GuestPtr<()>,
    text: WideStr,
    length: i32,
    font: GuestPtr<()>,
    rect: GuestPtr<()>,
    fmt: GuestPtr<()>,
    brush: GuestPtr<()>,
) -> Outcome {
    let args: Result<_, i32> = (|| {
        Ok((
            units(env, text.0.addr(), length)?,
            object(env, font.addr())?,
            rectangle(env, rect.addr())?,
            format(env, fmt.addr())?,
            object(env, brush.addr())?,
        ))
    })();
    let (text, font, rect, fmt, brush) = match args {
        Ok(v) => v,
        Err(v) => return Outcome::Return(v as u32),
    };
    draw(
        env,
        graphics.addr(),
        Op::DrawString {
            text: &text,
            font: &font,
            rect,
            format: fmt.as_deref(),
            brush: &brush,
        },
    )
}
#[win32(gdiplus,name="GdipMeasureString",level=state)]
fn measure(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    text: WideStr,
    length: i32,
    font: GuestPtr<()>,
    rect: GuestPtr<()>,
    fmt: GuestPtr<()>,
    out: GuestPtr<()>,
    fitted: GuestPtr<i32>,
    lines: GuestPtr<i32>,
) -> Outcome {
    let value = (|| {
        let g = graphics(env, handle.addr())?;
        let text = units(env, text.0.addr(), length)?;
        let font = object(env, font.addr())?;
        let rect = rectangle(env, rect.addr())?;
        let fmt = format(env, fmt.addr())?;
        g.object.execute(Op::Measure {
            text: &text,
            font: &font,
            rect,
            format: fmt.as_deref(),
        })
    })();
    match value {
        Ok(Reply::Measure(r, n, l)) => Outcome::Return(
            if put_rect(env, out.addr(), r)
                && (fitted.is_null() || fitted.write(env.cpu, n).is_ok())
                && (lines.is_null() || lines.write(env.cpu, l).is_ok())
            {
                0
            } else {
                2
            },
        ),
        Err(v) => Outcome::Return(v as u32),
        _ => Outcome::Return(2),
    }
}
#[win32(gdiplus,name="GdipGraphicsClear",level=state)]
fn clear_graphics(env: &mut Env<'_>, graphics: GuestPtr<()>, color: u32) -> Outcome {
    draw(env, graphics.addr(), Op::Clear(color))
}
#[win32(gdiplus,name="GdipSetSmoothingMode",level=state)]
fn smoothing(env: &mut Env<'_>, graphics: GuestPtr<()>, value: i32) -> Outcome {
    result(object(env, graphics.addr()).and_then(|o| o.execute(Op::Smoothing(value))))
}
#[win32(gdiplus,name="GdipSetTextRenderingHint",level=state)]
fn text_hint(env: &mut Env<'_>, graphics: GuestPtr<()>, value: i32) -> Outcome {
    result(object(env, graphics.addr()).and_then(|o| o.execute(Op::TextHint(value))))
}

#[win32(gdiplus,name="GdipDeleteGraphics",level=state)]
fn delete_graphics(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Graphics)
}

#[win32(gdiplus,name="GdipDeleteFont",level=state)]
fn delete_font(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Font)
}

#[win32(gdiplus,name="GdipDeleteFontFamily",level=state)]
fn delete_fontfamily(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Family)
}

#[win32(gdiplus,name="GdipDeleteBrush",level=state)]
fn delete_brush(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Brush)
}

#[win32(gdiplus,name="GdipDeletePath",level=state)]
fn delete_path(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Path)
}

#[win32(gdiplus,name="GdipDeleteStringFormat",level=state)]
fn delete_stringformat(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Format)
}

#[win32(gdiplus,name="GdipDisposeImage",level=state)]
fn dispose(env: &mut Env<'_>, object: GuestPtr<()>) -> i32 {
    delete(env, object.addr(), Kind::Image)
}

pub(super) fn register(reg: &mut Registry) {
    for api in [
        startup_ABI,
        shutdown_ABI,
        alloc_ABI,
        free_ABI,
        from_hdc_ABI,
        from_hdc2_ABI,
        scan0_ABI,
        bitmap_stream_ABI,
        bitmap_icm_ABI,
        image_graphics_ABI,
        family_ABI,
        sans_ABI,
        font_ABI,
        solid_ABI,
        path_ABI,
        gradient_ABI,
        string_format_ABI,
        clone_image_ABI,
        clone_brush_ABI,
        width_ABI,
        height_ABI,
        align_ABI,
        line_align_ABI,
        line_i_ABI,
        line_ABI,
        arc_i_ABI,
        arc_ABI,
        close_path_ABI,
        path_string_ABI,
        path_bounds_ABI,
        gradient_points_ABI,
        center_ABI,
        focus_ABI,
        gamma_ABI,
        surround_ABI,
        draw_image_ABI,
        draw_rect_i_ABI,
        draw_rect_ABI,
        fill_path_ABI,
        draw_string_ABI,
        measure_ABI,
        clear_graphics_ABI,
        smoothing_ABI,
        text_hint_ABI,
        delete_graphics_ABI,
        delete_font_ABI,
        delete_fontfamily_ABI,
        delete_brush_ABI,
        delete_path_ABI,
        delete_stringformat_ABI,
        dispose_ABI,
    ] {
        reg.register(api);
    }
}
