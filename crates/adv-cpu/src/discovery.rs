//! Bounded control-flow discovery from an observed hot guest entry.
use std::collections::BTreeSet;

use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind};

use crate::cliffs::InterpreterDetail;
use crate::{CompileStrategy, Memory, Region};

const MAX_BYTES: usize = 16 * 1024;
const MAX_INSTRUCTIONS: usize = 256;

pub(crate) fn discover(
    memory: &mut Memory,
    entry: u32,
    stop: u32,
    strategy: CompileStrategy,
    boundaries: &BTreeSet<u32>,
) -> Result<Region, InterpreterDetail> {
    let mut pending = vec![entry];
    let mut instructions = BTreeSet::new();
    let mut frontier = BTreeSet::new();
    let mut start = entry;
    let mut end = u64::from(entry);
    let mut rejection = InterpreterDetail::DiscoveryBoundary;
    while let Some(mut pc) = pending.pop() {
        loop {
            if instructions.contains(&pc) {
                break;
            }
            // An already compiled entry, an in-flight entry, a caller stop,
            // or an exhausted discovery budget remains a real guest transfer.
            if pc == stop || boundaries.contains(&pc) || instructions.len() == MAX_INSTRUCTIONS {
                frontier.insert(pc);
                break;
            }
            let mut bytes = [0; 15];
            let mut available = 0;
            for offset in 0..15u32 {
                let Some(address) = pc.checked_add(offset) else {
                    break;
                };
                if !memory.executable(address, 1) {
                    break;
                }
                let Ok(byte) = memory.read_data(address, 1) else {
                    break;
                };
                bytes[offset as usize] = byte[0];
                available += 1;
            }
            if available == 0 {
                rejection = InterpreterDetail::DiscoveryMemory;
                frontier.insert(pc);
                break;
            }
            let instruction =
                Decoder::with_ip(32, &bytes[..available], u64::from(pc), DecoderOptions::NONE)
                    .decode();
            memory.watch_code(
                pc,
                if instruction.is_invalid() {
                    available
                } else {
                    instruction.len()
                },
            );
            let candidate_start = start.min(pc);
            let candidate_end = end.max(u64::from(pc) + instruction.len() as u64);
            let len = (candidate_end - u64::from(candidate_start)) as usize;
            if len > MAX_BYTES
                || (u64::from(stop) >= u64::from(candidate_start)
                    && u64::from(stop) < candidate_end)
                || !memory.executable(candidate_start, len)
            {
                rejection = if len > MAX_BYTES {
                    InterpreterDetail::DiscoverySize
                } else if u64::from(stop) >= u64::from(candidate_start)
                    && u64::from(stop) < candidate_end
                {
                    InterpreterDetail::DiscoveryBoundary
                } else {
                    InterpreterDetail::DiscoveryMemory
                };
                frontier.insert(pc);
                break;
            }
            start = candidate_start;
            end = candidate_end;
            instructions.insert(pc);
            if instruction.is_invalid() {
                break;
            }
            let next = instruction.next_ip32();
            match instruction.flow_control() {
                FlowControl::ConditionalBranch => {
                    if instruction.op0_kind() == OpKind::NearBranch32 {
                        pending.push(instruction.near_branch32());
                    }
                }
                FlowControl::UnconditionalBranch => {
                    if instruction.op0_kind() == OpKind::NearBranch32 {
                        pending.push(instruction.near_branch32());
                    }
                    break;
                }
                // Keep callees as independent compilation units. A call back
                // into this graph remains internal if another edge reaches it.
                FlowControl::Call => {
                    if instruction.op0_kind() == OpKind::NearBranch32 {
                        frontier.insert(instruction.near_branch32());
                    }
                }
                FlowControl::IndirectBranch | FlowControl::Return | FlowControl::Exception => break,
                _ => {}
            }
            pc = next;
        }
    }
    if instructions.is_empty() {
        return Err(rejection);
    }
    frontier.retain(|pc| !instructions.contains(pc));
    let mut region = Region::new(start, (end - u64::from(start)) as usize);
    region.entries.push(entry);
    region.frontier = frontier.into_iter().collect();
    region.reachable = true;
    region.complete_tail = true;
    region.compile_strategy = strategy;
    Ok(region)
}
