//! Event-driven external gamepad capture on macOS and iOS via `GameController.framework`.
//!
//! Listens to `GCControllerDidConnectNotification` / `Disconnect` and attaches
//! `valueChangedHandler` on each `GCExtendedGamepad`. A 60 Hz `NSTimer` runs
//! only while an analog stick mapped to cursor or wheel is actively deflected.

use crate::gamepad;
use adv_shell::gamepad::{GamepadButton, GamepadStyle};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{MainThreadMarker, msg_send};
use objc2_foundation::{
    NSArray, NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue, NSString,
    NSTimer,
};
use std::cell::RefCell;
use std::ptr::NonNull;

#[link(name = "GameController", kind = "framework")]
unsafe extern "C" {}

struct Attached {
    ptr: usize,
    id: String,
    #[allow(dead_code)]
    controller: Retained<AnyObject>,
    buttons: [bool; 16],
    sticks: [f32; 4],
}

#[derive(Default)]
struct AppleGamepadHost {
    attached: Vec<Attached>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    tick_timer: Option<Retained<NSTimer>>,
}

thread_local! {
    static HOST: RefCell<AppleGamepadHost> = RefCell::new(AppleGamepadHost::default());
}

/// Starts observing `GCController` connections on the main thread.
pub fn setup() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Some(cls) = AnyClass::get(c"GCController") else {
        return;
    };
    let center = NSNotificationCenter::defaultCenter();
    let queue = NSOperationQueue::mainQueue();

    let connect_name = NSString::from_str("GCControllerDidConnectNotification");
    let connect_block = RcBlock::new(|note: NonNull<NSNotification>| {
        let note = unsafe { note.as_ref() };
        if let Some(obj) = note.object() {
            attach_controller(obj);
        }
    });
    let connect_obs = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(&connect_name),
            None,
            Some(&queue),
            &connect_block,
        )
    };

    let disconnect_name = NSString::from_str("GCControllerDidDisconnectNotification");
    let disconnect_block = RcBlock::new(|note: NonNull<NSNotification>| {
        let note = unsafe { note.as_ref() };
        if let Some(obj) = note.object() {
            detach_controller(&obj);
        }
    });
    let disconnect_obs = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(&disconnect_name),
            None,
            Some(&queue),
            &disconnect_block,
        )
    };

    HOST.with_borrow_mut(|host| {
        host.observers.push(connect_obs);
        host.observers.push(disconnect_obs);
    });

    // Attach any controllers already connected before the app launched.
    let existing: Retained<NSArray<AnyObject>> = unsafe { msg_send![cls, controllers] };
    let count = existing.count();
    for i in 0..count {
        let controller = existing.objectAtIndex(i);
        attach_controller(controller);
    }
}

fn detect_style(category: &str, name: &str) -> GamepadStyle {
    let combined = format!("{} {}", category.to_lowercase(), name.to_lowercase());
    if combined.contains("dualsense")
        || combined.contains("dualshock")
        || combined.contains("playstation")
        || combined.contains("sony")
    {
        GamepadStyle::PlayStation
    } else if combined.contains("switch")
        || combined.contains("joy-con")
        || combined.contains("nintendo")
        || combined.contains("pro controller")
    {
        GamepadStyle::Switch
    } else {
        GamepadStyle::Xbox
    }
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "controller".into()
    } else {
        trimmed.into()
    }
}

