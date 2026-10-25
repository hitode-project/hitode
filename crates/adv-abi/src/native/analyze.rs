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
            let (mut source, mut destination, mut remaining) = (None, None, None);
            let mut adds = false;
            while body.can_decode() && budget.instructions != 0 {
                budget.instructions -= 1;
                let step = body.decode();
                if step.op1_kind() == OpKind::Memory
                    && matches!(step.mnemonic(), Mnemonic::Mov | Mnemonic::Movzx)
                    && verify::plain_memory(&step)
                {
                    source = verify::register(step.memory_base())
                        .map(|r| (r, step.memory_displacement32()));
                }
                if step.op0_kind() == OpKind::Memory
                    && matches!(step.mnemonic(), Mnemonic::Mov | Mnemonic::Add)
                    && verify::plain_memory(&step)
                {
                    destination = verify::register(step.memory_base())
                        .map(|r| (r, step.memory_displacement32()));
                    adds = step.mnemonic() == Mnemonic::Add;
                }
                if step.mnemonic() == Mnemonic::Dec && step.op0_kind() == OpKind::Register {
                    remaining = verify::register(step.op0_register());
                }
            }
            let (
                Some((source, source_offset)),
                Some((destination, destination_offset)),
                Some(remaining),
            ) = (source, destination, remaining)
            else {
                continue;
            };
            if budget.instructions == 0 && body.can_decode() {
                continue;
            }
            let computations: &[Computation] = if adds {
                &[Computation::PngSub, Computation::PngUp]
            } else {
                &[Computation::Lz77]
            };
            for &computation in computations {
                let source = Expression::binary(
                    Binary::Add,
                    Expression::register(source),
                    Expression::constant(Width::Dword, u64::from(source_offset)),
                )
                .expect("32-bit address");
                let destination = Expression::binary(
                    Binary::Add,
                    Expression::register(destination),
                    Expression::constant(Width::Dword, u64::from(destination_offset)),
                )
                .expect("32-bit address");
                let remaining = Expression::register(remaining);
                let Some(view) = byte_view(computation, source, destination, remaining.clone())
                else {
                    continue;
                };
                candidates.push(CandidateBinding {
                    image: image.identity,
                    contract: ReferenceContract::get(computation),
                    entry: start,
                    end,
                    view,
                    cutpoints: vec![start],
                    exits: vec![end],
                    proof: ProofHint::ForwardBytes { remaining },
                });
            }
        }
    }
    candidates
}
