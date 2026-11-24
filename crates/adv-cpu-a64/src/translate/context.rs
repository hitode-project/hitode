//! The guest unit each thread is translating, so a panic report names the
//! guest code that reached a translator failure.
use crate::translate::front::Unit;
use std::cell::Cell;
use std::marker::PhantomData;

#[derive(Clone, Copy)]
struct Current {
    unit: *const Unit,
    eip: u32,
    stage: &'static str,
}

thread_local! {
    static CURRENT: Cell<Option<Current>> = const { Cell::new(None) };
}

/// Marks `unit` as this thread's translation until dropped.
pub(crate) struct Translating<'a> {
    previous: Option<Current>,
    unit: PhantomData<&'a Unit>,
}

impl<'a> Translating<'a> {
    pub(crate) fn new(unit: &'a Unit, eip: u32, stage: &'static str) -> Self {
        let previous = CURRENT.replace(Some(Current { unit, eip, stage }));
        Self {
            previous,
            unit: PhantomData,
        }
    }
}

impl Drop for Translating<'_> {
    fn drop(&mut self) {
        CURRENT.set(self.previous);
    }
}

/// The unit this thread is translating, with its instructions, or `None`
/// outside translation. A panic hook runs before unwinding, while the unit
/// is still borrowed.
pub fn panic_context() -> Option<String> {
    let current = CURRENT.get()?;
    // SAFETY: `Translating` borrows the unit for as long as it is current.
    let unit = unsafe { &*current.unit };
    let insts = &unit.trace.insts;
    let mut text = format!(
        "{} of the unit at {:#010x}: {} instructions from {:#010x} to {:#010x}\n",
        current.stage,
        current.eip,
        insts.len(),
        insts.first().map_or(0, |inst| inst.pc),
        insts.last().map_or(0, |inst| inst.pc),
    );
    for inst in insts {
        text += &format!(
            "  {:#010x}..{:#010x} {:?} next {:?} taken {:?}\n",
            inst.pc, inst.next_pc, inst.op, inst.next, inst.taken
        );
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-UI-11
    #[test]
    fn a_translation_names_its_unit_until_it_ends() {
        let unit = Unit::default();
        assert_eq!(panic_context(), None);
        {
            let _outer = Translating::new(&unit, 0x1234, "lowering");
            {
                let _inner = Translating::new(&unit, 0x5678, "optimization");
                assert!(
                    panic_context()
                        .unwrap()
                        .starts_with("optimization of the unit at 0x00005678")
                );
            }
            assert!(
                panic_context()
                    .unwrap()
                    .starts_with("lowering of the unit at 0x00001234")
            );
        }
        assert_eq!(panic_context(), None);
    }
}
