//! Multimedia error descriptions share A/W encoding and bounded output writes.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

fn description(error: u32) -> Option<&'static str> {
    Some(match error {
        0 => "The specified command was carried out.",
        1 => "Undefined external error.",
        2 => "A device ID has been used that is out of range for your system.",
        3 => "The driver was not enabled.",
        4 => "The specified device is already in use. Wait until it is free, and then try again.",
        5 => "The specified device handle is invalid.",
        6 => "There is no driver installed on your system.",
        7 => {
            "There is not enough memory available for this task. Quit one or more applications to increase available memory, and then try again."
        }
        8 => {
            "This function is not supported. Use the Capabilities function to determine which functions and messages the driver supports."
        }
        9 => "An error number was specified that is not defined in the system.",
        10 => "An invalid flag was passed to a system function.",
        11 => "An invalid parameter was passed to a system function.",
        12 => "Handle being used simultaneously on another thread (eg callback).",
        13 => "Specified alias not found in WIN.INI.",
        14 => "The registry database is corrupt.",
        15 => "The specified registry key was not found.",
        16 => "The registry could not be opened or could not be read.",
        17 => "The registry could not be written to.",
        18 => "The specified registry key could not be deleted.",
        19 => "The specified registry key value could not be found.",
        20 => "The driver did not generate a valid OPEN callback.",
        21 => "More data is available.",
        64 => {
            "The MIDI header was not prepared. Use the Prepare function to prepare the header, and then try again."
        }
        65 => {
            "Cannot perform this operation while media data is still playing. Reset the device, or wait until the data is finished playing."
        }
        66 => {
            "A MIDI map was not found. There may be a problem with the driver, or the MIDIMAP.CFG file may be corrupt or missing."
        }
        67 => {
            "The port is transmitting data to the device. Wait until the data has been transmitted, and then try again."
        }
        68 => {
            "The current MIDI Mapper setup refers to a MIDI device that is not installed on the system. Use MIDI Mapper to edit the setup."
        }
        69 => {
            "The current MIDI setup is damaged. Copy the original MIDIMAP.CFG file to the Windows SYSTEM directory, and then try again."
        }
        70 => {
            "A MIDI call was made which is invalid with the current open mode. Reopen the device with the correct mode."
        }
        71 => "The MIDI driver did not process the message.",
        _ => return None,
    })
}

fn text(env: &mut Env<'_>, error: u32, output: GuestPtr<()>, capacity: u32, wide: bool) -> u32 {
    // The SDK permits a zero-sized destination; no output is accessed.
    if capacity == 0 {
        return 0;
    }
    let Some(message) = description(error) else {
        return 9; // MMSYSERR_BADERRNUM.
    };
    let length = message.len().min(capacity as usize - 1);
    let units = message.as_bytes()[..length].iter().copied().chain([0]);
    let bytes: Vec<u8> = if wide {
        units.flat_map(|b| u16::from(b).to_le_bytes()).collect()
    } else {
        units.collect()
    };
    if check_range(env.cpu, output.addr(), bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output.addr(), &bytes).is_err()
    {
        return 11; // MMSYSERR_INVALPARAM.
    }
    0
}

#[win32(winmm, name = "midiOutGetErrorTextA", level = implement)]
fn output_a(env: &mut Env<'_>, error: u32, output: AnsiStr, capacity: u32) -> u32 {
    text(env, error, output.0.cast(), capacity, false)
}
#[win32(winmm, name = "midiOutGetErrorTextW", level = implement)]
fn output_w(env: &mut Env<'_>, error: u32, output: WideStr, capacity: u32) -> u32 {
    text(env, error, output.0.cast(), capacity, true)
}
#[win32(winmm, name = "midiInGetErrorTextA", level = implement)]
fn input_a(env: &mut Env<'_>, error: u32, output: AnsiStr, capacity: u32) -> u32 {
    text(env, error, output.0.cast(), capacity, false)
}
#[win32(winmm, name = "midiInGetErrorTextW", level = implement)]
fn input_w(env: &mut Env<'_>, error: u32, output: WideStr, capacity: u32) -> u32 {
    text(env, error, output.0.cast(), capacity, true)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(output_a_ABI);
    registry.register(output_w_ABI);
    registry.register(input_a_ABI);
    registry.register(input_w_ABI);
}
