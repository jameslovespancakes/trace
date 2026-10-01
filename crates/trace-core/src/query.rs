//! Bounded deterministic graph queries.
//!
//! Static reachability is not feasibility: a path is evidence that one symbol *can* reach
//! another under the chosen tier; no path in an incomplete graph proves nothing. Every
//! result carries which bounds were hit and whether the search completed.
//!
//! Bounds (all enforced, all reported):
//! * depth — maximum path length in edges; hit when an unseen node lies beyond it;
//! * work  — maximum in-view edges examined;
//! * paths — maximum simple paths returned by [`Graph::all_paths`];
//! * time  — wall-clock limit (checked per popped node and every 256 edges).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::graph::Graph;
use crate::model::{EdgeId, SymbolId, Tier};

/// Traversal limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Maximum path length in edges.
    pub max_depth: u32,
    /// Maximum edges examined.
    pub max_work: u64,
    /// Maximum paths returned by [`Graph::all_paths`].
    pub max_paths: usize,
    /// Wall-clock limit.
    pub timeout: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds {
            max_depth: 64,
            max_work: 100_000,
            max_paths: 100,
            timeout: Duration::from_secs(2),
        }
    }
}

impl Bounds {
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    fn validate(&self) -> Result<()> {
        if self.max_work == 0 || self.max_paths == 0 || self.timeout.is_zero() {
            return Err(CoreError::InvalidBounds("work, paths and timeout must be positive".into()));
        }
        Ok(())
    }
}

/// Which bounds stopped a traversal early.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BoundsHit {
    pub depth: bool,
    pub work: bool,
    pub paths: bool,
    pub time: bool,
}

impl BoundsHit {
    pub fn any(&self) -> bool {
        self.depth || self.work || self.paths || self.time
    }
    /// Names of hit bounds, e.g. `["depth", "time"]`.
    pub fn names(&self) -> Vec<&'static str> {
        [
            (self.depth, "depth"),
            (self.work, "work"),
            (self.paths, "paths"),
            (self.time, "time"),
        ]
        .into_iter()
        .filter_map(|(hit, name)| hit.then_some(name))
        .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Follow outgoing edges (what a symbol executes).
    Forward,
    /// Follow incoming edges (what may be affected).
    Reverse,
}

/// One path: `nodes.len() == edges.len() + 1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathResult {
    pub nodes: Vec<SymbolId>,
    pub edges: Vec<EdgeId>,
}

/// Result of a bounded traversal.
#[derive(Clone, Debug)]
pub struct Reach {
    /// Start symbol (the first start for [`Graph::reach_many`]).
    pub start: SymbolId,
    pub target: Option<SymbolId>,
    pub direction: Direction,
    pub include: Tier,
    /// Reached nodes with their BFS distance, in discovery order (starts first, distance 0).
    /// For path queries: the union of nodes on returned paths (minimum position on a path).
    pub nodes: Vec<(SymbolId, u32)>,
    /// Traversed edges (reach) or union of path edges (path queries), ascending id.
    pub edges: Vec<EdgeId>,
    pub paths: Vec<PathResult>,
    pub hit: BoundsHit,
    pub work: u64,
    pub elapsed: Duration,
}

impl Reach {
    /// True when no bound stopped the search (graph search complete for this tier).
    pub fn complete(&self) -> bool {
        !self.hit.any()
    }
    pub fn found(&self) -> bool {
        !self.paths.is_empty()
    }
}

const NOT_SEEN: u32 = u32::MAX;
const NO_PARENT: u32 = u32::MAX;
const CLOCK_EVERY: u64 = 256;

/// Shared work/time accounting for one traversal.
struct Meter<'b> {
    bounds: &'b Bounds,
    begun: Instant,
    work: u64,
    hit: BoundsHit,
}

impl<'b> Meter<'b> {
    fn new(bounds: &'b Bounds) -> Self {
        Meter {
            bounds,
            begun: Instant::now(),
            work: 0,
            hit: BoundsHit::default(),
        }
    }

    /// Per popped node: false (and `time` recorded) when out of time.
    fn node_ok(&mut self) -> bool {
        if self.begun.elapsed() >= self.bounds.timeout {
            self.hit.time = true;
            return false;
        }
        true
    }

