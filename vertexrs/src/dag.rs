//! DAG topology — node identifiers, access patterns, and dirty-range propagation.
//!
//! # Concepts
//! - A [`NodeId`] is a dense sequential index into a [`Graph`].
//! - Each node has an [`IndexMapping`] that describes how its *output* row
//!   indices relate to its *input* row indices.
//! - [`Graph::propagate_dirty`] performs a BFS from a source node, translating
//!   the dirty row range through each consumer's mapping and merging ranges when
//!   a node has multiple dirty producers.
//!
//! # Design notes
//! Row ranges are in *row* space, not chunk space.  The executor converts them
//! to chunk indices via [`ChunkedColumn::mark_dirty`] when writing back results.
//!
//! [`ChunkedColumn::mark_dirty`]: crate::column::ChunkedColumn::mark_dirty

use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    ops::Range,
};

// ── NodeId ────────────────────────────────────────────────────────────────────

/// Opaque identifier for a node in the computation [`Graph`].
///
/// Internally a dense `u32` index — sufficient for any realistic graph size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// Returns the `NodeId` as a `usize` for slice indexing.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

// ── IndexMapping ──────────────────────────────────────────────────────────────

/// Describes how a node's *output* row indices depend on its *input* row indices.
///
/// Used by [`Graph::propagate_dirty`] to map a dirty row range on a producer
/// into the corresponding dirty rows on a consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexMapping {
    /// Output row `i` depends only on input row `i` (e.g. arithmetic, cast).
    Pointwise,

    /// Output row `i` depends on input rows `[i − half, i + half]`
    /// (e.g. rolling mean with window `2·half + 1`).
    ///
    /// The propagated dirty range is expanded by `half` on each side.
    LocalWindow { half: usize },

    /// Many-to-many output/input mapping (e.g. hash-join scatter side).
    ///
    /// Any dirty input row makes the **entire** output dirty.
    Scatter,

    /// Re-interprets the buffer layout (e.g. transpose, reshape).
    ///
    /// Any dirty input row makes the **entire** output dirty.
    Reshape,
}

impl IndexMapping {
    /// Maps a dirty row range on the input to the corresponding output row range.
    ///
    /// Returns `0..usize::MAX` for blocking mappings ([`Scatter`] / [`Reshape`]).
    ///
    /// [`Scatter`]: IndexMapping::Scatter
    /// [`Reshape`]: IndexMapping::Reshape
    pub fn map_range(&self, input: Range<usize>) -> Range<usize> {
        match self {
            IndexMapping::Pointwise => input,
            IndexMapping::LocalWindow { half } => {
                let start = input.start.saturating_sub(*half);
                let end = input.end.saturating_add(*half);
                start..end
            }
            IndexMapping::Scatter | IndexMapping::Reshape => 0..usize::MAX,
        }
    }

    /// Returns `true` for mappings where any dirty input makes the whole output dirty.
    #[inline]
    pub fn is_blocking(&self) -> bool {
        matches!(self, IndexMapping::Scatter | IndexMapping::Reshape)
    }
}

// ── NodeDescriptor ────────────────────────────────────────────────────────────

/// Static metadata for one node in the [`Graph`].
///
/// Nodes are immutable once added — mutations are expressed as dirty-range
/// updates on the *data* side, not as graph rewiring.
#[derive(Debug, Clone)]
pub struct NodeDescriptor {
    /// Unique identifier for this node.
    pub id: NodeId,
    /// Human-readable name (typically the variable name from the macro).
    pub name: &'static str,
    /// Ordered list of producer nodes this node reads from.
    pub inputs: Vec<NodeId>,
    /// How this node's output rows relate to its input rows.
    pub mapping: IndexMapping,
}

// ── Graph ─────────────────────────────────────────────────────────────────────

