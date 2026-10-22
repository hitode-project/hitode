//! A/W style transitions expose old state before mutation and edited state after it.
use super::*;

fn styles<B: TestCpu>(cpu: B, wide: bool, index: i32) {
    let mut f = Fixture::new(cpu);
    let flag = if index == -16 {
        0x0200_0000u32
    } else {
        0x20u32
    };
    let capture = f.alloc(368);
    let original = f.callback(wide, capture, None);
    let observed = f.alloc(24);
    let start = f.procedure;
    let mut code = Asm::new(start);
    let get = f.function(if wide {
        "GetWindowLongW"
    } else {
        "GetWindowLongA"
    });
    for (message, offset) in [(wm::STYLECHANGING, 0), (wm::STYLECHANGED, 12)] {
        on_message(&mut code, message, |code| {
            code.mov_eax_esp(16)
                .mov_eax_ptr_eax()
                .mov_mem_eax(observed + offset);
            code.mov_eax_esp(16);
            code.bytes.extend_from_slice(&[0x8b, 0x40, 4]);
            code.mov_mem_eax(observed + offset + 4);
            if message == wm::STYLECHANGING {
                code.bytes.push(0x0d); // OR EAX, the guest's additional style.
                code.bytes.extend_from_slice(&flag.to_le_bytes());
                code.bytes
                    .extend_from_slice(&[0x8b, 0x54, 0x24, 16, 0x89, 0x42, 4]);
            }
            code.push_imm(index as u32);
            code.mov_eax_esp(8)
                .push_eax()
                .call(get)
                .mov_mem_eax(observed + offset + 8);
        });
    }
    code.jmp(original);
    f.machine.cpu.write(start, &code.bytes).unwrap();
    f.machine.cpu.region(start, code.bytes.len());
    f.procedure = code.here().next_multiple_of(16);
    let class = f.class(start, "style transitions", wide);
    let title = f.text("style", wide);
    let window = f.create(M(class), title, wide, false);
    let normal = if index == -16 { 0x00c0_0000 } else { 0 };
    let alternate = if index == -16 { WS_POPUP } else { 0x40000 };
    let mut cases = Vec::new();
    for new in [normal, alternate, normal] {
        f.call("kernel32.dll!SetLastError", &[I(0x4567)]);
        let old = f.call(
            if wide {
                "SetWindowLongW"
            } else {
                "SetWindowLongA"
            },
            &[M(window), I(index as u32), I(new)],
        );
        let last_error = f.call("kernel32.dll!GetLastError", &[]);
        let snapshot = f.alloc(24);
        for i in 0..6 {
            f.code
                .mov_eax_mem(observed + i * 4)
                .mov_mem_eax(snapshot + i * 4);
        }
        let actual = f.call(
            if wide {
                "GetWindowLongA"
            } else {
                "GetWindowLongW"
            },
            &[M(window), I(index as u32)],
        );
        cases.push((new, old, snapshot, actual, last_error));
    }
    f.call("kernel32.dll!SetLastError", &[I(0x4567)]);
    let user_data = f.call("SetWindowLongW", &[M(window), I((-21i32) as u32), I(42)]);
    let user_data_error = f.call("kernel32.dll!GetLastError", &[]);
    let invalid = f.call(
        if wide {
            "SetWindowLongW"
        } else {
            "SetWindowLongA"
        },
        &[I(0xdeadbeef), I(index as u32), I(0)],
    );
    let error = f.call("kernel32.dll!GetLastError", &[]);
    f.run();
    let mut previous = if index == -16 { WS_POPUP } else { 0x80 };
    for (new, old, snapshot, actual, last_error) in cases {
        let edited = new | flag;
        assert_eq!(f.value(old), previous);
        assert_eq!(f.value(last_error), 0x4567);
        let expected = [previous, new, previous, previous, edited, edited];
        for (i, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                f.value(snapshot + i as u32 * 4),
                expected,
                "style field {i}"
            );
        }
        assert_eq!(f.value(actual), edited);
        previous = edited;
    }
    assert_eq!(f.value(user_data), 0);
    assert_eq!(f.value(user_data_error), 0x4567);
    assert_eq!(f.value(invalid), 0);
    assert_eq!(f.value(error), 1400);
    let messages: Vec<_> = f
        .messages(f.value(window))
        .into_iter()
        .filter(|message| [wm::STYLECHANGING, wm::STYLECHANGED].contains(message))
        .collect();
    assert_eq!(messages, [wm::STYLECHANGING, wm::STYLECHANGED].repeat(3));
}
#[test]
// spec: WIN32-USER-10
fn style_changes_can_enter_and_leave_popup_styles_and_apply_guest_edits() {
    for wide in [false, true] {
        for index in [-16, -20] {
            styles(JitCpu::new(PARK_ADDR, false).unwrap(), wide, index);
            styles(JitCpu::new(PARK_ADDR, true).unwrap(), wide, index);
            styles(UnicornCpu::new(PARK_ADDR).unwrap(), wide, index);
        }
    }
}
