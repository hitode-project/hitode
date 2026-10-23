use super::*;
use adv_machine::{Clock, Resume, WakeReason, layout::PARK_ADDR, sched::Wait};

// spec: WINE-AUD-2, WINE-AUD-3, WINE-THR-1
#[test]
fn render_formats_wrap_and_follow_the_mixer_cursor() {
    for (tag, bits, sample) in [
        (1, 8, vec![160]),
        (1, 16, 8192i16.to_le_bytes().to_vec()),
        (1, 24, vec![0, 0, 32]),
        (1, 32, 0x2000_0000u32.to_le_bytes().to_vec()),
        (3, 32, 0.25f32.to_le_bytes().to_vec()),
        (3, 64, 0.25f64.to_le_bytes().to_vec()),
    ] {
        let cpu = adv_cpu::Cpu::new(PARK_ADDR, false).unwrap();
        let mut machine = Machine::new(cpu, Registry::default(), Clock::virtual_time()).unwrap();
        let audio = AudioControl::new(false);
        audio.sync_clock(machine.sys.clock.now_us());
        let mut reader = audio.stream().device_reader(44100, 2).unwrap();
        machine.ext.insert(audio);
        let env = &mut Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let args = allocate(env, 4096).unwrap();
        let fmt = args + 128;
        let align = bits / 4;
        crate::write_words(
            env,
            fmt,
            &[
                tag | (2 << 16),
                44100,
                44100 * align,
                align | (bits << 16),
                0,
            ],
        )
        .unwrap();
        crate::write_words(
            env,
            args,
            &[
                0,
                0,
                0,
                0,
                0,
                0,
                300_000,
                0,
                100_000,
                0,
                fmt,
                0,
                args + 256,
                args + 264,
            ],
        )
        .unwrap();
        execute(env, 4, args).unwrap();
        assert_eq!(read(env, args + 44).unwrap(), 0);
        let id = read(env, args + 264).unwrap();
        crate::write_words(env, args, &[id, 0, 882, 0, args + 256]).unwrap();
        execute(env, 10, args).unwrap();
        let data = read(env, args + 256).unwrap();
        let pcm = sample.repeat(882 * 2);
        env.cpu.write(data, &pcm).unwrap();
        crate::write_words(env, args, &[id, 0, 882, 0, 0]).unwrap();
        execute(env, 11, args).unwrap();
        assert_eq!(read(env, args + 16).unwrap(), 0);
        execute(env, 6, args).unwrap();
        assert_eq!(read(env, args + 8).unwrap(), 0);
        let mut output = [0.0; 882];
        reader.read(&mut output, 0);
        assert!(
            output.iter().filter(|&&v| (v - 0.25).abs() < 0.005).count() >= 800,
            "format {tag}/{bits}"
        );
        env.sys.clock.advance_to(env.sys.clock.now_us() + 10_000);
        crate::write_words(env, args, &[id, 0, 0, args + 256]).unwrap();
        execute(env, 20, args).unwrap();
        assert_eq!(read(env, args + 256).unwrap(), 441);
        crate::write_words(env, args, &[id, 0, 0, 0, args + 256, args + 272]).unwrap();
        execute(env, 23, args).unwrap();
        assert_eq!(read(env, args + 256).unwrap(), 441 * align);
        crate::write_words(env, args, &[id, 0, 883, 0, args + 256]).unwrap();
        execute(env, 10, args).unwrap();
        assert_eq!(read(env, args + 12).unwrap(), 0x8889_0006);
        write(env, args + 8, 882).unwrap();
        execute(env, 10, args).unwrap();
        assert_eq!(read(env, args + 12).unwrap(), 0);
        let wrapped = read(env, args + 256).unwrap();
        assert_ne!(wrapped, data);
        env.cpu.write(wrapped, &pcm).unwrap();
        // An oversized release preserves the outstanding buffer.
        crate::write_words(env, args, &[id, 0, 883, 0, 0]).unwrap();
        execute(env, 11, args).unwrap();
        assert_eq!(read(env, args + 16).unwrap(), 0x8889_0009);
        write(env, args + 8, 882).unwrap();
        execute(env, 11, args).unwrap();
        assert_eq!(read(env, args + 16).unwrap(), 0);
        for _ in 0..3 {
            reader.read(&mut output, 0);
            assert!(output.iter().all(|&v| (v - 0.25).abs() < 0.005));
            env.sys.clock.advance_to(env.sys.clock.now_us() + 10_000);
            crate::write_words(env, args, &[id, 0, 0, args + 256]).unwrap();
            execute(env, 20, args).unwrap();
        }
        assert_eq!(read(env, args + 256).unwrap(), 0);
        execute(env, 7, args).unwrap();
        execute(env, 8, args).unwrap();
        assert_eq!(read(env, args + 8).unwrap(), 0);
        let now = env.sys.clock.now_us();
        let Outcome::Block { wait, then, .. } = stream::timer(env, id, None) else {
            panic!("timer did not block")
        };
        assert_eq!(wait, Wait::Until(now + 10_000));
        env.ext
            .get_mut::<Streams>()
            .streams
            .get_mut(&id)
            .unwrap()
            .quit = true;
        assert!(matches!(
            then(env, Resume::Woke(WakeReason::Timeout)),
            Outcome::Return(0)
        ));
        crate::write_words(env, args, &[id, 0, 0, 0]).unwrap();
        assert!(matches!(stream::release(env, args), Outcome::Return(0)));
        reader.read(&mut output, 0);
        assert!(output.iter().all(|&v| v == 0.0));
    }
}
