//! Bounded pure recovery DAGs and parameterized cold-exit sharing.
use super::{ir::*, verify::Error};
use std::collections::BTreeMap;
pub const MAX_NODES: usize = 128;
pub const MAX_DEPTH: usize = 24;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Capture,
    Pure(ValueKind),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub value: ValueId,
    pub source: Source,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dag {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}
impl Dag {
    pub fn build(r: &Region, state: &StateMap) -> Self {
        fn add(r: &Region, dag: &mut Dag, value: ValueId, depth: usize) -> usize {
            if let Some(n) = dag.nodes.iter().position(|n| n.value == value) {
                return n;
            }
            let checkpoint = dag.nodes.len();
            let mut source = Source::Capture;
            // GPR arithmetic is already available. Flags remain recipes until
            // the cold block; their ordinary operands are captured SSA values.
            if depth < MAX_DEPTH - 1
                && dag.nodes.len() < MAX_NODES - 16
                && matches!(
                    r.values[value.0].kind,
                    ValueKind::Flags(_)
                        | ValueKind::Pure(Pure::FlagPack { .. })
                        | ValueKind::Constant(_)
                )
            {
                let mut kind = r.values[value.0].kind.clone();
                super::state_ssa::kind(&mut kind, &mut |v| {
                    *v = ValueId(add(r, dag, *v, depth + 1))
                });
                if dag.nodes.len() < MAX_NODES - 10 {
                    source = Source::Pure(kind);
                } else {
                    dag.nodes.truncate(checkpoint);
                }
            }
            let n = dag.nodes.len();
            dag.nodes.push(Node { value, source });
            n
        }
        let mut dag = Self {
            nodes: vec![],
            roots: vec![],
        };
        for value in state.values() {
            let n = add(r, &mut dag, value, 0);
            dag.roots.push(n);
        }
        dag
    }
    pub fn captures(&self) -> Vec<ValueId> {
        self.nodes
            .iter()
            .filter(|n| n.source == Source::Capture)
            .map(|n| n.value)
            .collect()
    }
    pub fn verify(&self, r: &Region, state: &StateMap) -> Result<(), Error> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES || self.roots.len() != 9 {
            return Err(Error::region("recovery DAG node bound or roots"));
        }
        let mut depths = vec![1; self.nodes.len()];
        for (n, node) in self.nodes.iter().enumerate() {
            let value = r
                .values
                .get(node.value.0)
                .ok_or_else(|| Error::region("recovery DAG value"))?;
            if !value.width.integer() {
                return Err(Error::region("recovery DAG capture type"));
            }
            if let Source::Pure(kind) = &node.source {
                if !matches!(
                    kind,
                    ValueKind::Constant(_) | ValueKind::Pure(_) | ValueKind::Flags(_)
                ) {
                    return Err(Error::region("recovery DAG must not reread memory"));
                }
                for input in kind.inputs() {
                    if input.0 >= n {
                        return Err(Error::region("recovery DAG topological order"));
                    }
                    depths[n] = depths[n].max(depths[input.0] + 1);
                }
                let mut restored = kind.clone();
                super::state_ssa::kind(&mut restored, &mut |v| *v = self.nodes[v.0].value);
                if restored != value.kind {
                    return Err(Error::region("recovery DAG does not reconstruct snapshot"));
                }
            }
            if depths[n] > MAX_DEPTH {
                return Err(Error::region("recovery DAG depth bound"));
            }
        }
        for (root, expected) in self.roots.iter().zip(state.values()) {
            if self.nodes.get(*root).is_none_or(|n| n.value != expected) {
                return Err(Error::region("recovery DAG root state"));
            }
        }
        Ok(())
    }
    pub fn key(&self, r: &Region) -> String {
        // Local node numbers encode the DAG, while capture identities are
        // intentionally absent. Different captured values become parameters.
        format!(
            "{:?}/{:?}",
            self.nodes
                .iter()
                .map(|n| (r.values[n.value.0].width, &n.source))
                .collect::<Vec<_>>(),
            self.roots
        )
    }
}
#[derive(Clone, Copy)]
pub enum Sharing {
    Compatible,
    Dedicated,
}
pub struct Site {
    pub dag: Dag,
    pub group: usize,
    pub arguments: Vec<ValueId>,
}
pub struct Plan {
    pub sites: Vec<Site>,
    pub groups: usize,
}
impl Plan {
    pub fn build(r: &Region, sharing: Sharing) -> Self {
        let mut keys = BTreeMap::new();
        let mut sites = vec![];
        for dag in &r.recovery {
            let dag = dag.clone();
            let mut key = dag.key(r);
            if matches!(sharing, Sharing::Dedicated) {
                key.push_str(&format!("/{}", sites.len()));
            }
            let next = keys.len();
            let group = *keys.entry(key).or_insert(next);
            let arguments = dag.captures();
            sites.push(Site {
                dag,
                group,
                arguments,
            });
        }
        Self {
            sites,
            groups: keys.len(),
        }
    }
    pub fn verify(&self, r: &Region) -> Result<(), Error> {
        if self.sites.len() != r.snapshots.len() {
            return Err(Error::region("missing cold snapshot"));
        }
        let mut shapes = BTreeMap::new();
        for (site, s) in self.sites.iter().zip(&r.snapshots) {
            site.dag.verify(r, &s.state)?;
            if site.arguments != site.dag.captures() || site.group >= self.groups {
                return Err(Error::region("shared exit parameter completeness"));
            }
            let key = site.dag.key(r);
            if shapes
                .insert(site.group, key.clone())
                .is_some_and(|old| old != key)
            {
                return Err(Error::region("incompatible shared exit snapshots"));
            }
        }
        Ok(())
    }
}
