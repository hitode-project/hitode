//! DirectSoundCapture and DirectSoundFullDuplex without capture devices.
//! Creation functions validate as Windows does and then find no driver. The
//! COM classes yield real uninitialized objects whose Initialize finds none.
use super::*;

const IID_CAPTURE: [u8; 16] = [
    0x81, 0x07, 0x21, 0xb0, 0xcd, 0x89, 0xd0, 0x11, 0xaf, 0x08, 0, 0xa0, 0xc9, 0x25, 0xcd, 0x16,
];
const IID_DUPLEX: [u8; 16] = [
    0x7a, 0x4c, 0xcb, 0xed, 0xab, 0xda, 0x16, 0x42, 0xa4, 0x2e, 0x6c, 0x50, 0x59, 0x6d, 0xdc, 0x1d,
];
pub(super) const CLSID_CAPTURE: [u8; 16] = [
    0x80, 0x07, 0x21, 0xb0, 0xcd, 0x89, 0xd0, 0x11, 0xaf, 0x08, 0, 0xa0, 0xc9, 0x25, 0xcd, 0x16,
];
pub(super) const CLSID_CAPTURE8: [u8; 16] = [
    0x13, 0xac, 0xbc, 0xe4, 0x99, 0x7f, 0x08, 0x49, 0x9a, 0x8e, 0x74, 0xe3, 0xbf, 0x24, 0xb6, 0xe1,
];
pub(super) const CLSID_DUPLEX: [u8; 16] = [
    0x0c, 0x30, 0xa4, 0xfe, 0x59, 0x79, 0x47, 0x41, 0xb2, 0x6a, 0x23, 0x77, 0xb9, 0xe7, 0xa9, 0x1d,
];
/// CLASS_E_NOAGGREGATION, which DirectSound names DSERR_NOAGGREGATION.
const NO_AGGREGATION: u32 = 0x8004_0110;
const UNINITIALIZED: u32 = 0x8878_00aa;

pub(super) const CAPTURE: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("CreateCaptureBuffer", 4),
    ("GetCaps", 2),
    ("Initialize", 2),
];
pub(super) const DUPLEX: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("Initialize", 9),
];

/// The creation functions with their stack argument counts.
pub(super) const CREATORS: [(&str, u8); 3] = [
    ("DirectSoundCaptureCreate", 3),
    ("DirectSoundCaptureCreate8", 3),
    ("DirectSoundFullDuplexCreate", 10),
];

/// Run the creation function `name`, or `None` when it is not one.
pub(super) fn create(e: &mut Env<'_>, name: &str) -> Result<Option<u32>, CallError> {
    let hr = match name {
        "DirectSoundCaptureCreate" | "DirectSoundCaptureCreate8" => {
            let (output, outer) = (e.arg(1), e.arg(2));
            if output == 0 {
                INVALID
            } else if outer != 0 {
                NO_AGGREGATION
            } else {
                e.cpu.write_u32(output, 0)?;
                NO_DRIVER
            }
        }
        "DirectSoundFullDuplexCreate" => {
            let (output, outer) = (e.arg(6), e.arg(9));
            if output == 0 {
                INVALID
            } else {
                e.cpu.write_u32(output, 0)?;
                if outer != 0 {
                    NO_AGGREGATION
                } else {
                    initialize_duplex(e, e.arg(7), e.arg(8))?
                }
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(hr))
}

fn initialize_duplex(e: &mut Env<'_>, capture: u32, render: u32) -> Result<u32, CallError> {
    if capture == 0 || render == 0 {
        return Ok(INVALID);
    }
    e.cpu.write_u32(capture, 0)?;
    e.cpu.write_u32(render, 0)?;
    Ok(NO_DRIVER)
}

pub(super) fn create_class(e: &mut Env<'_>, iid: [u8; 16], outer: u32, output: u32) -> Outcome {
    activate(e, Kind::Capture, IID_CAPTURE, iid, outer, output)
}

pub(super) fn create_duplex_class(
    e: &mut Env<'_>,
    iid: [u8; 16],
    outer: u32,
    output: u32,
) -> Outcome {
    activate(e, Kind::Duplex, IID_DUPLEX, iid, outer, output)
}

fn activate(
    e: &mut Env<'_>,
    kind: Kind,
    primary: [u8; 16],
    iid: [u8; 16],
    outer: u32,
    output: u32,
) -> Outcome {
    if outer != 0 {
        return Outcome::Return(NO_AGGREGATION);
    }
    if iid != adv_os::com::IUNKNOWN && iid != primary {
        return Outcome::Return(NO_INTERFACE);
    }
    activate_object(e, kind, iid, output)
}

/// The interface identity an object of `kind` is created with.
pub(super) fn primary(kind: Kind) -> [u8; 16] {
    if matches!(kind, Kind::Duplex) {
        IID_DUPLEX
    } else {
        IID_CAPTURE
    }
}

/// Methods of an uninitialized object: no capture device ever initializes it.
pub(super) fn method(e: &mut Env<'_>, kind: Kind, name: &str) -> Result<u32, CallError> {
    Ok(match (kind, name) {
        (Kind::Capture, "CreateCaptureBuffer") => {
            let (desc, output, outer) = (e.arg(1), e.arg(2), e.arg(3));
            if outer != 0 {
                NO_AGGREGATION
            } else if desc == 0 || output == 0 {
                INVALID
            } else {
                e.cpu.write_u32(output, 0)?;
                UNINITIALIZED
            }
        }
        (Kind::Capture, "GetCaps") => UNINITIALIZED,
        (Kind::Capture, "Initialize") => NO_DRIVER,
        (Kind::Duplex, "Initialize") => initialize_duplex(e, e.arg(7), e.arg(8))?,
        _ => unreachable!("registered {name} on a capture object"),
    })
}
