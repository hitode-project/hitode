//! Each generation keeps a rolling difference against the prefix it has
//! matched in the preceding frame. A command reads those differences before
//! advancing any of its writes. This makes depth and stencil tests local
//! even when the same command also writes the attachment.
use super::*;

type Changes<'a> = [Option<Progress<'a>>];

pub(super) struct Progress<'a> {
    len: usize,
    cursor: usize,
    old: Option<&'a Sequence>,
    next: &'a Sequence,
    append_final: bool,
    same_start: bool,
    pub(super) region: Region,
    /// Pixels written since the retained frame-start version.
    written: Option<Region>,
}
impl<'a> Progress<'a> {
    pub(super) fn new(h: &'a History, generation: usize, force: bool, append_final: bool) -> Self {
        let next = &h.next[generation];
        let extent = size(next.full);
        let old = h.matched[generation]
            .map(|m| &h.previous[m])
            .filter(|old| !force && h.serial != 0 && old.shape == next.shape);
        let same_start = old.is_some_and(|old| {
            (old.independent && next.independent)
                || (!old.independent && !next.independent && old.initial == next.initial)
        });
        Self {
            len: 0,
            cursor: 0,
            old,
            next,
            append_final,
            same_start,
            region: if old.is_some() {
                Region::empty(extent)
            } else {
                Region::whole(extent)
            },
            written: (generation == 0 && !next.independent).then(|| Region::empty(extent)),
        }
    }

    pub(super) fn advance(&mut self, inputs: Option<&Region>) {
        let next = self.next;
        let (_, bounds) = next.entries[self.len];
        if let Some(written) = &mut self.written {
            written.add(bounds.into());
        }
        if let Some(old) = self.old {
            if let Some(entry) = next.matches[self.len] {
                for &(_, r) in &old.entries[self.cursor..entry.index] {
                    self.region.add(r.into());
                }
                self.cursor = entry.index + 1;
                if entry.inputs {
                    if let Some(inputs) = inputs {
                        self.region.union_clipped(inputs, bounds.into());
                    } else {
                        self.region.add(bounds.into());
                    }
                }
            } else {
                self.region.add(bounds.into());
            }
        }
        self.len += 1;
    }

    pub(super) fn finish(&mut self) {
        if let Some(old) = self.old {
            for &(_, bounds) in &old.entries[self.cursor..] {
                self.region.add(bounds.into());
            }
        }
        // Appends leave everything outside their writes in the retained image.
        // This is a comparison with the previous final version, never a read
        // of a prefix whose previous version could still be transient.
        if let Some(written) = &self.written
            && self.append_final
        {
            self.region.intersect(written);
        }
    }
}

fn project_inputs(
    changes: &Changes<'_>,
    next: &Sequence,
    entry: usize,
    old: &Sequence,
    matched: usize,
    out: &mut Region,
) -> Option<()> {
    let observed = next.observed[entry].as_ref().unwrap();
    let previous = old.observed[matched].as_ref().unwrap();
    let samples = &observed.samples.reads;
    let before = &previous.samples.reads;
    if samples.len() != before.len() {
        return None;
    }
    let bounds = next.entries[entry].1;
    for (i, (read, before)) in samples.iter().zip(before).enumerate() {
        if read.source != before.source {
            return None;
        }
        if read.version == before.version {
            continue;
        }
        let prefix = observed.prefixes[i]?;
        let change = changes[prefix.forward].as_ref()?;
        debug_assert_eq!(prefix.len, change.len);
        let mut old_writes = &[][..];
        let region = if let Some(written) = &change.written
            && change.next.initial == before.version
        {
            written
        } else {
            let old_prefix = previous.prefixes[i]?;
            let old_source = change.old?;
            if !change.same_start || old_source.forward != old_prefix.forward {
                return None;
            }
            // If the aligned cursor is on either side of the observed old
            // prefix, all writes between the two are possible differences.
            let a = change.cursor.min(old_prefix.len);
            let b = change.cursor.max(old_prefix.len);
            old_writes = &old_source.entries[a..b];
            &change.region
        };
        if read.source.2 != 0 {
            out.union_clipped(region, bounds.into());
            for &(_, r) in old_writes {
                out.add(r.clipped(bounds).into());
            }
        } else {
            // The structural key proves the geometry and sampler match. A
            // read-only frame need not have run the coordinate analysis.
            let mappings = if read.mappings.is_empty() {
                &before.mappings
            } else {
                &read.mappings
            };
            if mappings.is_empty() {
                if !region.is_empty() || !old_writes.is_empty() {
                    return None;
                }
                continue;
            }
            for r in rects(region)
                .into_iter()
                .chain(old_writes.iter().map(|&(_, r)| r))
            {
                for mapping in mappings {
                    out.add(mapping.map(r, bounds).into());
                }
            }
        }
    }
    Some(())
}

pub(super) fn changes(
    histories: &HashMap<Content, History>,
    nodes: &[schedule::Node],
    count: usize,
    epoch: u64,
    force: bool,
) -> Vec<Option<Region>> {
    let mut changes: Vec<_> = (0..count).map(|_| None).collect();
    for h in histories.values() {
        for (g, next) in h.next.iter().enumerate() {
            let missed = h.seen + 1 != epoch;
            let append_final = !force
                && (!missed || h.retains_start())
                && h.previous.len() == 1
                && h.previous[0].shape == next.shape;
            changes[next.forward] = Some(Progress::new(h, g, force || missed, append_final));
        }
    }
    let diagnose = trace::capturing();
    let mut scratch: [Region; 3] = std::array::from_fn(|_| Region::default());
    for (at, node) in nodes.iter().enumerate() {
        let writes = node.record.writes.iter().zip(&node.generations);
        // All aspects observe the inputs before this command's first write.
        let mut proven = [false; 3];
        for ((_, g), (proven, region)) in writes.clone().zip(proven.iter_mut().zip(&mut scratch)) {
            let Some(change) = &changes[g.forward] else {
                continue;
            };
            if let Some(old) = change.old
                && let Some(entry) = change.next.matches[change.len].filter(|entry| entry.inputs)
            {
                if region.size() == change.region.size() {
                    region.clear();
                } else {
                    *region = Region::empty(change.region.size());
                }
                *proven =
                    project_inputs(&changes, change.next, change.len, old, entry.index, region)
                        .is_some();
            }
        }
        for ((w, g), (proven, region)) in writes.zip(proven.into_iter().zip(&scratch)) {
            let Some(change) = &mut changes[g.forward] else {
                continue;
            };
            let entry = change.len;
            let before = diagnose.then(|| area(&change.region));
            change.advance(proven.then_some(region));
            if let Some(before) = before {
                let after = area(&change.region);
                if entry == 0 && change.old.is_none() {
                    trace::forward(
                        w.0,
                        g.index,
                        format_args!("node #{at} whole: no comparable generation prefix"),
                    );
                } else if before != after {
                    let reason = match change.next.matches[entry] {
                        Some(m) if m.inputs => {
                            if proven {
                                "mapped inputs"
                            } else {
                                "unproven inputs"
                            }
                        }
                        Some(_) => "skipped old writes",
                        None => "unmatched write",
                    };
                    trace::forward(
                        w.0,
                        g.index,
                        format_args!("node #{at} entry {entry} ({reason}): {before} -> {after} px"),
                    );
                }
            }
        }
    }
    changes
        .into_iter()
        .map(|change| {
            change.map(|mut c| {
                c.finish();
                c.region
            })
        })
        .collect()
}
