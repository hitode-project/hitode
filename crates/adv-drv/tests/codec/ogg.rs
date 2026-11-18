use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, Resolver, RunResult,
    layout::{PAGE, PARK_ADDR},
};

struct Platform;

impl Resolver for Platform {
    fn provides(&self, _: &str) -> bool {
        true
    }

    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if name == "done" {
            return Some(HostFn {
                conv: Conv::Cdecl { argc: 0 },
                handler: |_| Outcome::ExitProcess(0),
                data: 0,
                leaf: None,
                trace: false,
                guest: None,
            });
        }
        super::host_fn(module, name)
    }
}

// Authored functions expose a synthetic stream layout through memory operands.
const FIXTURE: &str = "
field body_data packet 6 1
field body_fill pagein 6 1
field body_returned packet 9 1
field lacing_vals packet 12 1
field granule_vals packet 15 1
field lacing_packet packet 18 1
field lacing_returned packet 21 1
field packetno packet 24 1
root packet 6
code packet 34 98aeba9cb8442bf623710722bb41e5b5a4a2b6c5f7ec243e8fb5ed6b3519471a
root reset 6
code reset 13 a188a8772ab4e321921e6e8188e152d44a6ecbcef5f20dcf41de80f41fc4b987
root pagein 6
code pagein 14 34a5a79c5d213f54afa8267f465c3504a4ca3aae14608129a096bd34b1851738
";

const PACKET_CODE: [u8; 34] = [
    0x55, 0x8b, 0xec, 0x83, 0xec, 0x0c, 0x8b, 0x41, 4, 0x8b, 0x41, 12, 0x8b, 0x41, 16, 0x8b, 0x41,
    20, 0x8b, 0x41, 24, 0x8b, 0x41, 28, 0x8b, 0x41, 32, 0xb8, 1, 0, 0, 0, 0xc9, 0xc3,
];
const RESET_CODE: [u8; 13] = [
    0x55, 0x8b, 0xec, 0x8b, 0x45, 8, 0x8b, 0x40, 8, 0x31, 0xc0, 0x5d, 0xc3,
];
const PAGEIN_CODE: [u8; 14] = [
    0x55, 0x8b, 0xec, 0x8b, 0x45, 8, 0x8b, 0x40, 8, 0x90, 0x31, 0xc0, 0x5d, 0xc3,
];

// spec: CODEC-MAP-1, CODEC-CALL-3
#[test]
fn binds_relocated_functions_and_predicts_only_complete_packets_across_pages() {
    Family::parse(FAMILY).unwrap();
    let family = Family::parse(FIXTURE).unwrap();
    for base in [0x400000, 0x600000] {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Platform,
            Clock::virtual_time(),
        )
        .unwrap();
        super::super::install(&mut m.ext, super::super::Mode::Native);
        let stream = base + PAGE;
        let body = stream + 128;
        let laces = stream + 512;
        let granules = stream + 576;
        let out = stream + 640;
        let peek = stream + 704;
        m.cpu.map(base, PAGE, Prot::RX).unwrap();
        m.cpu.map(stream, PAGE, Prot::RW).unwrap();
        m.cpu.write(base, &PACKET_CODE).unwrap();
        m.cpu.write(base + 64, &RESET_CODE).unwrap();
        m.cpu.write(base + 128, &PAGEIN_CODE).unwrap();
        let ranges = [ExecutableRange {
            start: base,
            len: PAGE,
        }];
        m.visit_context(None, |e| bind_family(e, &ranges, &family))
            .unwrap()
            .unwrap();
        assert_eq!(m.ext.get::<Streams>().bindings.len(), 1);
        assert_eq!(m.sys.redirects.len(), 3);

        let write_stream = |m: &mut Machine<JitCpu>,
                            filled: u32,
                            lacing_packet: u32,
                            lacing_returned: u32,
                            body_returned: u32,
                            packetno: u32| {
            for (i, value) in [
                0,
                body,
                filled,
                body_returned,
                laces,
                granules,
                lacing_packet,
                lacing_returned,
                packetno,
            ]
            .into_iter()
            .enumerate()
            {
                m.cpu.write_u32(stream + 4 * i as u32, value).unwrap();
            }
        };
        // Page one ends after the first segment of a 258-byte packet.
        write_stream(&mut m, 257, 1, 0, 0, 0);
        m.cpu.write(body, &[12, 42]).unwrap();
        m.cpu.write(body + 2, &[91; 255]).unwrap();
        m.cpu.write_u32(laces, 0x102).unwrap();
        m.cpu.write_u32(laces + 4, 255).unwrap();
        m.cpu.write(granules, &13i64.to_le_bytes()).unwrap();

        let done = m.resolve_import(MODULE, "done").unwrap();
        let mut caller = Asm::new(base + 256);
        for (address, advance) in [(peek, 0), (out, 1)] {
            caller.mov_ecx_imm(stream);
            caller.bytes.push(0xba);
            caller.bytes.extend_from_slice(&address.to_le_bytes());
            caller.push_imm(advance).call(base).add_esp(4);
        }
        caller.call(done);
        m.cpu.write(base + 256, &caller.bytes).unwrap();
        m.spawn("authored Ogg ABI fixture", base + 256, &[], 0x10000)
            .unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        let (source, advanced) = take(&mut m.ext, out).unwrap();
        assert!(advanced);
        assert_eq!(take(&mut m.ext, peek), Some((source, false)));

        let first = predict(&m.cpu, &m.ext, source, 0, 2);
        assert_eq!(
            first,
            [Packet {
                bytes: vec![12, 42],
                bos: 0x100,
                eos: 0,
                granulepos: 13,
                packetno: 0
            }]
        );
        write_stream(&mut m, 257, 1, 1, 2, 1);
        assert!(predict(&m.cpu, &m.ext, source, 1, 2).is_empty());

        // A continued page completes the split packet and ends with an empty EOS packet.
        write_stream(&mut m, 260, 4, 1, 2, 1);
        m.cpu.write(body + 257, &[2, 4, 6]).unwrap();
        m.cpu.write_u32(laces + 8, 3).unwrap();
        m.cpu.write_u32(laces + 12, 0x200).unwrap();
        m.cpu.write(granules + 16, &(-1i64).to_le_bytes()).unwrap();
        m.cpu.write(granules + 24, &91i64.to_le_bytes()).unwrap();
        let mut expected = vec![91; 255];
        expected.extend([2, 4, 6]);
        let predicted = predict(&m.cpu, &m.ext, source, 1, 2);
        assert_eq!(
            predicted,
            [
                Packet {
                    bytes: expected,
                    bos: 0,
                    eos: 0,
                    granulepos: -1,
                    packetno: 1
                },
                Packet {
                    bytes: vec![],
                    bos: 0,
                    eos: 0x200,
                    granulepos: 91,
                    packetno: 2
                },
            ]
        );
        assert_eq!(m.cpu.read_u32(stream + 12).unwrap(), 2);
        assert_eq!(m.cpu.read_u32(stream + 28).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(stream + 32).unwrap(), 1);
        m.cpu.write_u32(laces + 4, 0x400).unwrap();
        assert!(predict(&m.cpu, &m.ext, source, 1, 2).is_empty());
    }
}