fn attach_controller(controller: Retained<AnyObject>) {
    let ptr = Retained::as_ptr(&controller) as usize;
    let already = HOST.with_borrow(|host| host.attached.iter().any(|a| a.ptr == ptr));
    if already {
        return;
    }

    let extended: Option<Retained<AnyObject>> = unsafe { msg_send![&*controller, extendedGamepad] };
    let Some(extended) = extended else {
        return;
    };

    let vendor: Option<Retained<NSString>> = unsafe { msg_send![&*controller, vendorName] };
    let category: Option<Retained<NSString>> = unsafe { msg_send![&*controller, productCategory] };
    let name = vendor
        .as_deref()
        .map(NSString::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            category
                .as_deref()
                .map(NSString::to_string)
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "Game Controller".into());
    let category_str = category
        .as_deref()
        .map(NSString::to_string)
        .unwrap_or_default();
    let style = detect_style(&category_str, &name);

    let base_id = format!("apple:{}:{}", style.as_str(), slug(&name));
    let id = HOST.with_borrow(|host| {
        if !host.attached.iter().any(|a| a.id == base_id) {
            return base_id;
        }
        let mut idx = 2usize;
        loop {
            let candidate = format!("{base_id}#{idx}");
            if !host.attached.iter().any(|a| a.id == candidate) {
                return candidate;
            }
            idx += 1;
        }
    });

    HOST.with_borrow_mut(|host| {
        host.attached.push(Attached {
            ptr,
            id: id.clone(),
            controller: controller.clone(),
            buttons: [false; 16],
            sticks: [0.0; 4],
        });
    });

    gamepad::on_device_connected(id, name, style);

    let handler = RcBlock::new(
        move |gp: NonNull<AnyObject>, _element: NonNull<AnyObject>| {
            if MainThreadMarker::new().is_some() {
                let gp_ref = unsafe { gp.as_ref() };
                handle_extended_change(ptr, gp_ref);
            } else {
                let gp_retained = unsafe { Retained::retain(gp.as_ptr()) };
                if let Some(gp_retained) = gp_retained {
                    let op = RcBlock::new(move || {
                        handle_extended_change(ptr, &gp_retained);
                    });
                    unsafe {
                        NSOperationQueue::mainQueue().addOperationWithBlock(&op);
                    }
                }
            }
        },
    );
    unsafe {
        let _: () = msg_send![&*extended, setValueChangedHandler: &*handler];
    }
}

fn detach_controller(controller: &AnyObject) {
    let ptr = std::ptr::from_ref(controller) as usize;
    let extended: Option<Retained<AnyObject>> = unsafe { msg_send![controller, extendedGamepad] };
    if let Some(extended) = extended {
        let nil: *const AnyObject = std::ptr::null();
        unsafe {
            let _: () = msg_send![&*extended, setValueChangedHandler: nil];
        }
    }
    let removed = HOST.with_borrow_mut(|host| {
        let pos = host.attached.iter().position(|a| a.ptr == ptr)?;
        Some(host.attached.remove(pos))
    });
    if let Some(entry) = removed {
        gamepad::on_device_disconnected(&entry.id);
    }
}

fn button_pressed(btn: Option<&AnyObject>) -> bool {
    let Some(btn) = btn else {
        return false;
    };
    let pressed: Bool = unsafe { msg_send![btn, isPressed] };
    if pressed.as_bool() {
        return true;
    }
    let value: f32 = unsafe { msg_send![btn, value] };
    value > 0.35
}

fn axis_value(axis: Option<&AnyObject>) -> f32 {
    let Some(axis) = axis else {
        return 0.0;
    };
    unsafe { msg_send![axis, value] }
}

