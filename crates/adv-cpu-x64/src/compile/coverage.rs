//! Opt-in loop coverage. With `ADV_LOOP_COVERAGE` set, each compilation
//! records every backward branch of its region: the loop either became a
//! kernel (and how its batch analysis ended) or the first reason the loop
//! analysis rejected it. Generated code then counts the loop's iterations on
//! the region's back edge and in its kernel, so a report can weigh each
//! reason by the work it leaves on the slower path.
//!
//! Counters are keyed by guest header and latch and survive recompilation,
//! including tier changes. Without the variable no counter is emitted and
//! generated code is unchanged.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Execution counts of one guest loop, added to by generated code.
#[repr(C)]
#[derive(Default)]
pub(super) struct Counters {
    /// Iterations through the region's back edge.
    pub checked: AtomicU64,
    /// Iterations run in the loop's kernel.
    pub kernel: AtomicU64,
    /// Kernel calls.
    pub calls: AtomicU64,
    /// Back edges whose range proof failed.
    pub failures: AtomicU64,
}

struct Entry {
    region: u32,
    /// Guest instructions from header to latch.
    length: usize,
    /// What became of the loop, the key the summary groups by.
    outcome: String,
    /// Plan properties that the outcome does not distinguish.
    detail: String,
    /// The loop's mnemonics, `.m` marking a memory operand.
    shape: String,
    counters: Arc<Counters>,
}

const SHAPE: usize = 48;

static LOOPS: Mutex<BTreeMap<(u32, u32), Entry>> = Mutex::new(BTreeMap::new());

pub(super) fn enabled() -> bool {
    #[cfg(test)]
    if testing::FORCE.get() {
        return true;
    }
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("ADV_LOOP_COVERAGE").is_some())
}

/// Records the loops of one compiled region and returns the address of each
/// one's counters by header and latch.
pub(super) fn record(
    region: u32,
    instructions: &BTreeMap<u32, Instruction>,
    plans: &[loops::Plan],
    rejected: &[loops::Rejected],
    batches: &[Result<vector::Batch, String>],
) -> BTreeMap<(u32, u32), usize> {
    let length = |header: u32, latch: u32| instructions.range(header..=latch).count();
    let shape = |header: u32, latch: u32| {
        let mut names: Vec<String> = instructions
            .range(header..=latch)
            .take(SHAPE + 1)
            .map(|(_, i)| {
                let memory = (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory);
                let suffix = if memory { ".m" } else { "" };
                format!("{}{suffix}", loops::name(i))
            })
            .collect();
        if names.len() > SHAPE {
            names[SHAPE] = "...".into();
        }
        names.join(" ")
    };
    let mut loops = LOOPS.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut addresses = BTreeMap::new();
    let mut add = |header: u32, latch: u32, outcome: String, detail: String| {
        let entry = loops.entry((header, latch)).or_insert_with(|| Entry {
            region,
            length: 0,
            outcome: String::new(),
            detail: String::new(),
            shape: String::new(),
            counters: Arc::default(),
        });
        entry.region = region;
        entry.length = length(header, latch);
        entry.outcome = outcome;
        entry.detail = detail;
        entry.shape = shape(header, latch);
        addresses.insert((header, latch), Arc::as_ptr(&entry.counters) as usize);
    };
    for (plan, batch) in plans.iter().zip(batches) {
        let outcome = match batch {
            Ok(batch) if batch.copies() => "kernel copy".to_owned(),
            Ok(_) => "kernel batch".to_owned(),
            Err(reason) => format!("kernel scalar: {reason}"),
        };
        // Operands neither proven nor windowed keep their per-access checks.
        let checked = instructions
            .range(plan.header..=plan.latch)
            .filter(|(pc, i)| {
                !plan.proven.contains_key(pc)
                    && !plan.windows.iter().any(|(at, _)| at == *pc)
                    && !matches!(i.mnemonic(), M::Lea | M::Nop)
                    && (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory)
            })
            .count();
        let detail = format!(
            "windows={} checked={checked} slots={}",
            plan.windows.len(),
            plan.slots.len()
        );
        add(plan.header, plan.latch, outcome, detail);
    }
    for loop_ in rejected {
        add(
            loop_.header,
            loop_.latch,
            format!("rejected: {}", loop_.reason),
            String::new(),
        );
    }
    addresses
}

