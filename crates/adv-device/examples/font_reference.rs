//! Deterministic portable CJK references. Run with <profile.json> <output-dir>.
//! The caller supplies licensed font files; this example contains no font data.

fn main() {
    if let Err(error) = reference::run() {
        eprintln!("font reference failed: {error}");
        std::process::exit(1);
    }
}

mod reference {
    use adv_device::{
        surface::{PixelFormat, Rect},
        text::*,
    };
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    struct Run {
        role: &'static str,
        content: String,
        font: FontRequest,
        origin: Point,
    }
    impl Run {
        fn new(role: &'static str, content: &str, font: FontRequest, x: i32, y: i32) -> Self {
            Self {
                role,
                content: content.to_owned(),
                font,
                origin: Point { x, y },
            }
        }
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 2 {
            return Err("usage: font_reference <profile.json> <output-dir>".into());
        }
        let profile = PathBuf::from(&args[0]);
        let output = PathBuf::from(&args[1]);
        let mut text = Text::with_font_profile(&profile).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        let chinese = font("黑体", 22, Encoding::SimplifiedChinese);
        let japanese = font("ＭＳ 明朝", 32, Encoding::Japanese);
        let body = vec![
            Run::new(
                "chinese-body",
                "夜色渐深，桥上的灯光映在水面。",
                chinese,
                24,
                24,
            ),
            Run::new(
                "japanese-body",
                "風の音が静かな夜空に響く。",
                japanese,
                24,
                80,
            ),
        ];
        let wrap_font = font("黑体", 26, Encoding::SimplifiedChinese);
        let wrap = wrapped_runs(
            &mut text,
            &wrap_font,
            "窗外的风轻轻吹过，桥上的灯光映在水面。我们沿着小路向前走，夜空里有星星。",
            300,
        )?;
        if wrap.len() < 2 {
            return Err("linewrap reference did not produce multiple lines".into());
        }
        let base = font("ＭＳ ゴシック", 48, Encoding::Japanese);
        let ruby_font = font("ＭＳ ゴシック", 20, Encoding::Japanese);
        let base_extent = extent(&mut text, &base, "夜空")?;
        let ruby_extent = extent(&mut text, &ruby_font, "よぞら")?;
        let ruby = vec![
            Run::new(
                "ruby",
                "よぞら",
                ruby_font,
                48 + (base_extent.width - ruby_extent.width) / 2,
                24,
            ),
            Run::new("ruby-base", "夜空", base.clone(), 48, 64),
            Run::new(
                "ruby-continuation",
                "に星が光る。",
                base,
                48 + base_extent.width,
                64,
            ),
        ];
        let choice_font = font("黑体", 26, Encoding::SimplifiedChinese);
        let choices = vec![
            Run::new("choice-1", "一起走过这座桥", choice_font.clone(), 48, 48),
            Run::new("choice-2", "留在这里等一会儿", choice_font, 48, 120),
        ];
        let cases = [
            render(&mut text, &output, "cjk-body", 800, 160, &body, false)?,
            render(&mut text, &output, "linewrap", 360, 240, &wrap, false)?,
            render(&mut text, &output, "ruby", 600, 160, &ruby, false)?,
            render(&mut text, &output, "choices", 600, 200, &choices, true)?,
        ];
        let profile_bytes = std::fs::read(&profile).map_err(|error| error.to_string())?;
        let report = json!({
            "version": 1,
            "purpose": "same-font portable reference; independent authored text, not a game checkpoint or Windows GDI pixel equivalence",
            "profile_sha256": hash(&profile_bytes),
            "backend": "portable/skrifa+zeno",
            "image_format": "BMP24 bottom-up; raster requests use Gray8, hinted, identity MAT2",
            "cases": cases,
        });
        let path = output.join("font-reference.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        println!("{}", path.display());
        Ok(())
    }

    fn font(face: &str, height: i32, encoding: Encoding) -> FontRequest {
        FontRequest {
            default: None,
            face: face.into(),
            height,
            width: 0,
            baseline_angle: 0,
            glyph_angle: 0,
            weight: 400,
            italic: false,
            underline: false,
            strikeout: false,
            encoding,
            precision: Precision::Default,
            clipping: Clipping {
                precision: ClipPrecision::Default,
                transform_rotation: false,
                force_outline: false,
                disable_association: false,
                embedded: false,
            },
            quality: Quality::Antialiased,
            pitch: Pitch::Default,
            family: Family::Default,
        }
    }
    fn extent(text: &mut Text, font: &FontRequest, content: &str) -> Result<TextExtent, String> {
        let units: Vec<_> = content.encode_utf16().collect();
        text.extent(font, TextInput::Unicode(&units))
            .map_err(|error| error.to_string())
    }

    // The wrap policy is explicit and independent of game-side newline logic:
    // greedy Unicode-scalar wrapping at a fixed measured pixel width.
    fn wrapped_runs(
        text: &mut Text,
        font: &FontRequest,
        content: &str,
        width: i32,
    ) -> Result<Vec<Run>, String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for character in content.chars() {
            let mut candidate = line.clone();
            candidate.push(character);
            if extent(text, font, &candidate)?.width > width {
                if line.is_empty() {
                    return Err("linewrap reference has a glyph wider than the line".into());
                }
                lines.push(std::mem::take(&mut line));
            }
            line.push(character);
        }
        if !line.is_empty() {
            lines.push(line);
        }
        Ok(lines
            .iter()
            .enumerate()
            .map(|(index, content)| {
                Run::new(
                    "wrapped-line",
                    content,
                    font.clone(),
                    24,
                    24 + index as i32 * 38,
                )
            })
            .collect())
    }

