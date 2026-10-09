//! Bounded mixed-lifetime heap benchmark: `cargo run -p adv-os --release --example heap_service`.
//! Inputs: `metadata` replaces guest memory with no-op bookkeeping; `large` selects
//! 128 allocate/free pairs of 16 MiB without ZERO_MEMORY, matching the observed hotspot.
//! Output: CSV service latency and time/count/bytes spent in guest memory operations.
//! No guest program, samples, addresses or memory contents are printed.

use adv_cpu::{
    JitCpu,
    backend::{Guest, MemFault, Prot, Reg},
};
use adv_machine::{Vm, layout::PARK_ADDR};
use adv_os::Heaps;
use std::{cell::Cell, hint::black_box, time::Instant};

#[derive(Clone, Copy, Default)]
struct Cost {
    ns: u64,
    calls: u64,
    bytes: u64,
}

impl Cost {
    fn add(&mut self, start: Instant, bytes: usize) {
        self.ns += start.elapsed().as_nanos() as u64;
        self.calls += 1;
        self.bytes += bytes as u64;
    }
}

#[derive(Default)]
struct MemoryCosts {
    read: Cell<Cost>,
    write: Cost,
    map: Cost,
    unmap: Cost,
    protect: Cost,
}

struct MeasuredGuest {
    cpu: Option<JitCpu>,
    costs: MemoryCosts,
}

impl Guest for MeasuredGuest {
    fn reg(&self, reg: Reg) -> u32 {
        self.cpu.as_ref().map_or(0, |cpu| cpu.reg(reg))
    }
    fn set_reg(&mut self, reg: Reg, value: u32) {
        if let Some(cpu) = &mut self.cpu {
            cpu.set_reg(reg, value);
        }
    }
    fn read(&self, addr: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
        let start = Instant::now();
        let result = self
            .cpu
            .as_ref()
            .map_or(Ok(()), |cpu| cpu.read(addr, bytes));
        let mut cost = self.costs.read.get();
        cost.add(start, bytes.len());
        self.costs.read.set(cost);
        result
    }
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
        let start = Instant::now();
        let result = self
            .cpu
            .as_mut()
            .map_or(Ok(()), |cpu| cpu.write(addr, bytes));
        self.costs.write.add(start, bytes.len());
        result
    }
    fn map(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
        let start = Instant::now();
        let result = self
            .cpu
            .as_mut()
            .map_or(Ok(()), |cpu| cpu.map(addr, len, prot));
        self.costs.map.add(start, len as usize);
        result
    }
    fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        let start = Instant::now();
        let result = self.cpu.as_mut().map_or(Ok(()), |cpu| cpu.unmap(addr, len));
        self.costs.unmap.add(start, len as usize);
        result
    }
    fn protect(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
        let start = Instant::now();
        let result = self
            .cpu
            .as_mut()
            .map_or(Ok(()), |cpu| cpu.protect(addr, len, prot));
        self.costs.protect.add(start, len as usize);
        result
    }
}

#[derive(Default)]
struct Service {
    times: Vec<u64>,
}

