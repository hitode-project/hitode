//! MCI command strings for the digital-video subset. Each string becomes the
//! message and parameter block that mciSendCommandW takes, so string and
//! command front ends share one device lifecycle. Aliases live here, as they
//! do in winmm's own string parser. Other verbs and keywords fail with the
//! MCI parser errors.
use super::*;

const UNRECOGNIZED_KEYWORD: u32 = 259;
const UNRECOGNIZED_COMMAND: u32 = 261;
const INVALID_DEVICE_NAME: u32 = 263;
const OUT_OF_MEMORY: u32 = 264;
const MISSING_COMMAND_STRING: u32 = 267;
const PARAM_OVERFLOW: u32 = 268;
const BAD_INTEGER: u32 = 270;
const UNSUPPORTED_FUNCTION: u32 = 274;
const DEVICE_TYPE_REQUIRED: u32 = 287;
const DUPLICATE_ALIAS: u32 = 289;
const MISSING_DEVICE_NAME: u32 = 292;
const NO_CLOSING_QUOTE: u32 = 294;

/// Open device names: the alias, or the element when no alias was given.
#[derive(Default)]
struct Aliases(Vec<(String, u32)>);

#[derive(Debug, PartialEq)]
enum Field {
    Word(u32),
    Text(String),
}

#[derive(Debug, PartialEq)]
enum Reply {
    Empty,
    Opened(String),
    Closed,
    Mode,
    Number,
    Rect,
}

/// One command string as mciSendCommandW arguments. `fields` follow the
/// callback dword that starts every parameter block.
#[derive(Debug, PartialEq)]
struct Plan {
    device: Option<String>,
    message: u32,
    flags: u32,
    fields: Vec<Field>,
    reply: Reply,
}

fn tokens(text: &str) -> Result<Vec<String>, u32> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(first) = chars.next() else {
            return Ok(out);
        };
        let mut token = String::new();
        if first == '"' {
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => token.push(c),
                    None => return Err(NO_CLOSING_QUOTE),
                }
            }
        } else {
            token.push(first);
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                token.push(c);
            }
        }
        out.push(token);
    }
}

fn number(token: Option<&String>) -> Result<u32, u32> {
    token.and_then(|t| t.parse().ok()).ok_or(BAD_INTEGER)
}

