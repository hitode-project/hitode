use adv_runtime::{DialogSet, MenuItem};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum Command {
    Key {
        key: u32,
        pressed: bool,
    },
    Click {
        x: i16,
        y: i16,
        right: bool,
    },
    Pointer {
        x: i16,
        y: i16,
        action: String,
        button: u32,
    },
    Menu(u32),
    Dialog {
        id: u32,
        dialog: Option<u32>,
    },
    Track {
        id: u32,
        dialog: Option<u32>,
        value: i32,
    },
    Select {
        id: u32,
        dialog: Option<u32>,
        index: i32,
    },
    Text {
        id: u32,
        dialog: Option<u32>,
        text: String,
    },
    DialogClose,
    Pause,
    Resume,
    AudioFocus(bool),
}
fn number(value: &Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{field} must be an integer"))
}
fn id(value: &Value, field: &str) -> Result<u32, String> {
    u32::try_from(number(value, field)?).map_err(|_| format!("invalid {field}"))
}
fn flag(value: &Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{field} must be boolean"))
}
pub fn parse(text: &str) -> Result<Command, String> {
    if text.len() > 4096 {
        return Err("command exceeds 4096 bytes".into());
    }
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let dialog = if value.get("dialog").is_some() {
        Some(id(&value, "dialog")?)
    } else {
        None
    };
    let coordinate =
        |field| i16::try_from(number(&value, field)?).map_err(|_| format!("invalid {field}"));
    match value.get("type").and_then(Value::as_str) {
        Some("key") => {
            let key = id(&value, "key")?;
            if key > 255 {
                return Err("key must be in 0..255".into());
            }
            Ok(Command::Key {
                key,
                pressed: flag(&value, "pressed")?,
            })
        }
        Some(kind @ ("click" | "right-click")) => Ok(Command::Click {
            x: coordinate("x")?,
            y: coordinate("y")?,
            right: kind == "right-click",
        }),
        Some("pointer") => {
            let action = value
                .get("action")
                .and_then(Value::as_str)
                .ok_or("pointer requires action")?;
            if !matches!(action, "move" | "down" | "up") {
                return Err("invalid pointer action".into());
            }
            let button = id(&value, "button")?;
            if button > 1 {
                return Err("pointer button must be 0 or 1".into());
            }
            Ok(Command::Pointer {
                x: coordinate("x")?,
                y: coordinate("y")?,
                action: action.into(),
                button,
            })
        }
        Some("menu") => Ok(Command::Menu(id(&value, "id")?)),
        Some("dialog") => Ok(Command::Dialog {
            id: id(&value, "id")?,
            dialog,
        }),
        Some("dialog-track") => Ok(Command::Track {
            id: id(&value, "id")?,
            dialog,
            value: i32::try_from(number(&value, "value")?).map_err(|_| "invalid track value")?,
        }),
        Some("dialog-select") => Ok(Command::Select {
            id: id(&value, "id")?,
            dialog,
            index: i32::try_from(number(&value, "value")?)
                .map_err(|_| "invalid selection index")?,
        }),
        Some("dialog-text") => {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .ok_or("dialog-text requires text")?;
            if text.len() > 2048 || text.contains('\0') {
                return Err("invalid dialog text".into());
            }
            Ok(Command::Text {
                id: id(&value, "id")?,
                dialog,
                text: text.into(),
            })
        }
        Some("dialog-close") => Ok(Command::DialogClose),
        Some("pause") => Ok(Command::Pause),
        Some("resume") => Ok(Command::Resume),
        Some("audio_focus") => Ok(Command::AudioFocus(flag(&value, "active")?)),
        _ => Err("unknown Android command type".into()),
    }
}
pub fn menus(items: &[MenuItem]) -> Value {
    Value::Array(items.iter().map(|item| json!({
        "id": item.command.id, "label": String::from_utf16_lossy(&item.text),
        "enabled": item.supported && item.flags & 3 == 0, "checked": item.flags & 8 != 0,
        "children": menus(&item.children)
    })).collect())
}
pub fn dialogs(set: &DialogSet) -> Value {
    Value::Array(set.dialogs.iter().map(|dialog| json!({
        "id": dialog.target.dialog, "title": dialog.text, "text": dialog.text,
        "visible": dialog.visible, "enabled": dialog.enabled, "modal": dialog.modal,
        "controls": dialog.controls.iter().map(|control| json!({
            "id": control.target.id, "class": control.class, "text": control.text,
            "visible": control.visible, "enabled": control.enabled, "check": control.check,
            "checked": control.check != 0, "supported": control.supported, "style": control.style,
            "range": control.range.map(|(min,max)| json!({"min":min,"max":max})),
            "value": control.value, "items": control.items, "selection": control.selection,
            "rect": {"left":control.rect.left,"top":control.rect.top,
                      "right":control.rect.right,"bottom":control.rect.bottom}
        })).collect::<Vec<_>>()
    })).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: HOST-AND-4
    #[test]
    fn malformed_input_cannot_be_truncated_into_another_guest_command() {
        for text in [
            r#"{"type":"key","key":256,"pressed":true}"#,
            r#"{"type":"pointer","action":"down","button":4,"x":1,"y":2}"#,
            r#"{"type":"click","x":40000,"y":2}"#,
            r#"{"type":"audio_focus","active":1}"#,
        ] {
            assert!(parse(text).is_err());
        }
        assert!(matches!(
            parse(r#"{"type":"key","key":17,"pressed":false}"#),
            Ok(Command::Key {
                key: 17,
                pressed: false
            })
        ));
    }
}
