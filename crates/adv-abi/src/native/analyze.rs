//! Budgeted candidate discovery over immutable executable snapshots.
//! Proposals are deliberately untrusted: this module never calls the verifier.

use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};

use super::*;

/// Find direct backward conditional edges, then infer pointer/counter roles
/// from the enclosed operations. No version strings, addresses or code hashes
/// select a computation, and candidates cannot be published for execution.
pub fn discover(image: &ImageSnapshot, budget: &mut Budget) -> Vec<CandidateBinding> {
    let mut candidates = Vec::new();
    for dependency in image
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Code)
    {
        let mut decoder = Decoder::with_ip(
            32,
            &dependency.bytes,
            u64::from(dependency.offset),
            DecoderOptions::NONE,
        );
        while decoder.can_decode() && budget.instructions != 0 {
            budget.instructions -= 1;
            let i = decoder.decode();
            if i.mnemonic() != Mnemonic::Jne || i.near_branch_target() >= i.ip() {
                continue;
            }
            let start = i.near_branch_target() as u32;
            let end = i.next_ip32();
            let Some(bytes) = image.bytes(start, end.wrapping_sub(start)) else {
                continue;
            };
            let mut body = Decoder::with_ip(32, bytes, u64::from(start), DecoderOptions::NONE);
            let (mut destination, mut remaining) = (None, None);
            let mut sources = Vec::new();
            let values = std::array::from_fn(|r| Expression::register(Register(r as u8)));
            let mut increments = [false; 8];
            let mut adds = false;
            let mut halves = false;
            while body.can_decode() && budget.instructions != 0 {
                budget.instructions -= 1;
                let step = body.decode();
                if step.op1_kind() == OpKind::Memory
                    && matches!(step.mnemonic(), Mnemonic::Mov | Mnemonic::Movzx)
                    && verify::plain_memory(&step)
                    && let Some(source) = verify::address(&step, &values)
                {
                    sources.push(source);
                }
                halves |= step.mnemonic() == Mnemonic::Shr;
                if step.op0_kind() == OpKind::Memory
                    && matches!(step.mnemonic(), Mnemonic::Mov | Mnemonic::Add)
                    && verify::plain_memory(&step)
                {
                    destination = verify::address(&step, &values);
                    adds = step.mnemonic() == Mnemonic::Add;
                }
                if step.mnemonic() == Mnemonic::Dec && step.op0_kind() == OpKind::Register {
                    remaining = verify::register(step.op0_register()).map(Expression::register);
                }
                if step.mnemonic() == Mnemonic::Inc
                    && let Some(r) = verify::register(step.op0_register())
                {
                    increments[r.index() as usize] = true;
                }
                if step.mnemonic() == Mnemonic::Cmp
                    && let (Some(a), Some(b)) = (
                        verify::register(step.op0_register()),
                        verify::register(step.op1_register()),
                    )
                {
                    let pair = match (
                        increments[a.index() as usize],
                        increments[b.index() as usize],
                    ) {
                        (true, false) => Some((b, a)),
                        (false, true) => Some((a, b)),
                        _ => None,
                    };
                    if let Some((bound, index)) = pair {
                        remaining = Expression::binary(
                            Binary::Subtract,
                            Expression::register(bound),
                            Expression::register(index),
                        );
                    }
                }
            }
            let (Some(destination), Some(remaining)) = (destination, remaining) else {
                continue;
            };
            if budget.instructions == 0 && body.can_decode() {
                continue;
            }
            let mut proposals = Vec::new();
            if adds && halves && sources.len() == 2 {
                for (previous, left) in [(0, 1), (1, 0)] {
                    if let Some(view) = row_view(
                        Computation::PngAverage,
                        sources[previous].clone(),
                        sources[left].clone(),
                        destination.clone(),
                        remaining.clone(),
                    ) {
                        proposals.push((Computation::PngAverage, view));
                    }
                }
            } else if let [source] = sources.as_slice() {
                let computations: &[Computation] = if adds {
                    &[Computation::PngSub, Computation::PngUp]
                } else {
                    &[Computation::Lz77]
                };
                for &computation in computations {
                    if let Some(view) = byte_view(
                        computation,
                        source.clone(),
                        destination.clone(),
                        remaining.clone(),
                    ) {
                        proposals.push((computation, view));
                    }
                }
            }
            for (computation, view) in proposals {
                candidates.push(CandidateBinding {
                    image: image.identity,
                    contract: ReferenceContract::get(computation),
                    entry: start,
                    end,
                    view,
                    cutpoints: vec![start],
                    exits: vec![end],
                    proof: ProofHint::ForwardBytes {
                        remaining: remaining.clone(),
                    },
                });
            }
        }
    }
    candidates
}