/// A directed acyclic computation graph.
///
/// Nodes are added with [`add_node`]; the graph builds the consumer
/// (reverse-edge) adjacency list automatically.  [`topological_order`] uses
/// Kahn's algorithm, and [`propagate_dirty`] performs BFS dirty-range
/// propagation.
///
/// # Invariants
/// - `NodeId` values are dense indices `0, 1, 2, …`.
/// - All inputs to a node must already exist in the graph.
/// - The graph must be acyclic; [`topological_order`] panics on cycles.
///
/// [`add_node`]: Graph::add_node
/// [`topological_order`]: Graph::topological_order
/// [`propagate_dirty`]: Graph::propagate_dirty
#[derive(Debug, Default)]
pub struct Graph {
    nodes: Vec<NodeDescriptor>,
    /// `consumers[i]` = IDs of nodes that list node `i` as an input.
    consumers: Vec<Vec<NodeId>>,
}

impl Graph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node and returns its freshly allocated [`NodeId`].
    ///
    /// # Panics
    /// Panics if any `NodeId` in `inputs` does not already exist in the graph.
    pub fn add_node(
        &mut self,
        name: &'static str,
        inputs: &[NodeId],
        mapping: IndexMapping,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);

        for &inp in inputs {
            assert!(
                inp.index() < self.nodes.len(),
                "Graph::add_node: input {:?} does not exist (graph has {} nodes)",
                inp,
                self.nodes.len(),
            );
            self.consumers[inp.index()].push(id);
        }

        self.consumers.push(Vec::new());
        self.nodes.push(NodeDescriptor {
            id,
            name,
            inputs: inputs.to_vec(),
            mapping,
        });
        id
    }

    /// Returns the number of nodes in the graph.
    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` if the graph has no nodes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the [`NodeDescriptor`] for `id`.
    ///
    /// # Panics
    /// Panics if `id` is not in the graph.
    pub fn node(&self, id: NodeId) -> &NodeDescriptor {
        &self.nodes[id.index()]
    }

    /// Returns the IDs of every node that directly consumes `id`'s output.
    pub fn consumers_of(&self, id: NodeId) -> &[NodeId] {
        &self.consumers[id.index()]
    }

    /// Returns all nodes in a valid topological order (every producer before
    /// all of its consumers).
    ///
    /// Uses Kahn's algorithm (BFS on in-degrees).
    ///
    /// # Panics
    /// Panics if the graph contains a cycle.
    pub fn topological_order(&self) -> Vec<NodeId> {
        let n = self.nodes.len();

        // in_degree[i] = number of producer inputs for node i
        let mut in_degree: Vec<usize> = self.nodes.iter().map(|d| d.inputs.len()).collect();

        let mut queue: VecDeque<NodeId> = in_degree
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d == 0)
            .map(|(i, _)| NodeId(i as u32))
            .collect();

        let mut order = Vec::with_capacity(n);

        while let Some(id) = queue.pop_front() {
            order.push(id);
            for &consumer_id in &self.consumers[id.index()] {
                in_degree[consumer_id.index()] -= 1;
                if in_degree[consumer_id.index()] == 0 {
                    queue.push_back(consumer_id);
                }
            }
        }

        assert_eq!(
            order.len(),
            n,
            "Graph::topological_order: cycle detected ({} / {} nodes processed)",
            order.len(),
            n,
        );

        order
    }

    /// Propagates a dirty row range from `source` through the graph via BFS.
    ///
    /// Returns a map of every node reachable from `source` (including `source`
    /// itself) to the row range that needs recomputation.  When a node has
    /// multiple dirty producers, their translated ranges are unioned.
    ///
    /// # Row-space ranges
    /// Ranges are in *row* space for each node.  Convert to chunk indices with
    /// [`ChunkedColumn::mark_dirty`] before scheduling execution.
    ///
    /// # Blocking nodes
    /// Nodes with [`Scatter`] or [`Reshape`] mappings always receive
    /// `0..usize::MAX`, signalling that the whole output is dirty.
    ///
    /// [`ChunkedColumn::mark_dirty`]: crate::column::ChunkedColumn::mark_dirty
    /// [`Scatter`]: IndexMapping::Scatter
    /// [`Reshape`]: IndexMapping::Reshape
    pub fn propagate_dirty(
        &self,
        source: NodeId,
        row_range: Range<usize>,
    ) -> HashMap<NodeId, Range<usize>> {
        let mut dirty: HashMap<NodeId, Range<usize>> = HashMap::new();
        let mut queue: VecDeque<NodeId> = VecDeque::new();

        dirty.insert(source, row_range);
        queue.push_back(source);

        while let Some(node_id) = queue.pop_front() {
            // Always use the current (potentially widened) range, not a stale
            // snapshot from enqueue time — this ensures correctness when a node
            // is re-queued after its range is widened by a second producer.
            let range = dirty[&node_id].clone();

            for &consumer_id in &self.consumers[node_id.index()] {
                let consumer = &self.nodes[consumer_id.index()];
                let mapped = consumer.mapping.map_range(range.clone());

                match dirty.entry(consumer_id) {
                    Entry::Vacant(v) => {
                        v.insert(mapped);
                        queue.push_back(consumer_id);
                    }
                    Entry::Occupied(mut o) => {
                        let existing = o.get_mut();
                        let new_start = existing.start.min(mapped.start);
                        let new_end = existing.end.max(mapped.end);
                        // Only re-queue if the range actually grew.
                        if new_start < existing.start || new_end > existing.end {
                            *existing = new_start..new_end;
                            queue.push_back(consumer_id);
                        }
                    }
                }
            }
        }

        dirty
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Verifies that `order` is a valid topological ordering for `graph`.
    fn is_valid_topo_order(order: &[NodeId], graph: &Graph) -> bool {
        let pos: HashMap<NodeId, usize> =
            order.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        graph
            .nodes
            .iter()
            .all(|desc| desc.inputs.iter().all(|&inp| pos[&inp] < pos[&desc.id]))
    }

    // ── IndexMapping ─────────────────────────────────────────────────────────

    #[test]
    fn mapping_pointwise_identity() {
        assert_eq!(IndexMapping::Pointwise.map_range(100..200), 100..200);
    }

    #[test]
    fn mapping_local_window_expands_range() {
        let m = IndexMapping::LocalWindow { half: 10 };
        assert_eq!(m.map_range(100..200), 90..210);
    }

    #[test]
    fn mapping_local_window_saturates_at_zero() {
        let m = IndexMapping::LocalWindow { half: 50 };
        assert_eq!(m.map_range(10..20), 0..70); // start clamped to 0
    }

    #[test]
    fn mapping_scatter_is_full_range() {
        assert_eq!(IndexMapping::Scatter.map_range(100..200), 0..usize::MAX);
    }

    #[test]
    fn mapping_reshape_is_full_range() {
        assert_eq!(IndexMapping::Reshape.map_range(100..200), 0..usize::MAX);
    }

    #[test]
    fn mapping_is_blocking_flags() {
        assert!(!IndexMapping::Pointwise.is_blocking());
        assert!(!IndexMapping::LocalWindow { half: 1 }.is_blocking());
        assert!(IndexMapping::Scatter.is_blocking());
        assert!(IndexMapping::Reshape.is_blocking());
    }

    // ── Graph construction ────────────────────────────────────────────────────

    #[test]
    fn add_and_retrieve_nodes() {
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);

        assert_eq!(g.node(a).name, "a");
        assert_eq!(g.node(b).name, "b");
        assert_eq!(g.node(b).inputs, vec![a]);
        assert_eq!(g.consumers_of(a), &[b]);
    }

    #[test]
    #[should_panic(expected = "does not exist")]
    fn add_node_with_missing_input_panics() {
        let mut g = Graph::new();
        let phantom = NodeId(99);
        g.add_node("x", &[phantom], IndexMapping::Pointwise);
    }

    // ── topological_order ────────────────────────────────────────────────────

    #[test]
    fn topo_order_single_node() {
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let order = g.topological_order();
        assert_eq!(order, vec![a]);
    }

    #[test]
    fn topo_order_linear_chain() {
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let c = g.add_node("c", &[b], IndexMapping::Pointwise);
        let d = g.add_node("d", &[c], IndexMapping::Pointwise);

        let order = g.topological_order();
        assert!(is_valid_topo_order(&order, &g));
        assert_eq!(order.len(), 4);
        // For a linear chain, there is exactly one valid order.
        assert_eq!(order, vec![a, b, c, d]);
    }

    #[test]
    fn topo_order_diamond() {
        //   A
        //  / \
        // B   C
        //  \ /
        //   D
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let c = g.add_node("c", &[a], IndexMapping::Pointwise);
        let d = g.add_node("d", &[b, c], IndexMapping::Pointwise);

        let order = g.topological_order();
        assert!(is_valid_topo_order(&order, &g));
        assert_eq!(order.len(), 4);
        // A must be first, D must be last.
        assert_eq!(order[0], a);
        assert_eq!(order[3], d);
    }

    // ── propagate_dirty ───────────────────────────────────────────────────────

    #[test]
    fn propagate_dirty_source_only() {
        // Source node with no consumers — only the source is in the dirty map.
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);

        let dirty = g.propagate_dirty(a, 100..200);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[&a], 100..200);
    }

    #[test]
    fn propagate_dirty_simple_pointwise_chain() {
        // A → B → C, all Pointwise.  Dirty rows pass through unchanged.
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let c = g.add_node("c", &[b], IndexMapping::Pointwise);

        let dirty = g.propagate_dirty(a, 50..150);
        assert_eq!(dirty[&a], 50..150);
        assert_eq!(dirty[&b], 50..150);
        assert_eq!(dirty[&c], 50..150);
    }

    #[test]
    fn propagate_dirty_local_window_expands() {
        // A → B (LocalWindow half=10).  Dirty rows 100..200 → B dirty 90..210.
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::LocalWindow { half: 10 });

        let dirty = g.propagate_dirty(a, 100..200);
        assert_eq!(dirty[&b], 90..210);
    }

    #[test]
    fn propagate_dirty_blocking_node_full_output() {
        // A → B (Scatter).  Any dirty rows → B fully dirty.
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Scatter);

        let dirty = g.propagate_dirty(a, 100..200);
        assert_eq!(dirty[&b], 0..usize::MAX);
    }

    #[test]
    fn propagate_dirty_five_node_mixed_graph() {
        // A (source)
        // B (Pointwise,          inputs=[A])
        // C (LocalWindow half=10, inputs=[B])
        // D (Scatter,             inputs=[B])
        // E (Pointwise,           inputs=[C])
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let c = g.add_node("c", &[b], IndexMapping::LocalWindow { half: 10 });
        let d = g.add_node("d", &[b], IndexMapping::Scatter);
        let e = g.add_node("e", &[c], IndexMapping::Pointwise);

        let dirty = g.propagate_dirty(a, 100..200);

        assert_eq!(dirty[&a], 100..200);
        assert_eq!(dirty[&b], 100..200);
        assert_eq!(dirty[&c], 90..210);
        assert_eq!(dirty[&d], 0..usize::MAX);
        assert_eq!(dirty[&e], 90..210);
        // All 5 nodes are reachable from A.
        assert_eq!(dirty.len(), 5);
    }

    #[test]
    fn propagate_dirty_diamond_merges_ranges() {
        //   A (dirty 100..200)
        //  / \
        // B   C (LocalWindow half=10)
        //  \ /
        //   D (Pointwise — receives union from B and C)
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let c = g.add_node("c", &[a], IndexMapping::LocalWindow { half: 10 });
        let d = g.add_node("d", &[b, c], IndexMapping::Pointwise);

        let dirty = g.propagate_dirty(a, 100..200);

        // B: Pointwise → 100..200
        assert_eq!(dirty[&b], 100..200);
        // C: LocalWindow → 90..210
        assert_eq!(dirty[&c], 90..210);
        // D: union of Pointwise(100..200) from B and Pointwise(90..210) from C
        assert_eq!(dirty[&d], 90..210);
    }

    #[test]
    fn propagate_dirty_does_not_include_unrelated_nodes() {
        // A → B → C, and separately D (unconnected source).
        let mut g = Graph::new();
        let a = g.add_node("a", &[], IndexMapping::Pointwise);
        let b = g.add_node("b", &[a], IndexMapping::Pointwise);
        let _c = g.add_node("c", &[b], IndexMapping::Pointwise);
        let d = g.add_node("d", &[], IndexMapping::Pointwise); // unrelated

        let dirty = g.propagate_dirty(d, 0..10);
        // Only D is in the dirty map — A/B/C are unreachable from D.
        assert_eq!(dirty.len(), 1);
        assert!(dirty.contains_key(&d));
    }
}