    fn render(
        text: &mut Text,
        output: &Path,
        name: &str,
        width: u32,
        height: u32,
        runs: &[Run],
        choices: bool,
    ) -> Result<Value, String> {
        let mut pixels = vec![255; (width * height * 4) as usize];
        if choices {
            for y in 32..168u32 {
                if (88..104).contains(&y) {
                    continue;
                }
                for x in 24..576u32 {
                    let color = if x == 24 || x == 575 || matches!(y, 32 | 87 | 104 | 167) {
                        160
                    } else {
                        238
                    };
                    let offset = ((y * width + x) * 4) as usize;
                    pixels[offset..offset + 3].fill(color);
                }
            }
        }
        let mut run_reports = Vec::new();
        for run in runs {
            let units: Vec<_> = run.content.encode_utf16().collect();
            let metrics = text.metrics(&run.font).map_err(|error| error.to_string())?;
            let extent = text
                .extent(&run.font, TextInput::Unicode(&units))
                .map_err(|error| error.to_string())?;
            let baseline = run.origin.y + metrics.ascent;
            let mut cursor = run.origin.x;
            let mut glyph_reports = Vec::new();
            for character in run.content.chars() {
                let code = u32::from(character);
                let resolved = text
                    .resolved_character(&run.font, code)
                    .map_err(|error| error.to_string())?;
                if resolved.default_glyph {
                    return Err(format!("{name}/{} lacks U+{code:04X} coverage", run.role));
                }
                let glyph = text
                    .glyph(
                        &run.font,
                        GlyphRequest {
                            character: Character::Unicode(code),
                            format: GlyphFormat::Gray8,
                            hinted: true,
                            transform: Transform::IDENTITY,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                let GlyphData::Bitmap(bitmap) = &glyph.data else {
                    return Err("reference glyph is not a bitmap".into());
                };
                let gm = glyph.metrics;
                let left = cursor + gm.origin_x;
                let top = baseline - gm.origin_y;
                let right = left + gm.width as i32;
                let bottom = top + gm.height as i32;
                if left < 0 || top < 0 || right > width as i32 || bottom > height as i32 {
                    return Err(format!(
                        "{name}/{} glyph U+{code:04X} would be clipped",
                        run.role
                    ));
                }
                glyph_reports.push(json!({
                    "code": code, "character": character.to_string(), "bitmap_sha256": hash(bitmap),
                    "required_bytes": glyph.required_bytes,
                    "glyph": {"index": resolved.glyph_index, "width": gm.width, "height": gm.height,
                        "origin_x": gm.origin_x, "origin_y": gm.origin_y, "advance_x": gm.advance_x, "advance_y": gm.advance_y},
                    "paint_bounds": [left, top, right, bottom],
                    "resolved": {"family": resolved.family, "source": resolved.source, "face_index": resolved.face_index,
                        "sha256": resolved.sha256, "font_bytes": resolved.font_bytes,
                        "em_pixels": resolved.em_pixels, "cell_pixels": resolved.cell_pixels,
                        "horizontal_scale": resolved.horizontal_scale, "profile_sha256": resolved.profile_sha256,
                        "backend": resolved.backend, "default_glyph": resolved.default_glyph},
                }));
                cursor += i32::from(gm.advance_x);
            }
            if cursor - run.origin.x != extent.width {
                return Err("glyph advances disagree with run extent".into());
            }
            text.paint(
                &run.font,
                Paint {
                    input: TextInput::Unicode(&units),
                    origin: run.origin,
                    horizontal: Horizontal::Left,
                    vertical: Vertical::Top,
                    foreground: [24; 3],
                    background: [255; 3],
                    opaque_background: false,
                    clip: None,
                    fill: None,
                },
                Pixels {
                    bounds: Rect {
                        x: 0,
                        y: 0,
                        w: width,
                        h: height,
                    },
                    pitch: width * 4,
                    format: PixelFormat::Bgra32,
                    bytes: &mut pixels,
                },
            )
            .map_err(|error| error.to_string())?;
            run_reports.push(json!({
                "role": run.role, "text": run.content, "origin": [run.origin.x, run.origin.y],
                "request": {"face": run.font.face, "height": run.font.height, "width": run.font.width,
                    "weight": run.font.weight, "italic": run.font.italic, "encoding": format!("{:?}", run.font.encoding),
                    "quality": "Antialiased", "pitch": "Default", "family": "Default", "hinted": true,
                    "format": "Gray8", "transform": [65536, 0, 0, 65536]},
                "extent": {"width": extent.width, "height": extent.height},
                "metrics": {"height": metrics.height, "ascent": metrics.ascent, "descent": metrics.descent,
                    "internal_leading": metrics.internal_leading, "external_leading": metrics.external_leading},
                "baseline": baseline, "glyphs": glyph_reports,
            }));
        }
        let bmp = bmp(width, height, &pixels);
        let image = format!("{name}.bmp");
        std::fs::write(output.join(&image), &bmp).map_err(|error| error.to_string())?;
        Ok(
            json!({"name": name, "image": image, "image_sha256": hash(&bmp),
            "pixels_sha256": hash(&pixels), "width": width, "height": height, "runs": run_reports}),
        )
    }

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn bmp(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let pitch = (width * 3 + 3) & !3;
        let image_bytes = pitch * height;
        let mut bytes = Vec::with_capacity((54 + image_bytes) as usize);
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(54 + image_bytes).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&(width as i32).to_le_bytes());
        bytes.extend_from_slice(&(height as i32).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&24u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&image_bytes.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        for y in (0..height).rev() {
            for x in 0..width {
                let offset = ((y * width + x) * 4) as usize;
                bytes.extend_from_slice(&pixels[offset..offset + 3]);
            }
            bytes.resize(bytes.len() + (pitch - width * 3) as usize, 0);
        }
        bytes
    }
}
