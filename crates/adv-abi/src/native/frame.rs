//! Fixed dword frame slots for straight-line integer regions. Invariant loads
//! become explicit entry equalities; temporary loads forward earlier stores.
//! Their complete memory effects remain part of the checked transition.
use std::collections::BTreeMap;

use super::verify::Store;
use super::*;

#[derive(Clone, Debug)]
pub struct FrameInvariant {
    pub offset: u32,
    pub register: Register,
}

#[derive(Clone, Debug)]
pub struct FrameState {
    pub span: MemorySpan,
    pub invariants: Vec<FrameInvariant>,
    pub stores: Vec<Store>,
}

#[derive(Default)]
pub(super) struct Slots {
    invariant: BTreeMap<i32, Register>,
    temporary: BTreeMap<i32, Expression>,
    stores: Vec<Store>,
}

fn unsupported(at: u32, reason: &'static str) -> Rejection {
    Rejection::Unsupported { offset: at, reason }
}

pub(super) fn offset(address: &Expression) -> Option<i32> {
    let (registers, constant) = address.affine()?;
    let mut stack = [0; 8];
    stack[4] = 1;
    (registers == stack).then_some(constant as i32)
}

pub(super) fn address(offset: i32) -> Expression {
    Expression::binary(
        Binary::Add,
        Expression::register(Register(4)),
        Expression::constant(Width::Dword, u64::from(offset as u32)),
    )
    .unwrap()
}

impl Slots {
    fn check_overlap(&self, offset: i32, at: u32) -> Result<(), Rejection> {
        if self
            .invariant
            .keys()
            .chain(self.temporary.keys())
            .any(|&old| old != offset && old.abs_diff(offset) < 4)
        {
            return Err(unsupported(at, "overlapping frame slots"));
        }
        Ok(())
    }

    pub fn version(&self) -> MemoryVersion {
        MemoryVersion(self.stores.len() as u32)
    }

    pub fn read(
        &mut self,
        offset: i32,
        target: Option<Register>,
        at: u32,
    ) -> Result<Expression, Rejection> {
        self.check_overlap(offset, at)?;
        if let Some(value) = self.temporary.get(&offset) {
            return Ok(value.clone());
        }
        let r = match self.invariant.get(&offset) {
            Some(&r) => r,
            None => {
                let r = target.ok_or_else(|| {
                    unsupported(at, "invariant frame load has no stable register mapping")
                })?;
                self.invariant.insert(offset, r);
                r
            }
        };
        Ok(Expression::register(r))
    }

    pub fn write(&mut self, offset: i32, value: Expression, at: u32) -> Result<(), Rejection> {
        self.check_overlap(offset, at)?;
        if self.invariant.contains_key(&offset) {
            return Err(unsupported(
                at,
                "frame slot reads old history before a write",
            ));
        }
        let before = self.version();
        self.stores.push(Store {
            before,
            after: MemoryVersion(before.0 + 1),
            address: address(offset),
            value: value.clone(),
        });
        self.temporary.insert(offset, value);
        Ok(())
    }

    pub fn finish(
        &self,
        values: &[Expression; 8],
        at: u32,
    ) -> Result<Option<FrameState>, Rejection> {
        let Some(&first) = self.invariant.keys().chain(self.temporary.keys()).min() else {
            return Ok(None);
        };
        let last = *self
            .invariant
            .keys()
            .chain(self.temporary.keys())
            .max()
            .unwrap();
        if values[4] != Expression::register(Register(4)) {
            return Err(unsupported(at, "frame base changes across the backedge"));
        }
        let bytes = i64::from(last) - i64::from(first) + 4;
        if bytes > 4096 || self.invariant.len() > 8 {
            return Err(unsupported(at, "frame exceeds the fixed-slot rule"));
        }
        for &r in self.invariant.values() {
            if values[r.index() as usize] != Expression::register(r) {
                return Err(unsupported(
                    at,
                    "frame invariant register changes across the backedge",
                ));
            }
        }
        Ok(Some(FrameState {
            span: MemorySpan {
                address: address(first),
                length: Expression::constant(Width::Dword, bytes as u64),
            },
            invariants: self
                .invariant
                .iter()
                .map(|(&offset, &register)| FrameInvariant {
                    offset: (i64::from(offset) - i64::from(first)) as u32,
                    register,
                })
                .collect(),
            stores: self.stores.clone(),
        }))
    }
}