/// The loops recorded so far as tab-separated text, heaviest first: a
/// summary by outcome, then one row per loop. `None` unless enabled.
pub fn report() -> Option<String> {
    if !enabled() {
        return None;
    }
    let loops = LOOPS.lock().unwrap_or_else(|poison| poison.into_inner());
    struct Row<'a> {
        header: u32,
        latch: u32,
        entry: &'a Entry,
        counts: [u64; 4],
        /// Guest instructions the loop ran, approximately.
        work: u64,
    }
    let mut rows: Vec<Row> = loops
        .iter()
        .map(|(&(header, latch), entry)| {
            let c = &entry.counters;
            let counts = [&c.checked, &c.kernel, &c.calls, &c.failures]
                .map(|counter| counter.load(Ordering::Relaxed));
            let work = (counts[0] + counts[1]).saturating_mul(entry.length as u64);
            Row {
                header,
                latch,
                entry,
                counts,
                work,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.work.cmp(&a.work).then(a.header.cmp(&b.header)));
    let total = rows.iter().map(|row| row.work).sum::<u64>().max(1);
    let mut summary: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for row in &rows {
        let group = summary.entry(&row.entry.outcome).or_default();
        group.0 += 1;
        group.1 += row.work;
    }
    let mut summary: Vec<_> = summary.into_iter().collect();
    summary.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(b.0)));
    let mut text = String::from("# outcome\tloops\twork\tshare\n");
    for (outcome, (count, work)) in summary {
        let share = work as f64 * 100.0 / total as f64;
        text.push_str(&format!("{outcome}\t{count}\t{work}\t{share:.2}%\n"));
    }
    text.push_str(
        "\n# region\theader\tlatch\tlength\toutcome\tdetail\tchecked\tkernel\tcalls\tfailures\twork\tshare\tshape\n",
    );
    for row in rows {
        let share = row.work as f64 * 100.0 / total as f64;
        let [checked, kernel, calls, failures] = row.counts;
        text.push_str(&format!(
            "{:#010x}\t{:#010x}\t{:#010x}\t{}\t{}\t{}\t{checked}\t{kernel}\t{calls}\t{failures}\t{}\t{share:.2}%\t{}\n",
            row.entry.region,
            row.header,
            row.latch,
            row.entry.length,
            row.entry.outcome,
            row.entry.detail,
            row.work,
            row.entry.shape,
        ));
    }
    Some(text)
}

impl Emitter<'_> {
    /// Adds `amount` to the counter at `offset` within `counters`.
    pub(super) fn count(&mut self, counters: usize, offset: usize, amount: Value) {
        let address = self.b.ins().iconst(self.ptr_ty, (counters + offset) as i64);
        self.b.ins().atomic_rmw(
            types::I64,
            MemFlags::trusted(),
            ir::AtomicRmwOp::Add,
            address,
            amount,
        );
    }
    pub(super) fn count_one(&mut self, counters: usize, offset: usize) {
        let one = self.b.ins().iconst(types::I64, 1);
        self.count(counters, offset, one);
    }
    /// The counters of loop `index`, when coverage is on.
    pub(super) fn plan_counters(&self, index: usize) -> Option<usize> {
        let plan = &self.plans[index];
        self.coverage.get(&(plan.header, plan.latch)).copied()
    }
}

#[cfg(test)]
pub(super) mod testing {
    use std::cell::Cell;
    thread_local! {
        pub(in crate::compile) static FORCE: Cell<bool> = const { Cell::new(false) };
    }
}
