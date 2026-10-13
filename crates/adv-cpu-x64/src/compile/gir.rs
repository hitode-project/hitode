//! Production GIR lowering through the shared native state and memory contract.
use super::*;
use crate::gir::{ir as g, lift::Tier2Operation};

/// An SSA run borrows native state variables. Its opaque neighbors use these
/// variables directly, so no extra CpuState publication or reload is needed.
struct Lower<'a, 'b> {
    e: &'a mut Emitter<'b>,
    region: &'a g::Region,
    values: Vec<Option<Value>>,
    flags: g::ValueId,
}

impl Lower<'_, '_> {
    fn get(&mut self, id: g::ValueId) -> Value {
        if let Some(value) = self.values[id.0] {
            return value;
        }
        let node = &self.region.values[id.0];
        let value = match &node.kind {
            g::ValueKind::Constant(n) => self.e.constant(*n as u32),
            g::ValueKind::Pure(pure) => match *pure {
                g::Pure::Binary(op, a, b) => {
                    let a = self.get(a);
                    let b = self.get(b);
                    match op {
                        g::Binary::Add => self.e.b.ins().iadd(a, b),
                        g::Binary::Sub => self.e.b.ins().isub(a, b),
                        g::Binary::And => self.e.b.ins().band(a, b),
                        g::Binary::Or => self.e.b.ins().bor(a, b),
                        g::Binary::Xor => self.e.b.ins().bxor(a, b),
                        g::Binary::Mul => self.e.b.ins().imul(a, b),
                        _ => unreachable!("non-native integer operation"),
                    }
                }
                g::Pure::Extract { value, lsb } => {
                    let value = self.get(value);
                    if lsb == 0 {
                        value
                    } else {
                        self.e.b.ins().ushr_imm(value, i64::from(lsb))
                    }
                }
                g::Pure::Extend { value, signed } => {
                    let input = self.get(value);
                    if signed {
                        let width = self.region.values[value.0].width.bits();
                        let narrow = self
                            .e
                            .b
                            .ins()
                            .ireduce(integer_type(width as usize / 8), input);
                        self.e.b.ins().sextend(types::I32, narrow)
                    } else {
                        input
                    }
                }
                _ => unreachable!("non-native pure operation"),
            },
            _ => unreachable!("native GIR value has no definition"),
        };
        let value = match node.width {
            g::Width::B8 | g::Width::B16 => {
                self.e.b.ins().band_imm(value, node.width.mask() as i64)
            }
            _ => value,
        };
        self.values[id.0] = Some(value);
        value
    }

    fn commit(&mut self, state: &g::StateMap) {
        for (index, id) in state.gpr.iter().enumerate() {
            let value = self.get(*id);
            self.e.b.def_var(self.e.regs[index], value);
        }
        if state.flags == self.flags {
            return;
        }
        let g::ValueKind::Flags(flags) = &self.region.values[state.flags.0].kind else {
            unreachable!("native GIR flags are an arithmetic recipe")
        };
        let a = self.get(flags.lhs);
        let b = self.get(flags.rhs);
        let result = self.get(flags.result);
        self.e.flags(
            a,
            b,
            result,
            flags.kind == g::FlagKind::Sub,
            flags.kind == g::FlagKind::Logic,
            flags.defined & g::CF == 0,
        );
        self.flags = state.flags;
    }

    fn memory(&mut self, id: g::MemoryId) {
        let memory = &self.region.memory[id.0];
        let address = self.get(memory.address);
        let ty = integer_type(memory.width.bits() as usize / 8);
        match memory.access {
            g::Access::Read => {
                let value = self.e.direct_load(address, ty);
                let value = if ty == types::I32 {
                    value
                } else {
                    self.e.b.ins().uextend(types::I32, value)
                };
                self.values[memory.result.unwrap().0] = Some(value);
            }
            g::Access::Write {
                value,
                enabled: None,
            } => {
                let value = self.get(value);
                self.e.direct_store(address, value);
            }
            _ => unreachable!("conditional stores remain opaque"),
        }
    }
}

