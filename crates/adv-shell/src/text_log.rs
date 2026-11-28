//! `--text-log`: the session's text records (WINE-UNIX-8) as JSON lines, for
//! checking text extraction locally. The file holds game text, so it stays local.
use adv_runtime::wine::session::WindowPlane;
use adv_runtime::wine::text_records::{Code, CopyKind, Plane, TextBatch, TextRecord};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

pub(crate) struct TextLog {
    out: std::io::LineWriter<std::fs::File>,
    /// The window planes last written, so only changes are written.
    windows: Vec<WindowPlane>,
}

impl TextLog {
    pub(crate) fn open(path: Option<&Path>) -> Result<Option<Self>, String> {
        let Some(path) = path else { return Ok(None) };
        let file = std::fs::File::create(path)
            .map_err(|error| format!("text log {}: {error}", path.display()))?;
        Ok(Some(Self {
            out: std::io::LineWriter::new(file),
            windows: Vec::new(),
        }))
    }

    /// Append `batch`, after `windows` when they changed; a failed write
    /// ends the log, never the game.
    pub(crate) fn write(log: &mut Option<Self>, batch: TextBatch, windows: Vec<WindowPlane>) {
        let Some(this) = log else { return };
        let mut lines = String::new();
        if windows != this.windows {
            let planes = windows
                .iter()
                .map(|w| {
                    format!(
                        "{{\"hwnd\": {}, \"bits\": {}, \"stride\": {}, \"size\": {:?}, \"client\": {}}}",
                        w.hwnd,
                        w.bits,
                        w.stride,
                        w.size,
                        w.client.map_or("null".into(), |c| format!("{c:?}"))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(lines, "{{\"kind\": \"windows\", \"planes\": [{planes}]}}");
            this.windows = windows;
        }
        if batch.dropped != 0 {
            let _ = writeln!(
                lines,
                "{{\"kind\": \"dropped\", \"count\": {}}}",
                batch.dropped
            );
        }
        for record in &batch.records {
            lines.push_str(&line(record));
            lines.push('\n');
        }
        if let Err(error) = this.out.write_all(lines.as_bytes()) {
            eprintln!("text log: {error}; logging stopped");
            *log = None;
        }
    }
}

fn code(code: Code) -> String {
    match code {
        Code::Unicode(c) => format!("{{\"unicode\": {c}}}"),
        Code::Encoded(c) => format!("{{\"encoded\": {c}}}"),
        Code::Index(i) => format!("{{\"index\": {i}}}"),
    }
}

fn plane(plane: Option<Plane>) -> String {
    plane.map_or("null".into(), |p| {
        format!(
            "{{\"bits\": {}, \"stride\": {}, \"bpp\": {}, \"origin\": {:?}, \"size\": {:?}}}",
            p.bits, p.stride, p.bpp, p.origin, p.size
        )
    })
}

fn line(record: &TextRecord) -> String {
    match record {
        TextRecord::Line(line) => {
            let text: String = line
                .glyphs
                .iter()
                .filter_map(|glyph| match glyph.code {
                    Code::Unicode(c) => Some(char::from_u32(c).unwrap_or('\u{fffd}')),
                    _ => None,
                })
                .collect();
            let glyphs = line
                .glyphs
                .iter()
                .map(|glyph| {
                    format!(
                        "{{\"code\": {}, \"x\": {}, \"y\": {}}}",
                        code(glyph.code),
                        glyph.origin[0],
                        glyph.origin[1]
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{{\"sequence\": {}, \"kind\": \"line\", \"thread\": {}, \"dc\": {}, \
                 \"plane\": {}, \"face\": {}, \"height\": {}, \"clip\": {}, \
                 \"text\": {}, \"glyphs\": [{glyphs}]}}",
                line.sequence,
                line.thread,
                line.dc,
                plane(line.plane),
                super::json(&line.face),
                line.height,
                line.clip.map_or("null".into(), |c| format!("{c:?}")),
                super::json(&text),
            )
        }
        TextRecord::Glyph(glyph) => format!(
            "{{\"sequence\": {}, \"kind\": \"glyph\", \"thread\": {}, \"dc\": {}, \
             \"face\": {}, \"height\": {}, \"code\": {}, \"buffer\": {}, \"len\": {}}}",
            glyph.sequence,
            glyph.thread,
            glyph.dc,
            super::json(&glyph.face),
            glyph.height,
            code(glyph.code),
            glyph.buffer,
            glyph.len
        ),
        TextRecord::Copy(copy) => format!(
            "{{\"sequence\": {}, \"kind\": \"copy\", \"thread\": {}, \"op\": \"{}\", \
             \"destination\": {}, \"rect\": {:?}, \"source\": {}, \"offset\": {:?}}}",
            copy.sequence,
            copy.thread,
            match copy.kind {
                CopyKind::Copy => "copy",
                CopyKind::Blend => "blend",
                CopyKind::Convert => "convert",
            },
            plane(Some(copy.destination)),
            copy.rect,
            plane(Some(copy.source)),
            copy.offset
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_runtime::wine::text_records::{CopiedPixels, DrawnGlyph, DrawnLine, ProducedGlyph};

    // spec: WINE-UNIX-8
    #[test]
    fn records_become_one_json_line_each_after_any_drop_count() {
        let path = std::env::temp_dir().join(format!("adv-text-log-{}", std::process::id()));
        let mut log = TextLog::open(Some(&path)).unwrap();
        let surface = Plane {
            bits: 32,
            stride: -64,
            bpp: 32,
            origin: [1, 2],
            size: [16, 8],
        };
        let glyph = |code, x| DrawnGlyph {
            code,
            origin: [x, 7],
        };
        TextLog::write(
            &mut log,
            TextBatch {
                records: vec![
                    TextRecord::Line(DrawnLine {
                        sequence: 4,
                        thread: 9,
                        dc: 16,
                        plane: Some(surface),
                        face: "Face \"A\"".into(),
                        height: -16,
                        clip: None,
                        glyphs: vec![glyph(Code::Unicode(0x3042), 2), glyph(Code::Index(5), 18)],
                    }),
                    TextRecord::Glyph(ProducedGlyph {
                        sequence: 5,
                        thread: 9,
                        dc: 16,
                        face: "Face".into(),
                        height: -16,
                        code: Code::Encoded(0xf041),
                        buffer: 4096,
                        len: 64,
                    }),
                    TextRecord::Copy(CopiedPixels {
                        sequence: 6,
                        thread: 9,
                        kind: CopyKind::Blend,
                        destination: Plane {
                            bits: 64,
                            ..surface
                        },
                        rect: [0, 1, 2, 3],
                        source: surface,
                        offset: [-4, 5],
                    }),
                ],
                dropped: 3,
            },
            vec![WindowPlane {
                hwnd: 0x10020,
                bits: 64,
                stride: 64,
                size: [16, 8],
                client: Some([0, 0, 16, 8]),
            }],
        );
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines,
            [
                r#"{"kind": "windows", "planes": [{"hwnd": 65568, "bits": 64, "stride": 64, "size": [16, 8], "client": [0, 0, 16, 8]}]}"#,
                r#"{"kind": "dropped", "count": 3}"#,
                "{\"sequence\": 4, \"kind\": \"line\", \"thread\": 9, \"dc\": 16, \
                 \"plane\": {\"bits\": 32, \"stride\": -64, \"bpp\": 32, \"origin\": [1, 2], \"size\": [16, 8]}, \
                 \"face\": \"Face \\\"A\\\"\", \"height\": -16, \"clip\": null, \"text\": \"\u{3042}\", \
                 \"glyphs\": [{\"code\": {\"unicode\": 12354}, \"x\": 2, \"y\": 7}, \
                 {\"code\": {\"index\": 5}, \"x\": 18, \"y\": 7}]}",
                r#"{"sequence": 5, "kind": "glyph", "thread": 9, "dc": 16, "face": "Face", "height": -16, "code": {"encoded": 61505}, "buffer": 4096, "len": 64}"#,
                concat!(
                    r#"{"sequence": 6, "kind": "copy", "thread": 9, "op": "blend", "#,
                    r#""destination": {"bits": 64, "stride": -64, "bpp": 32, "origin": [1, 2], "size": [16, 8]}, "#,
                    r#""rect": [0, 1, 2, 3], "#,
                    r#""source": {"bits": 32, "stride": -64, "bpp": 32, "origin": [1, 2], "size": [16, 8]}, "#,
                    r#""offset": [-4, 5]}"#
                ),
            ]
        );
    }
}
