//! Inject construction faults over real guest mappings, then retry the same ID.
use super::*;
use adv_cpu::JitCpu;

const TEMPLATE: [u8; 4] = [0x91, 0x52, 0xa3, 0xe4];
const ARGS: [u32; 2] = [0x11223344, 0xaabbccdd];

struct Empty;
impl Resolver for Empty {
    fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
        None
    }
    fn provides(&self, _: &str) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    TebMap,
    TebWrite,
    StackArg,
    StackReturn,
    TlsDirectoryMap,
    TlsBlockMap,
    TlsTemplateWrite,
    TlsSlotWrite,
    TlsPointerWrite,
}
struct FaultGuest<'a> {
    inner: &'a mut JitCpu,
    fail: Failure,
    fired: bool,
    maps: Vec<(u32, u32, bool)>,
    unmaps: Vec<(u32, u32)>,
}
impl FaultGuest<'_> {
    fn reject(&mut self, matches: bool, addr: u32, len: u32) -> Result<(), MemFault> {
        if matches && !self.fired {
            self.fired = true;
            Err(MemFault { addr, len })
        } else {
            Ok(())
        }
    }
}
impl Guest for FaultGuest<'_> {
    fn reg(&self, reg: Reg) -> u32 {
        self.inner.reg(reg)
    }
    fn set_reg(&mut self, reg: Reg, value: u32) {
        self.inner.set_reg(reg, value);
    }
    fn read(&self, addr: u32, data: &mut [u8]) -> Result<(), MemFault> {
        self.inner.read(addr, data)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
        let fail = match self.fail {
            Failure::TebWrite => addr == teb_addr(0) + teb::SELF,
            Failure::StackArg => data == ARGS[1].to_le_bytes(),
            Failure::StackReturn => data == thunk_addr(thunk::RETURN_PAD).to_le_bytes(),
            Failure::TlsTemplateWrite => data == TEMPLATE,
            Failure::TlsSlotWrite => self.maps.get(2).is_some_and(|m| addr == m.0),
            Failure::TlsPointerWrite => addr == teb_addr(0) + teb::TLS_POINTER,
            _ => false,
        };
        self.reject(fail, addr, data.len() as u32)?;
        self.inner.write(addr, data)
    }
    fn map(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
        let ordinal = self.maps.len() + 1;
        self.maps.push((addr, len, false));
        let fail = match self.fail {
            Failure::TebMap => addr == teb_addr(0),
            Failure::TlsDirectoryMap => ordinal == 3,
            Failure::TlsBlockMap => ordinal == 4,
            _ => false,
        };
        self.reject(fail, addr, len)?;
        self.inner.map(addr, len, prot)?;
        self.maps.last_mut().unwrap().2 = true;
        Ok(())
    }
    fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        self.unmaps.push((addr, len));
        self.inner.unmap(addr, len)
    }
    fn protect(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
        self.inner.protect(addr, len, prot)
    }
}

#[test]
fn failed_spawn_preserves_foreign_teb_releases_owned_storage_and_retries_same_id() {
    for failure in [
        Failure::TebMap,
        Failure::TebWrite,
        Failure::StackArg,
        Failure::StackReturn,
        Failure::TlsDirectoryMap,
        Failure::TlsBlockMap,
        Failure::TlsTemplateWrite,
        Failure::TlsSlotWrite,
        Failure::TlsPointerWrite,
    ] {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Empty,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.sys.tls_images.push(TlsImage {
            index: 0,
            template: TEMPLATE.to_vec(),
            zero_fill: 8,
            active: true,
        });
        let collision = matches!(failure, Failure::TebMap);
        if collision {
            machine.cpu.map(teb_addr(0), PAGE, Prot::RW).unwrap();
            machine.cpu.write_u32(teb_addr(0), 0xfeedcafe).unwrap();
        }
        let tid = machine.sys.next_tid;
        let (maps, unmaps) = {
            let mut guest = FaultGuest {
                inner: &mut machine.cpu,
                fail: failure,
                fired: false,
                maps: Vec::new(),
                unmaps: Vec::new(),
            };
            assert!(
                machine
                    .sys
                    .spawn(&mut guest, "failed", 0x400000, &ARGS, 0x10000)
                    .is_err(),
                "{failure:?}"
            );
            assert!(guest.fired, "failure point was not reached: {failure:?}");
            (guest.maps, guest.unmaps)
        };
        assert!(machine.sys.contexts.is_empty(), "{failure:?}");
        assert_eq!(machine.sys.next_tid, tid, "{failure:?}");
        assert_eq!(machine.sys.objects.object_count(), 0, "{failure:?}");
        assert_eq!(machine.sys.objects.alias_count(Namespace::DEFAULT), 0);
        for &(addr, len, mapped) in &maps {
            if addr != teb_addr(0) {
                assert!(machine.sys.vm.containing(addr).is_none(), "{failure:?}");
            }
            if mapped {
                assert!(machine.cpu.read_u32(addr).is_err(), "{failure:?}");
                assert!(machine.cpu.read_u32(addr + len - 4).is_err(), "{failure:?}");
            }
        }
        assert!(machine.sys.vm.region(DYN_START).is_none(), "{failure:?}");
        if collision {
            assert!(unmaps.iter().all(|&(addr, _)| addr != teb_addr(0)));
            assert_eq!(machine.cpu.read_u32(teb_addr(0)).unwrap(), 0xfeedcafe);
            machine.cpu.unmap(teb_addr(0), PAGE).unwrap();
        } else {
            assert!(machine.cpu.read_u32(teb_addr(0)).is_err());
        }

        let id = machine.spawn("retry", 0x400000, &ARGS, 0x10000).unwrap();
        assert_eq!(id, 0, "{failure:?}");
        let context = &machine.sys.contexts[id as usize];
        assert_eq!(context.tid, tid);
        assert_eq!(context.teb, teb_addr(0));
        assert_eq!(context.thread_handle, 0x104);
        assert_eq!(context.stack_lo, DYN_START + PAGE);
        assert!(matches!(
            machine.sys.objects.get(context.thread_handle),
            Some(Object::Thread { ctx: 0 })
        ));
        let (sp, entry) = context.start.unwrap();
        assert_eq!(entry, 0x400000);
        assert_eq!(
            machine.cpu.read_u32(sp).unwrap(),
            thunk_addr(thunk::RETURN_PAD)
        );
        assert_eq!(machine.cpu.read_u32(sp + 4).unwrap(), ARGS[0]);
        assert_eq!(machine.cpu.read_u32(sp + 8).unwrap(), ARGS[1]);
        let directory = machine
            .cpu
            .read_u32(context.teb + teb::TLS_POINTER)
            .unwrap();
        let block = machine.cpu.read_u32(directory).unwrap();
        let mut bytes = [0xff; 12];
        machine.cpu.read(block, &mut bytes).unwrap();
        assert_eq!(&bytes[..4], &TEMPLATE);
        assert_eq!(&bytes[4..], &[0; 8]);
        assert_eq!(machine.sys.next_tid, tid + 4);
    }
}