fn plan(text: &str) -> Result<Plan, u32> {
    let tokens = tokens(text)?;
    let mut words = tokens.iter();
    let verb = words
        .next()
        .ok_or(MISSING_COMMAND_STRING)?
        .to_ascii_lowercase();
    if !matches!(
        verb.as_str(),
        "open"
            | "close"
            | "play"
            | "stop"
            | "pause"
            | "resume"
            | "window"
            | "put"
            | "where"
            | "status"
            | "set"
            | "setaudio"
    ) {
        return Err(UNRECOGNIZED_COMMAND);
    }
    let name = words.next().ok_or(MISSING_DEVICE_NAME)?.clone();
    let mut plan = Plan {
        device: Some(name.to_ascii_lowercase()),
        message: 0,
        flags: 0,
        fields: Vec::new(),
        reply: Reply::Empty,
    };
    let (mut kind, mut alias) = (None, None);
    let (mut from, mut to, mut item) = (None, None, None);
    let mut rect = None;
    let (mut handle, mut destination) = (None, false);
    let (mut side, mut level) = (0, None);
    while let Some(word) = words.next() {
        let word = word.to_ascii_lowercase();
        match (verb.as_str(), word.as_str()) {
            (_, "wait") => plan.flags |= 2,
            (_, "notify") => plan.flags |= 1,
            ("open", "type") => kind = Some(words.next().ok_or(DEVICE_TYPE_REQUIRED)?.clone()),
            ("open", "alias") => alias = Some(words.next().ok_or(MISSING_DEVICE_NAME)?.clone()),
            ("play", "from") => from = Some(number(words.next())?),
            ("play", "to") => to = Some(number(words.next())?),
            ("window", "handle") => handle = Some(number(words.next())?),
            ("put", "destination") | ("where", "destination") => destination = true,
            ("put", "at") => {
                let mut values = [0; 4];
                for value in &mut values {
                    *value = number(words.next())?;
                }
                rect = Some(values);
            }
            ("setaudio", "left") => side = 0x200000,
            ("setaudio", "right") => side = 0x400000,
            ("setaudio", "volume") => item = Some((0x4002, Reply::Empty)),
            ("setaudio", "to") => level = Some(number(words.next())?),
            ("status", "mode") => item = Some((4, Reply::Mode)),
            ("status", "length") => item = Some((1, Reply::Number)),
            ("status", "position") => item = Some((2, Reply::Number)),
            ("set", "time") => {
                let format = words.next().map(|w| w.to_ascii_lowercase());
                let unit = words.next().map(|w| w.to_ascii_lowercase());
                if format.as_deref() != Some("format")
                    || !matches!(unit.as_deref(), Some("milliseconds" | "ms"))
                {
                    return Err(UNSUPPORTED_FUNCTION);
                }
                item = Some((0, Reply::Empty));
            }
            _ => return Err(UNRECOGNIZED_KEYWORD),
        }
    }
    match verb.as_str() {
        "open" => {
            plan.device = None;
            plan.message = 0x803;
            plan.flags |= 0x2000 | 0x200;
            plan.fields = vec![
                Field::Word(0),
                Field::Text(kind.ok_or(DEVICE_TYPE_REQUIRED)?),
                Field::Text(name.clone()),
                Field::Word(0),
            ];
            plan.reply = Reply::Opened(alias.unwrap_or(name).to_ascii_lowercase());
        }
        "close" => {
            plan.message = 0x804;
            plan.reply = Reply::Closed;
        }
        "play" => {
            plan.message = 0x806;
            plan.flags |= (u32::from(from.is_some()) * 4) | (u32::from(to.is_some()) * 8);
            plan.fields = vec![Field::Word(from.unwrap_or(0)), Field::Word(to.unwrap_or(0))];
        }
        "stop" => plan.message = 0x808,
        "pause" => plan.message = 0x809,
        "resume" => plan.message = 0x855,
        "window" => {
            plan.message = 0x841;
            plan.flags |= 0x10000;
            let handle = handle.ok_or(UNSUPPORTED_FUNCTION)?;
            plan.fields = vec![Field::Word(handle), Field::Word(0), Field::Word(0)];
        }
        "put" | "where" if destination => {
            plan.message = if verb == "put" { 0x842 } else { 0x843 };
            plan.flags |= 0x40000;
            if verb == "where" {
                plan.reply = Reply::Rect;
            } else if rect.is_none() {
                return Err(UNSUPPORTED_FUNCTION);
            } else {
                plan.flags |= 0x10000;
            }
            plan.fields = rect.unwrap_or_default().map(Field::Word).into();
        }
        "status" => {
            let (item, reply) = item.ok_or(UNSUPPORTED_FUNCTION)?;
            plan.message = 0x814;
            plan.flags |= 0x100;
            plan.fields = vec![Field::Word(0), Field::Word(item), Field::Word(0)];
            plan.reply = reply;
        }
        "setaudio" => {
            let (Some((item, _)), Some(level)) = (item, level) else {
                return Err(UNSUPPORTED_FUNCTION);
            };
            plan.message = 0x873;
            plan.flags |= 0x800000 | 0x1000000 | side;
            plan.fields = vec![
                Field::Word(item),
                Field::Word(level),
                Field::Word(0),
                Field::Word(0),
                Field::Word(0),
            ];
        }
        "set" if item.is_some() => {
            plan.message = 0x80d;
            plan.flags |= 0x400;
            plan.fields = vec![Field::Word(0), Field::Word(0)];
        }
        _ => return Err(UNRECOGNIZED_COMMAND),
    }
    Ok(plan)
}