    /// Per examined edge: false (and `work`/`time` recorded) when a bound stops the search.
    fn edge_ok(&mut self) -> bool {
        if self.work >= self.bounds.max_work {
            self.hit.work = true;
            return false;
        }
        self.work += 1;
        if self.work.is_multiple_of(CLOCK_EVERY) && self.begun.elapsed() >= self.bounds.timeout {
            self.hit.time = true;
            return false;
        }
        true
    }
}

impl<'a> Graph<'a> {
    fn check_ids(&self, ids: &[SymbolId]) -> Result<()> {
        match ids.iter().find(|id| id.idx() >= self.symbol_count()) {
            Some(id) => Err(CoreError::InvalidBounds(format!("symbol id {} is not in the graph", id.0))),
            None => Ok(()),
        }
    }

    /// All symbols reachable from `start` (forward: dependencies; reverse: impact).
    pub fn reach(
        &self,
        start: SymbolId,
        direction: Direction,
        include: Tier,
        bounds: &Bounds,
    ) -> Result<Reach> {
        self.reach_many(&[start], direction, include, bounds)
    }

    /// Multi-source reach: every start has distance 0 (impact of several changed symbols).
    /// Duplicate starts are ignored; at least one start is required.
    pub fn reach_many(
        &self,
        starts: &[SymbolId],
        direction: Direction,
        include: Tier,
        bounds: &Bounds,
    ) -> Result<Reach> {
        bounds.validate()?;
        self.check_ids(starts)?;
        let Some(&first) = starts.first() else {
            return Err(CoreError::InvalidBounds("reach needs a start symbol".into()));
        };
        let mut meter = Meter::new(bounds);
        let mut dist = vec![NOT_SEEN; self.symbol_count()];
        let mut order = Vec::new();
        let mut queue = VecDeque::new();
        for &s in starts {
            if dist[s.idx()] == NOT_SEEN {
                dist[s.idx()] = 0;
                order.push((s, 0u32));
                queue.push_back(s);
            }
        }
        let mut selected = Vec::new();
        'outer: while let Some(node) = queue.pop_front() {
            if !meter.node_ok() {
                break;
            }
            let depth = dist[node.idx()];
            for (eid, other) in self.neighbors(node, direction, include) {
                if !meter.edge_ok() {
                    break 'outer;
                }
                if depth >= bounds.max_depth {
                    if dist[other.idx()] == NOT_SEEN {
                        meter.hit.depth = true;
                    }
                    continue;
                }
                selected.push(eid);
                if dist[other.idx()] != NOT_SEEN {
                    continue;
                }
                dist[other.idx()] = depth + 1;
                order.push((other, depth + 1));
                queue.push_back(other);
            }
        }
        selected.sort_unstable();
        selected.dedup();
        Ok(Reach {
            start: first,
            target: None,
            direction,
            include,
            nodes: order,
            edges: selected,
            paths: Vec::new(),
            hit: meter.hit,
            work: meter.work,
            elapsed: meter.begun.elapsed(),
        })
    }

    /// One shortest forward path `from -> to` (BFS; ties broken by edge order).
    pub fn shortest_path(
        &self,
        from: SymbolId,
        to: SymbolId,
        include: Tier,
        bounds: &Bounds,
    ) -> Result<Reach> {
        bounds.validate()?;
        self.check_ids(&[from, to])?;
        let mut meter = Meter::new(bounds);
        let n = self.symbol_count();
        let mut dist = vec![NOT_SEEN; n];
        let mut parent: Vec<Option<(SymbolId, EdgeId)>> = vec![None; n];
        dist[from.idx()] = 0;
        let mut queue = VecDeque::from([from]);
        let mut found = from == to;
        'outer: while !found {
            let Some(node) = queue.pop_front() else { break };
            if !meter.node_ok() {
                break;
            }
            let depth = dist[node.idx()];
            for (eid, other) in self.neighbors(node, Direction::Forward, include) {
                if !meter.edge_ok() {
                    break 'outer;
                }
                if dist[other.idx()] != NOT_SEEN {
                    continue;
                }
                if depth >= bounds.max_depth {
                    meter.hit.depth = true;
                    continue;
                }
                dist[other.idx()] = depth + 1;
                parent[other.idx()] = Some((node, eid));
                if other == to {
                    found = true;
                    break 'outer;
                }
                queue.push_back(other);
            }
        }
        let mut reach = Reach {
            start: from,
            target: Some(to),
            direction: Direction::Forward,
            include,
            nodes: Vec::new(),
            edges: Vec::new(),
            paths: Vec::new(),
            hit: meter.hit,
            work: meter.work,
            elapsed: meter.begun.elapsed(),
        };
        if found {
            let mut nodes = vec![to];
            let mut edges = Vec::new();
            let mut cur = to;
            while let Some((p, e)) = parent[cur.idx()] {
                nodes.push(p);
                edges.push(e);
                cur = p;
            }
            nodes.reverse();
            edges.reverse();
            reach.nodes = nodes.iter().enumerate().map(|(i, &s)| (s, i as u32)).collect();
            reach.edges = edges.clone();
            reach.edges.sort_unstable();
            reach.paths.push(PathResult { nodes, edges });
            // A found shortest path answers the question completely (BFS order guarantees
            // no shorter path exists), even if the depth frontier was touched elsewhere.
            reach.hit = BoundsHit::default();
        }
        Ok(reach)
    }

    /// Bounded simple paths `from -> to`, shortest first (BFS over a parent-pointer arena).
    pub fn all_paths(&self, from: SymbolId, to: SymbolId, include: Tier, bounds: &Bounds) -> Result<Reach> {
        bounds.validate()?;
        self.check_ids(&[from, to])?;
        let mut meter = Meter::new(bounds);
        // Arena entries: (node, parent entry, edge from parent, depth). Every push costs one
        // unit of work, so the arena is bounded by `max_work + 1` entries.
        let mut arena: Vec<(SymbolId, u32, EdgeId, u32)> = vec![(from, NO_PARENT, EdgeId(0), 0)];
        let mut queue = VecDeque::from([0u32]);
        let mut paths = Vec::new();
        let mut expanded: Vec<SymbolId> = Vec::new();
        let on_chain = |arena: &[(SymbolId, u32, EdgeId, u32)], mut entry: u32, node: SymbolId| -> bool {
            while entry != NO_PARENT {
                let (n, p, _, _) = arena[entry as usize];
                if n == node {
                    return true;
                }
                entry = p;
            }
            false
        };
        'outer: while let Some(entry) = queue.pop_front() {
            if !meter.node_ok() {
                break;
            }
            let (node, _, _, depth) = arena[entry as usize];
            if node == to {
                let mut nodes = Vec::with_capacity(depth as usize + 1);
                let mut edges = Vec::with_capacity(depth as usize);
                let mut cur = entry;
                while cur != NO_PARENT {
                    let (n, p, e, _) = arena[cur as usize];
                    nodes.push(n);
                    if p != NO_PARENT {
                        edges.push(e);
                    }
                    cur = p;
                }
                nodes.reverse();
                edges.reverse();
                paths.push(PathResult { nodes, edges });
                if paths.len() >= bounds.max_paths {
                    if !queue.is_empty() {
                        meter.hit.paths = true;
                    }
                    break;
                }
                continue;
            }
            // Simple paths are node sequences: parallel edges (several call sites between the
            // same pair) contribute only their first edge.
            expanded.clear();
            for (eid, other) in self.neighbors(node, Direction::Forward, include) {
                if !meter.edge_ok() {
                    break 'outer;
                }
                if expanded.contains(&other) {
                    continue;
                }
                expanded.push(other);
                if on_chain(&arena, entry, other) {
                    continue;
                }
                if depth >= bounds.max_depth {
                    meter.hit.depth = true;
                    continue;
                }
                arena.push((other, entry, eid, depth + 1));
                queue.push_back((arena.len() - 1) as u32);
            }
        }
        let mut position: HashMap<SymbolId, usize> = HashMap::new();
        let mut nodes_out: Vec<(SymbolId, u32)> = Vec::new();
        let mut edges_out: Vec<EdgeId> = Vec::new();
        for p in &paths {
            for (i, &n) in p.nodes.iter().enumerate() {
                match position.get(&n) {
                    Some(&slot) => nodes_out[slot].1 = nodes_out[slot].1.min(i as u32),
                    None => {
                        position.insert(n, nodes_out.len());
                        nodes_out.push((n, i as u32));
                    }
                }
            }
            edges_out.extend_from_slice(&p.edges);
        }
        edges_out.sort_unstable();
        edges_out.dedup();
        Ok(Reach {
            start: from,
            target: Some(to),
            direction: Direction::Forward,
            include,
            nodes: nodes_out,
            edges: edges_out,
            paths,
            hit: meter.hit,
            work: meter.work,
            elapsed: meter.begun.elapsed(),
        })
    }
}

#[cfg(test)]
#[path = "../tests/unit/query.rs"]
mod tests;