impl Service {
    fn add(&mut self, start: Instant) {
        self.times.push(start.elapsed().as_nanos() as u64);
    }
    fn print(&mut self, name: &str) {
        self.times.sort_unstable();
        let count = self.times.len();
        if count == 0 {
            return;
        }
        let total: u64 = self.times.iter().sum();
        println!(
            "service,{name},{count},{:.3},{:.3},{:.3},{:.3}",
            total as f64 / 1e6,
            total as f64 / count as f64 / 1e3,
            self.times[(count - 1) * 99 / 100] as f64 / 1e3,
            self.times[count - 1] as f64 / 1e3,
        );
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let metadata = args.iter().any(|arg| arg == "metadata");
    let large_only = args.iter().any(|arg| arg == "large");
    let mut guest = MeasuredGuest {
        cpu: (!metadata).then(|| JitCpu::new(PARK_ADDR, true).unwrap()),
        costs: MemoryCosts::default(),
    };
    let (mut vm, mut heaps) = (Vm::new(), Heaps::default());
    heaps.process_heap = heaps.create(&mut guest, &mut vm).unwrap();
    guest.costs = MemoryCosts::default();
    let (mut small, mut medium, mut large, mut free, mut realloc) = (
        Service::default(),
        Service::default(),
        Service::default(),
        Service::default(),
        Service::default(),
    );
    let sizes = [0, 7, 24, 63, 128, 511, 1024, 4093, 16384, 65535, 0x90000];
    let mut live = vec![None; 4096];
    // Odd multiplier visits every slot; every eighth slot has a long lifetime.
    // A large request is sparse so the benchmark stays below 128 MiB live data.
    for step in 0..if large_only { 128 } else { 40_000 } {
        if large_only {
            let start = Instant::now();
            let addr = heaps
                .alloc(&mut guest, &mut vm, 0, 16 * 1024 * 1024, false)
                .unwrap();
            large.add(start);
            let start = Instant::now();
            assert!(heaps.free(&mut guest, &mut vm, 0, black_box(addr)));
            free.add(start);
            continue;
        }
        let slot = step * 4051 % live.len();
        if slot % 8 == 0 && live[slot].is_some() && step < 32_768 {
            continue;
        }
        if let Some(addr) = live[slot].take() {
            let start = Instant::now();
            assert!(heaps.free(&mut guest, &mut vm, 0, addr));
            free.add(start);
        }
        let size = if step % 251 == 0 {
            sizes[10]
        } else {
            sizes[(step * 17 + step / 4096) % 10]
        };
        let start = Instant::now();
        let mut addr = heaps
            .alloc(&mut guest, &mut vm, 0, size, step % 4 == 0)
            .unwrap();
        if size >= 0x80000 {
            large.add(start);
        } else if size >= 4096 {
            medium.add(start);
        } else {
            small.add(start);
        }
        if step % 127 == 0 {
            let start = Instant::now();
            addr = heaps
                .realloc(&mut guest, &mut vm, 0, addr, size + 31, true, false)
                .unwrap();
            realloc.add(start);
        }
        live[slot] = Some(black_box(addr));
    }
    for addr in live.into_iter().flatten() {
        let start = Instant::now();
        assert!(heaps.free(&mut guest, &mut vm, 0, addr));
        free.add(start);
    }
    println!("backend,{}", if metadata { "metadata" } else { "jit" });
    println!(
        "workload,{}",
        if large_only { "large_16m" } else { "mixed" }
    );
    println!("kind,name,count,total_ms,mean_us,p99_us,max_us");
    for (name, service) in [
        ("alloc_small", &mut small),
        ("alloc_medium", &mut medium),
        ("alloc_large", &mut large),
        ("free", &mut free),
        ("realloc", &mut realloc),
    ] {
        service.print(name);
    }
    println!("kind,name,count,total_ms,bytes");
    for (name, cost) in [
        ("read", guest.costs.read.get()),
        ("write", guest.costs.write),
        ("map", guest.costs.map),
        ("unmap", guest.costs.unmap),
        ("protect", guest.costs.protect),
    ] {
        println!(
            "memory,{name},{},{:.3},{}",
            cost.calls,
            cost.ns as f64 / 1e6,
            cost.bytes
        );
    }
    assert!(heaps.destroy(&mut guest, &mut vm, heaps.process_heap));
}

#[cfg(test)]
mod tests {
    #[test]
    fn schedule_visits_every_slot_and_bounds_large_live_data() {
        let mut visited = [false; 4096];
        for step in 0..4096 {
            visited[step * 4051 % visited.len()] = true;
        }
        assert!(visited.into_iter().all(|value| value));
        assert!(4096 / 251 * 0x90000 < 10 * 1024 * 1024);
    }
}
