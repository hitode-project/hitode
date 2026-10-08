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

#[test]
fn sequential_threads_reuse_execution_storage_and_keep_completed_handle_identities() {
    for count in [200, 1000] {
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
        let keeper = machine.spawn("keeper", 0x400000, &[], 0x10000).unwrap();
        let mut retired = Vec::new();
        let mut addresses = None;
        for sequence in 1..=count {
            let id = machine
                .spawn("short", 0x400000, &[sequence], 0x10000)
                .unwrap();
            let context = machine.sys.context(id);
            let handle = context.thread_handle;
            let tid = context.tid;
            let teb = context.teb;
            let stack = context.stack_lo;
            let directory = machine.cpu.read_u32(teb + teb::TLS_POINTER).unwrap();
            let tls = machine.cpu.read_u32(directory).unwrap();
            let actual = (teb, stack, directory, tls);
            assert_eq!(
                *addresses.get_or_insert(actual),
                actual,
                "storage is reused"
            );
            assert_eq!(machine.cpu.read_u32(stack).unwrap(), 0);
            assert_eq!(machine.cpu.read_u32(teb + teb::LAST_ERROR).unwrap(), 0);
            assert_eq!(machine.cpu.read_u32(teb + teb::TLS_SLOTS).unwrap(), 0);
            assert_eq!(machine.cpu.read_u32(directory + 4).unwrap(), 0);
            let mut bytes = [0xff; 12];
            machine.cpu.read(tls, &mut bytes).unwrap();
            assert_eq!(&bytes[..4], &TEMPLATE);
            assert_eq!(&bytes[4..], &[0; 8]);
            assert_eq!(machine.cpu.read_u32(teb + teb::THREAD_ID).unwrap(), tid);
            assert_eq!(
                machine
                    .sys
                    .contexts
                    .iter()
                    .filter(|c| c.storage.is_some())
                    .count(),
                2
            );
            machine.cpu.write_u32(stack, 0xfeedcafe).unwrap();
            machine.cpu.write_u32(teb + teb::LAST_ERROR, 777).unwrap();
            machine.cpu.write_u32(teb + teb::TLS_SLOTS, 888).unwrap();
            machine.cpu.write_u32(directory + 4, 999).unwrap();
            machine.cpu.write_u32(tls + 4, 0xfeedcafe).unwrap();
            machine.sys.wakes.wake(id, 0, None);
            assert!(machine.sys.terminate_context_stopped(id, sequence));
            assert!(machine.sys.try_acquire(handle));
            retired.push((id, handle, tid, sequence));
        }
        for (id, handle, tid, code) in retired {
            assert!(
                matches!(machine.sys.objects.get(handle), Some(Object::Thread { ctx }) if *ctx == id)
            );
            assert_eq!(machine.sys.context(id).tid, tid);
            assert_eq!(machine.sys.context(id).exit_code(), Some(code));
            assert_eq!(
                machine
                    .sys
                    .thread_identity(id)
                    .unwrap()
                    .completion
                    .exit_code(),
                Some(code)
            );
            assert!(machine.sys.try_acquire(handle));
        }
        let pick = machine
            .sys
            .wakes
            .pick(&mut None, machine.sys.contexts.len(), |_| true, true)
            .unwrap();
        assert_eq!(pick.id, keeper);
        assert!(
            pick.wake.is_none(),
            "terminated contexts leave no wake preference"
        );
    }
}

#[test]
fn concurrent_storage_limit_is_preserved_and_one_finished_slot_can_be_reused() {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Empty,
        Clock::virtual_time(),
    )
    .unwrap();
    for _ in 0..MAX_CONTEXTS {
        machine.spawn("live", 0x400000, &[], 0x10000).unwrap();
    }
    assert!(machine.spawn("overflow", 0x400000, &[], 0x10000).is_err());
    let retired = 17;
    let teb = machine.sys.context(retired).teb;
    assert!(machine.sys.terminate_context_stopped(retired, 37));
    let replacement = machine
        .spawn("replacement", 0x400000, &[], 0x10000)
        .unwrap();
    assert_eq!(replacement, MAX_CONTEXTS);
    assert_eq!(machine.sys.context(replacement).teb, teb);
    assert_eq!(machine.sys.context(retired).exit_code(), Some(37));
}