impl Emitter<'_> {
    pub(super) fn gir_operation(&mut self, operation: &Tier2Operation) -> (bool, u32) {
        let Tier2Operation::Ssa {
            region,
            instructions,
        } = operation
        else {
            let Tier2Operation::Opaque(instruction) = operation else {
                unreachable!()
            };
            self.precheck(instruction);
            return (self.instruction(instruction), instruction.next_ip32());
        };
        debug_assert_eq!(region.blocks.len(), 1);
        let block = &region.blocks[region.entry.0];
        let mut values = vec![None; region.values.len()];
        for (index, &id) in block.entry_state.gpr.iter().enumerate() {
            values[id.0] = Some(self.b.use_var(self.regs[index]));
        }
        let mut lower = Lower {
            e: self,
            region,
            values,
            flags: block.entry_state.flags,
        };
        let mut next_instruction = instructions.iter();
        let mut current = next_instruction.next().unwrap();
        lower.e.at(current.ip32());
        lower.e.precheck(current);
        for operation in &block.operations {
            match operation {
                g::Operation::Define(_) => {}
                g::Operation::Memory(id) => lower.memory(*id),
                g::Operation::Commit { state, .. } => {
                    lower.commit(state);
                    if let Some(instruction) = next_instruction.next() {
                        current = instruction;
                        lower.e.at(current.ip32());
                        lower.e.precheck(current);
                    }
                }
            }
        }
        (false, current.next_ip32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;
    use std::sync::atomic::AtomicBool;

    const CODE: u32 = 0x10000;
    const DATA: u32 = 0x20000;

    fn memory(bytes: &[u8]) -> Memory {
        let mut memory = Memory::new(CODE, 0x20000).unwrap();
        memory.protect(CODE, 0x20000, READ | WRITE).unwrap();
        memory.write(CODE, bytes).unwrap();
        memory.protect(CODE, 4096, READ | EXEC).unwrap();
        memory
    }

    fn native(snapshot: &Snapshot, memory: &mut Memory, state: &mut CpuState) -> (Compiled, u32) {
        let compiled = compile_snapshot(snapshot).unwrap();
        let mut bridge = crate::trap::Bridge {
            timestamp: Default::default(),
            memory,
            error: None,
            trap: None,
            calls: 0,
            handler: None,
            parked: false,
            deadline: None,
            clock_due: true,
            unclocked_bytes: 0,
            bulk_calls: 0,
            bulk_bytes: 0,
            cliffs: Default::default(),
            scratch: Default::default(),
        };
        let interrupt = AtomicBool::new(false);
        let exit = compiled.run(
            state,
            memory,
            &mut bridge,
            NativeRun {
                rows: None,
                links: None,
                stop: 0,
                interrupt: &interrupt,
            },
        );
        (compiled, exit)
    }

    fn compare(bytes: &[u8], strategy: CompileStrategy, fault: bool) {
        compare_at(bytes, strategy, fault, &[], CODE);
    }

    fn compare_at(
        bytes: &[u8],
        strategy: CompileStrategy,
        fault: bool,
        entries: &[u32],
        entry: u32,
    ) {
        let mut actual_memory = memory(bytes);
        let mut oracle_memory = memory(bytes);
        if fault {
            actual_memory.protect(DATA, 4096, READ).unwrap();
            oracle_memory.protect(DATA, 4096, READ).unwrap();
        }
        let mut region = crate::Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        region.entries.extend_from_slice(entries);
        let snapshot = Snapshot::capture(&region, &mut actual_memory).unwrap();
        let mut actual = CpuState {
            eip: entry,
            eflags: 0xad7,
            regs: [
                11,
                13,
                17,
                19,
                DATA + 2048,
                DATA + 256,
                DATA + 128,
                DATA + 64,
            ],
            fuel: 100,
            ..Default::default()
        };
        let mut oracle = actual.clone();
        let (compiled, exit) = native(&snapshot, &mut actual_memory, &mut actual);
        assert!(compiled.gir_ssa_instructions > 0);
        assert_eq!(
            compiled.gir_ssa_instructions + compiled.gir_opaque_instructions,
            compiled.instruction_count
        );
        let mut interpreter = crate::interpreter::Interpreter::new();
        let stop = CODE + bytes.len() as u32;
        for _ in 0..100 {
            if oracle.eip == stop {
                break;
            }
            match interpreter.step(&mut oracle, &mut oracle_memory) {
                crate::interpreter::Step::Executed => {}
                crate::interpreter::Step::WriteFault { pc, address } if fault => {
                    assert_eq!(pc, actual.eip);
                    assert_eq!(address, actual.fault_addr);
                    break;
                }
                step => panic!("unexpected interpreter outcome {step:?}"),
            }
        }
        assert_eq!(
            exit,
            if fault {
                Exit::WriteFault
            } else {
                Exit::Dispatch
            } as u32
        );
        oracle.fuel = actual.fuel;
        oracle.fault_addr = actual.fault_addr;
        assert_eq!(actual, oracle);
        assert_eq!(
            actual_memory.read(DATA, 4096).unwrap(),
            oracle_memory.read(DATA, 4096).unwrap()
        );
    }

    // spec: EXEC-GIR-3
    #[test]
    fn ssa_runs_bridge_opaque_simd_x87_and_partial_registers() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, 0xffff_ffffu32).unwrap();
        a.add(eax, 1).unwrap();
        a.mov(ecx, eax).unwrap();
        a.pxor(xmm1, xmm1).unwrap();
        a.mov(ah, 0x42).unwrap();
        a.inc(ecx).unwrap();
        a.sbb(edx, 0).unwrap();
        a.fld1().unwrap();
        a.fld1().unwrap();
        a.faddp(st1, st0).unwrap();
        a.mov(eax, ecx).unwrap();
        a.add(eax, 3).unwrap();
        a.mov(dword_ptr(DATA), eax).unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            compare(&bytes, strategy, false);
        }
    }

    // spec: EXEC-GIR-4
    #[test]
    fn gir_memory_fault_keeps_the_committed_integer_and_flag_prefix() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, 0xffff_ffffu32).unwrap();
        a.add(eax, 1).unwrap();
        a.inc(eax).unwrap();
        a.add(dword_ptr(DATA), eax).unwrap();
        a.mov(ecx, 71).unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            compare(&bytes, strategy, true);
        }
    }

    // spec: EXEC-GIR-2
    #[test]
    fn side_entries_split_integer_ssa_without_losing_opaque_coverage() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, 1).unwrap();
        a.add(eax, 2).unwrap();
        a.pxor(xmm0, xmm0).unwrap();
        a.inc(eax).unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        let decoded: BTreeMap<_, _> =
            Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .map(|i| (i.ip32(), i))
                .collect();
        let entry = decoded.values().nth(1).unwrap().ip32();
        let plan =
            crate::gir::lift::tier2(&decoded, &BTreeSet::from([entry]), &BTreeSet::new()).unwrap();
        assert_eq!(plan.ssa_instructions, 3);
        assert_eq!(plan.opaque_instructions, 1);
        assert!(plan.operations.contains_key(&entry));
        assert_eq!(plan.operations.len(), 4);
    }

    // spec: EXEC-GIR-2, EXEC-GIR-5
    #[test]
    fn overlapping_decode_entries_share_an_integer_suffix() {
        let mut a = CodeAssembler::new(32).unwrap();
        // The alternate entry interprets the MOV immediate as four NOPs;
        // both instruction streams then reach the same integer suffix.
        a.mov(eax, 0x9090_9090u32).unwrap();
        a.inc(eax).unwrap();
        a.inc(ecx).unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for entry in [CODE, CODE + 1] {
                compare_at(&bytes, strategy, false, &[CODE + 1], entry);
            }
        }
    }
}