fn read_extended_snapshot(gp: &AnyObject) -> ([bool; 16], [f32; 4]) {
    let get = |sel_obj: Option<Retained<AnyObject>>| button_pressed(sel_obj.as_deref());
    let btn_a: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonA] };
    let btn_b: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonB] };
    let btn_x: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonX] };
    let btn_y: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonY] };
    let l1: Option<Retained<AnyObject>> = unsafe { msg_send![gp, leftShoulder] };
    let r1: Option<Retained<AnyObject>> = unsafe { msg_send![gp, rightShoulder] };
    let l2: Option<Retained<AnyObject>> = unsafe { msg_send![gp, leftTrigger] };
    let r2: Option<Retained<AnyObject>> = unsafe { msg_send![gp, rightTrigger] };
    let l3: Option<Retained<AnyObject>> = unsafe { msg_send![gp, leftThumbstickButton] };
    let r3: Option<Retained<AnyObject>> = unsafe { msg_send![gp, rightThumbstickButton] };
    let dpad: Option<Retained<AnyObject>> = unsafe { msg_send![gp, dpad] };
    let (up, down, left, right) = if let Some(d) = dpad.as_deref() {
        let u: Option<Retained<AnyObject>> = unsafe { msg_send![d, up] };
        let dn: Option<Retained<AnyObject>> = unsafe { msg_send![d, down] };
        let l: Option<Retained<AnyObject>> = unsafe { msg_send![d, left] };
        let r: Option<Retained<AnyObject>> = unsafe { msg_send![d, right] };
        (get(u), get(dn), get(l), get(r))
    } else {
        (false, false, false, false)
    };
    let menu: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonMenu] };
    let options: Option<Retained<AnyObject>> = unsafe { msg_send![gp, buttonOptions] };

    let buttons = [
        get(btn_a),
        get(btn_b),
        get(btn_x),
        get(btn_y),
        get(l1),
        get(r1),
        get(l2),
        get(r2),
        get(l3),
        get(r3),
        up,
        down,
        left,
        right,
        get(menu),
        get(options),
    ];

    let read_stick = |stick: Option<Retained<AnyObject>>| -> (f32, f32) {
        let Some(s) = stick.as_deref() else {
            return (0.0, 0.0);
        };
        let x: Option<Retained<AnyObject>> = unsafe { msg_send![s, xAxis] };
        let y: Option<Retained<AnyObject>> = unsafe { msg_send![s, yAxis] };
        // GameController uses +y up; engine uses +y down (screen space).
        (axis_value(x.as_deref()), -axis_value(y.as_deref()))
    };
    let ls: Option<Retained<AnyObject>> = unsafe { msg_send![gp, leftThumbstick] };
    let rs: Option<Retained<AnyObject>> = unsafe { msg_send![gp, rightThumbstick] };
    let (lx, ly) = read_stick(ls);
    let (rx, ry) = read_stick(rs);

    (buttons, [lx, ly, rx, ry])
}

fn handle_extended_change(ptr: usize, gp: &AnyObject) {
    let (buttons, sticks) = read_extended_snapshot(gp);
    let diff = HOST.with_borrow_mut(|host| {
        let entry = host.attached.iter_mut().find(|a| a.ptr == ptr)?;
        let mut btn_changes = Vec::new();
        for (i, &now) in buttons.iter().enumerate() {
            if entry.buttons[i] != now {
                entry.buttons[i] = now;
                btn_changes.push((GamepadButton::ALL[i], now));
            }
        }
        let left_changed = (entry.sticks[0] - sticks[0]).abs() > 0.002
            || (entry.sticks[1] - sticks[1]).abs() > 0.002;
        let right_changed = (entry.sticks[2] - sticks[2]).abs() > 0.002
            || (entry.sticks[3] - sticks[3]).abs() > 0.002;
        entry.sticks = sticks;
        Some((entry.id.clone(), btn_changes, left_changed, right_changed))
    });

    let Some((id, btn_changes, left_changed, right_changed)) = diff else {
        return;
    };
    for (btn, down) in btn_changes {
        gamepad::on_button(&id, btn, down);
    }
    let mut needs_tick = false;
    if left_changed {
        needs_tick |= gamepad::on_stick(&id, false, sticks[0], sticks[1]);
    }
    if right_changed {
        needs_tick |= gamepad::on_stick(&id, true, sticks[2], sticks[3]);
    }
    if left_changed || right_changed {
        sync_tick_timer(needs_tick);
    }
}

fn sync_tick_timer(needs_tick: bool) {
    HOST.with_borrow_mut(|host| {
        if needs_tick {
            if host.tick_timer.is_none() {
                let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
                    if !gamepad::on_tick() {
                        sync_tick_timer(false);
                    }
                });
                let timer = unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 60.0, true, &block)
                };
                host.tick_timer = Some(timer);
            }
        } else if let Some(timer) = host.tick_timer.take() {
            timer.invalidate();
        }
    });
}
