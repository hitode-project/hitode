//! `--text-log`: the session's text records (WINE-UNIX-8) as JSON lines, for
//! checking text extraction locally. The file holds game text, so it stays local.
use adv_runtime::wine::text_records::{Code, TextBatch, TextRecord};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

pub(crate) struct TextLog {
    out: std::io::LineWriter<std::fs::File>,
}

impl TextLog {
    pub(crate) fn open(path: Option<&Path>) -> Result<Option<Self>, String> {
        let Some(path) = path else { return Ok(None) };
        let file = std::fs::File::create(path)
            .map_err(|error| format!("text log {}: {error}", path.display()))?;
        Ok(Some(Self {
            out: std::io::LineWriter::new(file),
        }))
    }

    /// Append `batch`; a failed write ends the log, never the game.
    pub(crate) fn write(log: &mut Option<Self>, batch: TextBatch) {
        let Some(this) = log else { return };
        let mut lines = String::new();
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
                 \"surface\": {}, \"face\": {}, \"height\": {}, \"clip\": {}, \
                 \"text\": {}, \"glyphs\": [{glyphs}]}}",
                line.sequence,
                line.thread,
                line.dc,
                line.surface.map_or("null".into(), |s| s.to_string()),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_runtime::wine::text_records::{DrawnGlyph, DrawnLine, ProducedGlyph};

    // spec: WINE-UNIX-8
    #[test]
    fn records_become_one_json_line_each_after_any_drop_count() {
        let path = std::env::temp_dir().join(format!("adv-text-log-{}", std::process::id()));
        let mut log = TextLog::open(Some(&path)).unwrap();
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
                        surface: Some(32),
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
                ],
                dropped: 3,
            },
        );
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines,
            [
                r#"{"kind": "dropped", "count": 3}"#,
                "{\"sequence\": 4, \"kind\": \"line\", \"thread\": 9, \"dc\": 16, \"surface\": 32, \
                 \"face\": \"Face \\\"A\\\"\", \"height\": -16, \"clip\": null, \"text\": \"\u{3042}\", \
                 \"glyphs\": [{\"code\": {\"unicode\": 12354}, \"x\": 2, \"y\": 7}, \
                 {\"code\": {\"index\": 5}, \"x\": 18, \"y\": 7}]}",
                r#"{"sequence": 5, "kind": "glyph", "thread": 9, "dc": 16, "face": "Face", "height": -16, "code": {"encoded": 61505}, "buffer": 4096, "len": 64}"#,
            ]
        );
    }
}