fn mode(value: u32) -> &'static str {
    match value {
        525 => "stopped",
        526 => "playing",
        527 => "recording",
        528 => "seeking",
        529 => "paused",
        530 => "open",
        _ => "not ready",
    }
}

fn read_command(env: &Env<'_>, command: u32, wide: bool) -> String {
    if wide {
        String::from_utf16_lossy(&crate::util::wstr(env.cpu, command))
    } else {
        env.ext
            .get::<crate::Win32>()
            .codec
            .decode(&crate::util::cstr(env.cpu, command))
    }
}

fn reply(env: &mut Env<'_>, text: &str, output: u32, capacity: u32, wide: bool) -> u32 {
    if output == 0 || capacity == 0 {
        return 0;
    }
    let mut bytes = if wide {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else {
        env.ext.get::<crate::Win32>().codec.encode(text)
    };
    let unit = if wide { 2 } else { 1 };
    if bytes.len() / unit >= capacity as usize {
        return PARAM_OVERFLOW;
    }
    bytes.extend(std::iter::repeat_n(0, unit));
    if env.cpu.write(output, &bytes).is_err() {
        return MISSING_PARAMETER;
    }
    0
}

pub(super) fn send(
    env: &mut Env<'_>,
    command: u32,
    output: u32,
    capacity: u32,
    callback: u32,
    wide: bool,
) -> Outcome {
    let api = if wide {
        "mciSendStringW"
    } else {
        "mciSendStringA"
    };
    if command == 0 {
        return Outcome::Return(MISSING_COMMAND_STRING);
    }
    let text = read_command(env, command, wide);
    let plan = match plan(&text) {
        Ok(plan) => plan,
        Err(error) => {
            if matches!(error, UNRECOGNIZED_COMMAND | UNSUPPORTED_FUNCTION) {
                adv_abi::warn_unavailable(env, api, "only digital-video commands are parsed");
            }
            return Outcome::Return(error);
        }
    };
    let aliases = &env.ext.get_or_default::<Aliases>().0;
    let device = match &plan.device {
        None => {
            if let Reply::Opened(name) = &plan.reply
                && aliases.iter().any(|(alias, _)| alias == name)
            {
                return Outcome::Return(DUPLICATE_ALIAS);
            }
            0
        }
        Some(name) => match aliases.iter().find(|(alias, _)| alias == name) {
            Some(&(_, id)) => id,
            None => return Outcome::Return(INVALID_DEVICE_NAME),
        },
    };
    let words = 1 + plan.fields.len() as u32;
    let texts: Vec<Vec<u16>> = plan
        .fields
        .iter()
        .filter_map(|field| match field {
            Field::Text(text) => Some(text.encode_utf16().chain([0]).collect()),
            Field::Word(_) => None,
        })
        .collect();
    let size = words * 4 + texts.iter().map(|t| t.len() as u32 * 2).sum::<u32>();
    let Some(block) = video::alloc(env, size) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let (mut at, mut strings) = (block + 4, block + words * 4);
    let _ = env
        .cpu
        .write_u32(block, if plan.flags & 1 != 0 { callback } else { 0 });
    let mut texts = texts.into_iter();
    for field in &plan.fields {
        let value = match field {
            Field::Word(value) => *value,
            Field::Text(_) => {
                let text = texts.next().unwrap();
                let bytes: Vec<u8> = text.iter().flat_map(|u| u.to_le_bytes()).collect();
                let _ = env.cpu.write(strings, &bytes);
                strings += bytes.len() as u32;
                strings - bytes.len() as u32
            }
        };
        let _ = env.cpu.write_u32(at, value);
        at += 4;
    }
    let target = match env
        .sys
        .thunks
        .publish(env.cpu, "winmm.dll", "mciSendCommandW")
    {
        Ok(target) => target,
        Err(_) => {
            video::free(env, block);
            return Outcome::Return(UNSUPPORTED_FUNCTION);
        }
    };
    let args = vec![device, plan.message, plan.flags, block];
    Outcome::call(target, args, move |env, result| {
        let result = result.value();
        let word = |env: &Env<'_>, index: u32| env.cpu.read_u32(block + index * 4).unwrap_or(0);
        let text = match &plan.reply {
            _ if result != 0 => None,
            Reply::Empty => Some(String::new()),
            Reply::Opened(name) => {
                let id = word(env, 1);
                env.ext.get_mut::<Aliases>().0.push((name.clone(), id));
                Some(id.to_string())
            }
            Reply::Closed => {
                env.ext
                    .get_mut::<Aliases>()
                    .0
                    .retain(|&(_, id)| id != device);
                Some(String::new())
            }
            Reply::Mode => Some(mode(word(env, 1)).into()),
            Reply::Number => Some(word(env, 1).to_string()),
            Reply::Rect => Some(
                (1..5)
                    .map(|i| word(env, i).to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        };
        video::free(env, block);
        Outcome::Return(match text {
            Some(text) => reply(env, &text, output, capacity, wide),
            None => result,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // spec: MEDIA-MCI-1
    fn siglus_movie_strings_map_to_digital_video_commands() {
        let open =
            plan(r#"open "C:\Game\mov\op.mpg" type MPEGVideo alias SiglusMCIMovie wait"#).unwrap();
        assert_eq!(open.device, None);
        assert_eq!((open.message, open.flags), (0x803, 0x2000 | 0x200 | 2));
        assert_eq!(
            open.fields,
            [
                Field::Word(0),
                Field::Text("MPEGVideo".into()),
                Field::Text(r"C:\Game\mov\op.mpg".into()),
                Field::Word(0),
            ]
        );
        assert_eq!(open.reply, Reply::Opened("siglusmcimovie".into()));
        let window = plan("window SiglusMCIMovie handle 65572 wait").unwrap();
        assert_eq!((window.message, window.flags), (0x841, 0x10002));
        assert_eq!(window.fields[0], Field::Word(65572));
        let put = plan("put SiglusMCIMovie destination at 0 0 1280 720 wait").unwrap();
        assert_eq!((put.message, put.flags), (0x842, 0x50002));
        assert_eq!(put.fields[2..], [Field::Word(1280), Field::Word(720)]);
        let play = plan("play SiglusMCIMovie").unwrap();
        assert_eq!((play.message, play.flags), (0x806, 0));
        let status = plan("status SiglusMCIMovie mode wait").unwrap();
        assert_eq!(
            (status.message, status.flags, status.reply),
            (0x814, 0x102, Reply::Mode)
        );
        assert_eq!(plan("close SiglusMCIMovie wait").unwrap().message, 0x804);
        let left = plan("setaudio SiglusMCIMovie left volume to 750").unwrap();
        assert_eq!((left.message, left.flags), (0x873, 0x1a00000));
        assert_eq!(left.fields[..2], [Field::Word(0x4002), Field::Word(750)]);
    }

    #[test]
    // spec: MEDIA-MCI-3
    fn unsupported_or_malformed_strings_report_parser_errors() {
        for (text, error) in [
            ("", MISSING_COMMAND_STRING),
            ("play", MISSING_DEVICE_NAME),
            ("setaudio movie left volume", UNSUPPORTED_FUNCTION),
            ("setaudio movie bass to 10", UNRECOGNIZED_KEYWORD),
            ("play movie loudly", UNRECOGNIZED_KEYWORD),
            ("seek movie", UNRECOGNIZED_COMMAND),
            ("open \"a.mpg alias movie", NO_CLOSING_QUOTE),
            ("open a.mpg alias movie", DEVICE_TYPE_REQUIRED),
            ("play movie from x", BAD_INTEGER),
            ("put movie destination", UNSUPPORTED_FUNCTION),
        ] {
            assert_eq!(plan(text), Err(error), "{text}");
        }
    }
}
