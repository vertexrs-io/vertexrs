//! Single-threaded DAG executor — Phase 1.5 / 1.6.
//!
//! # Architecture
//! The [`Executor`] owns a [`Graph`], one [`ChunkedColumn<T>`] per node, and
//! one optional [`Kernel<T>`] per node.  Source nodes (no kernel) carry
//! pre-populated columns; derived nodes are recomputed on demand.
//!
//! # Dirty tracking
//! Call [`Executor::mark_source_dirty`] with a row range whenever source data
//! changes.  The executor uses [`Graph::propagate_dirty`] to mark every
//! downstream column, then [`Executor::run`] recomputes all dirty chunks in
//! topological order and clears dirty flags as it goes.
//!
//! # Failure modes
//! - [`FailureMode::Soft`] — a panicking kernel marks the output chunk as
//!   all-null and pushes a warning.  Execution continues.
//! - [`FailureMode::Hard`] — a panicking kernel immediately returns
//!   [`Err(ExecutionError)`] and halts the cycle.
//!
//! # NA tracking
//! Per-node, per-chunk [`NullBuffer`]s are maintained inside the executor.
//! Input nulls are propagated to the output via [`propagate_nulls`].  If the
//! null fraction in any output chunk exceeds [`NaConfig::warn_threshold`], a
//! warning is emitted.
//!
//! # Purity checking
//! When [`Executor::debug_purity_check`] is `true`, each chunk is computed
//! twice and the outputs are compared.  A mismatch means the kernel closure
//! has hidden side-effects; a warning is pushed.

use std::{
    cell::{Ref, RefCell, RefMut},
    ops::Range,
    panic,
    sync::Arc,
};

use arrow_buffer::{ArrowNativeType, BooleanBuffer, NullBuffer};

use crate::{
    column::{AlignedChunk, ChunkedColumn},
    dag::{Graph, NodeId},
    kernel::{Kernel, propagate_nulls},
};

// ── WarningCollector ──────────────────────────────────────────────────────────

/// A capped, per-cycle warning buffer.
///
/// Warnings are accumulated during [`Executor::run`] and drained by the
/// caller via [`drain`].  Once `cap` warnings have been collected, further
/// pushes are silently dropped until the buffer is drained.
///
/// [`drain`]: WarningCollector::drain
pub struct WarningCollector {
    warnings: Vec<String>,
    cap: usize,
}

impl WarningCollector {
    /// Creates a collector with the given maximum capacity.
    pub fn new(cap: usize) -> Self {
        Self {
            warnings: Vec::new(),
            cap,
        }
    }

    /// Appends `msg` if the cap has not been reached.
    pub fn push(&mut self, msg: impl Into<String>) {
        if self.warnings.len() < self.cap {
            self.warnings.push(msg.into());
        }
    }

    /// Drains and returns all collected warnings, leaving the buffer empty.
    pub fn drain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Returns `true` when no warnings have been collected.
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Number of warnings currently buffered.
    pub fn len(&self) -> usize {
        self.warnings.len()
    }
}

// ── FailureMode ───────────────────────────────────────────────────────────────

/// Controls how a kernel panic or invalid output is handled during execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureMode {
    /// Mark the output chunk as all-null, collect a warning, and continue.
    #[default]
    Soft,
    /// Return [`Err(ExecutionError)`] immediately, halting the cycle.
    Hard,
}

// ── ExecutionError ────────────────────────────────────────────────────────────

/// Returned by [`Executor::run`] when [`FailureMode::Hard`] is active and a
/// kernel fails.
#[derive(Debug)]
pub struct ExecutionError {
    /// The node whose kernel failed.
    pub node: NodeId,
    /// Which chunk (0-based) was being computed when the failure occurred.
    pub chunk_idx: usize,
    /// Human-readable description of the failure.
    pub message: String,
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "execution error at node {:?} chunk {}: {}",
            self.node, self.chunk_idx, self.message
        )
    }
}

impl std::error::Error for ExecutionError {}

// ── NaConfig ─────────────────────────────────────────────────────────────────

/// Configuration for NA (null-value) threshold warnings.
pub struct NaConfig {
    /// Emit a warning if the null fraction in any output chunk exceeds this
    /// value.  Range: `0.0` (warn on any NA) to `1.0` (never warn).
    pub warn_threshold: f64,
}

impl Default for NaConfig {
    fn default() -> Self {
        Self {
            warn_threshold: 0.5,
        }
    }
}

// ── Executor ──────────────────────────────────────────────────────────────────

/// Single-threaded executor for a homogeneous-typed computation DAG.
///
/// # Type parameter
/// `T` is the native element type for every column and kernel in this
/// executor.  Multi-type support is deferred to a later phase.
pub struct Executor<T: ArrowNativeType> {
    graph: Graph,
    /// One data column per node.  `RefCell` enables the borrow-split pattern
    /// needed to read input columns while writing to an output column.
    columns: Vec<RefCell<ChunkedColumn<T>>>,
    /// One kernel per node (`None` = source node).  `Arc` so that we can
    /// clone a reference cheaply and avoid holding a borrow of `self.kernels`
    /// across other `&mut self` calls.
    kernels: Vec<Option<Arc<dyn Kernel<T>>>>,
    /// Per-node, per-chunk null bitmaps.  `null_chunks[node][chunk]` is
    /// `Some(buf)` only when that chunk has at least one null element.
    null_chunks: Vec<Vec<Option<NullBuffer>>>,
    /// Warnings collected during the most recent `run()` call.
    pub warnings: WarningCollector,
    /// How to handle a panicking kernel.
    pub failure_mode: FailureMode,
    /// NA threshold configuration.
    pub na_config: NaConfig,
    /// When `true`, each chunk is computed twice and outputs are compared for
    /// determinism (purity check).
    pub debug_purity_check: bool,
}

impl<T: ArrowNativeType + PartialEq> Executor<T> {
    /// Creates an executor.
    ///
    /// `columns` and `kernels` must have the same length as the number of
    /// nodes in `graph`.  Source nodes should supply `None` for their kernel.
    ///
    /// # Panics
    /// Panics if `columns.len() != graph.len()` or
    ///          `kernels.len() != graph.len()`.
    pub fn new(
        graph: Graph,
        columns: Vec<ChunkedColumn<T>>,
        kernels: Vec<Option<Box<dyn Kernel<T>>>>,
    ) -> Self {
        let n = graph.len();
        assert_eq!(
            columns.len(),
            n,
            "Executor::new: columns.len() ({}) != graph.len() ({})",
            columns.len(),
            n,
        );
        assert_eq!(
            kernels.len(),
            n,
            "Executor::new: kernels.len() ({}) != graph.len() ({})",
            kernels.len(),
            n,
        );

        let null_chunks = columns
            .iter()
            .map(|c| vec![None; c.chunk_count()])
            .collect();

        let kernels_arc: Vec<Option<Arc<dyn Kernel<T>>>> =
            kernels.into_iter().map(|k| k.map(Arc::from)).collect();

        Self {
            graph,
            columns: columns.into_iter().map(RefCell::new).collect(),
            kernels: kernels_arc,
            null_chunks,
            warnings: WarningCollector::new(1_000),
            failure_mode: FailureMode::default(),
            na_config: NaConfig::default(),
            debug_purity_check: false,
        }
    }

    /// Read-only access to the column for `id`.
    pub fn column(&self, id: NodeId) -> Ref<'_, ChunkedColumn<T>> {
        self.columns[id.index()].borrow()
    }

    /// Mutable access to the column for `id`.
    pub fn column_mut(&self, id: NodeId) -> RefMut<'_, ChunkedColumn<T>> {
        self.columns[id.index()].borrow_mut()
    }

    /// Drains and returns all collected warnings.
    pub fn drain_warnings(&mut self) -> Vec<String> {
        self.warnings.drain()
    }

    /// Returns the null buffer for a single (`node_id`, `chunk_idx`) pair, if any.
    pub fn null_chunk(&self, node_id: NodeId, chunk_idx: usize) -> Option<&NullBuffer> {
        self.null_chunks
            .get(node_id.index())
            .and_then(|v| v.get(chunk_idx))
            .and_then(|n| n.as_ref())
    }

    /// Marks `source` and all downstream nodes dirty for `row_range`.
    ///
    /// Call this after updating source column data before calling [`run`].
    ///
    /// [`run`]: Executor::run
    pub fn mark_source_dirty(&self, source: NodeId, row_range: Range<usize>) {
        let dirty_map = self.graph.propagate_dirty(source, row_range);
        for (node_id, range) in dirty_map {
            self.columns[node_id.index()].borrow_mut().mark_dirty(range);
        }
    }

    /// Runs one compute cycle: recomputes all dirty chunks in topological order.
    ///
    /// Returns `Ok(())` on success or when all failures are soft.
    /// Returns `Err(ExecutionError)` immediately on the first hard failure.
    pub fn run(&mut self) -> Result<(), ExecutionError> {
        let order = self.graph.topological_order();

        for node_id in order {
            let is_dirty = self.columns[node_id.index()].borrow().is_dirty();
            if !is_dirty {
                continue;
            }

            // Clone the Arc so we release the borrow on `self.kernels` before
            // making other `&mut self` calls inside the chunk loop.
            let maybe_kernel = self.kernels[node_id.index()].clone();

            let Some(kernel) = maybe_kernel else {
                // Source node: data was updated externally; just clear dirty.
                self.columns[node_id.index()].borrow_mut().clear_dirty();
                continue;
            };

            // Clone the descriptor to avoid holding a borrow across mutations.
            let node_desc = self.graph.node(node_id).clone();

            // Snapshot which chunks are dirty *before* we start writing.
            let dirty_chunk_indices: Vec<usize> = {
                let col = self.columns[node_id.index()].borrow();
                col.dirty_chunks().map(|(i, _)| i).collect()
            };

            for chunk_idx in dirty_chunk_indices {
                // ── Gather input values ───────────────────────────────────
                //
                // We copy each input chunk into an owned Vec so we can
                // release the immutable borrows before mutably borrowing the
                // output column.
                let input_values: Vec<Vec<T>> = node_desc
                    .inputs
                    .iter()
                    .map(|&inp_id| {
                        self.columns[inp_id.index()]
                            .borrow()
                            .iter_chunks()
                            .nth(chunk_idx)
                            .map(|c| c.values().to_vec())
                            .unwrap_or_default()
                    })
                    .collect();

                // ── Gather input null bitmaps ─────────────────────────────
                let input_nulls: Vec<Option<NullBuffer>> = node_desc
                    .inputs
                    .iter()
                    .map(|&inp_id| {
                        self.null_chunks
                            .get(inp_id.index())
                            .and_then(|v| v.get(chunk_idx))
                            .and_then(|n| n.clone())
                    })
                    .collect();

                let chunk_len = input_values.first().map(|v| v.len()).unwrap_or(0);

                // ── Execute kernel (catch panics) ─────────────────────────
                let input_slices: Vec<&[T]> = input_values.iter().map(Vec::as_slice).collect();

                let kernel_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    kernel.execute_chunk(&input_slices, chunk_idx)
                }));

                let (output_values, chunk_failed) = match kernel_result {
                    Ok(values) => (values, false),
                    Err(payload) => {
                        let msg = payload
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "kernel panicked".to_string());

                        match self.failure_mode {
                            FailureMode::Hard => {
                                return Err(ExecutionError {
                                    node: node_id,
                                    chunk_idx,
                                    message: msg,
                                });
                            }
                            FailureMode::Soft => {
                                self.warnings.push(format!(
                                    "node '{}' chunk {}: kernel failed: {}",
                                    node_desc.name, chunk_idx, msg
                                ));
                                (vec![T::default(); chunk_len], true)
                            }
                        }
                    }
                };

                // ── Purity check (debug mode) ─────────────────────────────
                if self.debug_purity_check && !chunk_failed {
                    let input_slices2: Vec<&[T]> = input_values.iter().map(Vec::as_slice).collect();
                    let second = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                        kernel.execute_chunk(&input_slices2, chunk_idx)
                    }));
                    if let Ok(second_out) = second
                        && second_out != output_values
                    {
                        self.warnings.push(format!(
                            "node '{}' chunk {}: purity check failed \
                             (non-deterministic output)",
                            node_desc.name, chunk_idx
                        ));
                    }
                }

                // ── Compute output null bitmap ────────────────────────────
                let input_null_refs: Vec<Option<&NullBuffer>> =
                    input_nulls.iter().map(Option::as_ref).collect();

                let output_nulls = if chunk_failed {
                    // All elements failed — mark entire chunk as null.
                    Some(NullBuffer::new(BooleanBuffer::from(vec![false; chunk_len])))
                } else {
                    propagate_nulls(&input_null_refs)
                };

                // ── NA threshold check ────────────────────────────────────
                if let Some(ref null_buf) = output_nulls {
                    let fraction = null_buf.null_count() as f64 / chunk_len.max(1) as f64;
                    if fraction > self.na_config.warn_threshold {
                        self.warnings.push(format!(
                            "node '{}' chunk {}: NA fraction {:.1}% exceeds \
                             threshold {:.1}%",
                            node_desc.name,
                            chunk_idx,
                            fraction * 100.0,
                            self.na_config.warn_threshold * 100.0,
                        ));
                    }
                }

                // ── Write output values ───────────────────────────────────
                {
                    let mut out_col = self.columns[node_id.index()].borrow_mut();
                    let new_chunk = AlignedChunk::new(&output_values);
                    if chunk_idx < out_col.chunk_count() {
                        out_col.replace_chunk(chunk_idx, new_chunk);
                    } else {
                        out_col.push_chunk(new_chunk);
                    }
                    out_col.clear_dirty_chunk(chunk_idx);
                }

                // ── Update null tracking ──────────────────────────────────
                self.set_null_chunk(node_id, chunk_idx, output_nulls);
            }
        }

        Ok(())
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn set_null_chunk(&mut self, node_id: NodeId, chunk_idx: usize, nulls: Option<NullBuffer>) {
        let v = &mut self.null_chunks[node_id.index()];
        if v.len() <= chunk_idx {
            v.resize(chunk_idx + 1, None);
        }
        v[chunk_idx] = nulls;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        column::ChunkedColumn,
        dag::{Graph, IndexMapping},
        kernel::{self, UnaryKernel},
    };

    // ── WarningCollector ─────────────────────────────────────────────────────

    #[test]
    fn warning_collector_push_and_drain() {
        let mut wc = WarningCollector::new(3);
        wc.push("a");
        wc.push("b");
        assert_eq!(wc.len(), 2);
        assert!(!wc.is_empty());

        let v = wc.drain();
        assert_eq!(v, ["a", "b"]);
        assert!(wc.is_empty());
    }

    #[test]
    fn warning_collector_cap_is_respected() {
        let mut wc = WarningCollector::new(2);
        wc.push("a");
        wc.push("b");
        wc.push("c"); // should be dropped
        assert_eq!(wc.len(), 2);
        let v = wc.drain();
        assert_eq!(v, ["a", "b"]);
    }

    // ── Integration: 3-node linear DAG ──────────────────────────────────────

    fn three_node_exec() -> (Executor<f64>, NodeId, NodeId, NodeId) {
        // A (source, 512 f64 elements: 1.0..=512.0)
        // B = A * 2.0   (UnaryKernel)
        // C = A + B     (BinaryKernel add)
        //
        // Expected: B[i] = (i+1)*2, C[i] = (i+1)*3
        let mut graph = Graph::new();
        let a_id = graph.add_node("A", &[], IndexMapping::Pointwise);
        let b_id = graph.add_node("B", &[a_id], IndexMapping::Pointwise);
        let c_id = graph.add_node("C", &[a_id, b_id], IndexMapping::Pointwise);

        let n = 512usize;
        let a_data: Vec<f64> = (1..=n).map(|i| i as f64).collect();
        let zeros = vec![0.0_f64; n];

        let columns: Vec<ChunkedColumn<f64>> = vec![
            ChunkedColumn::from_slice(&a_data),
            ChunkedColumn::from_slice(&zeros),
            ChunkedColumn::from_slice(&zeros),
        ];
        let kernels: Vec<Option<Box<dyn Kernel<f64>>>> = vec![
            None,
            Some(Box::new(UnaryKernel::new(|x: f64| x * 2.0))),
            Some(Box::new(kernel::add::<f64>())),
        ];

        let exec = Executor::new(graph, columns, kernels);
        (exec, a_id, b_id, c_id)
    }

    #[test]
    fn integration_three_node_dag() {
        let (mut exec, a_id, b_id, c_id) = three_node_exec();
        let n = 512usize;

        exec.mark_source_dirty(a_id, 0..n);
        exec.run().expect("run should succeed");

        // B = A * 2
        let b = exec.column(b_id);
        for i in 0..n {
            let expected = (i + 1) as f64 * 2.0;
            assert_eq!(b.get(i), Some(expected), "B[{i}] mismatch");
        }
        drop(b);

        // C = A + B = A * 3
        let c = exec.column(c_id);
        for i in 0..n {
            let expected = (i + 1) as f64 * 3.0;
            assert_eq!(c.get(i), Some(expected), "C[{i}] mismatch");
        }
        drop(c);

        // Both derived columns should be clean after run().
        assert!(!exec.column(b_id).is_dirty());
        assert!(!exec.column(c_id).is_dirty());
    }

    #[test]
    fn run_is_idempotent_when_nothing_is_dirty() {
        let (mut exec, _a, b_id, c_id) = three_node_exec();

        // First run with a dirty source.
        exec.mark_source_dirty(b_id, 0..512); // mark a derived node to keep it simple
        exec.run().expect("first run ok");

        // Second run without re-dirtying anything.
        exec.run().expect("second run ok");

        // Nothing blew up and the columns still look fine.
        assert!(!exec.column(c_id).is_dirty());
    }

    // ── Failure modes ────────────────────────────────────────────────────────

    fn panicking_exec() -> (Executor<f64>, NodeId, NodeId) {
        let mut graph = Graph::new();
        let a_id = graph.add_node("A", &[], IndexMapping::Pointwise);
        let b_id = graph.add_node("B", &[a_id], IndexMapping::Pointwise);

        let a_col = ChunkedColumn::from_slice(&[1.0_f64, 2.0, 3.0]);
        let b_col = ChunkedColumn::from_slice(&[0.0_f64, 0.0, 0.0]);

        let kernels: Vec<Option<Box<dyn Kernel<f64>>>> = vec![
            None,
            Some(Box::new(UnaryKernel::new(|_: f64| -> f64 {
                panic!("intentional panic")
            }))),
        ];

        let exec = Executor::new(graph, vec![a_col, b_col], kernels);
        (exec, a_id, b_id)
    }

    #[test]
    fn soft_failure_marks_output_null_and_warns() {
        let (mut exec, a_id, b_id) = panicking_exec();
        exec.failure_mode = FailureMode::Soft;

        exec.mark_source_dirty(a_id, 0..3);
        exec.run().expect("soft mode must not return Err");

        // Chunk 0 of B should be all-null (3 elements).
        let null_buf = exec.null_chunk(b_id, 0).expect("null buffer should be set");
        assert_eq!(
            null_buf.null_count(),
            3,
            "all 3 output elements should be null"
        );

        // At least one warning should have been emitted.
        assert!(!exec.warnings.is_empty());
    }

    #[test]
    fn hard_failure_returns_error() {
        let (mut exec, a_id, b_id) = panicking_exec();
        exec.failure_mode = FailureMode::Hard;

        exec.mark_source_dirty(a_id, 0..3);
        let result = exec.run();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.node, b_id);
        assert_eq!(err.chunk_idx, 0);
        assert!(err.message.contains("intentional panic"));
    }

    // ── NA threshold ─────────────────────────────────────────────────────────

    #[test]
    fn na_threshold_warning_on_soft_failure() {
        let (mut exec, a_id, _b_id) = panicking_exec();
        exec.failure_mode = FailureMode::Soft;
        exec.na_config = NaConfig {
            warn_threshold: 0.5,
        }; // 50%

        exec.mark_source_dirty(a_id, 0..3);
        exec.run().expect("soft mode must not return Err");

        // Null fraction = 3/3 = 100% > 50% → threshold warning expected.
        let warnings = exec.drain_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("NA fraction") || w.contains("threshold")),
            "expected an NA-threshold warning; got: {warnings:?}",
        );
    }

    // ── Purity check ─────────────────────────────────────────────────────────

    #[test]
    fn purity_check_passes_for_pure_kernel() {
        let (mut exec, a_id, _b, _c) = three_node_exec();
        exec.debug_purity_check = true;

        exec.mark_source_dirty(a_id, 0..512);
        exec.run().expect("run ok");

        let warnings = exec.drain_warnings();
        assert!(
            !warnings.iter().any(|w| w.contains("purity check failed")),
            "pure kernel should produce no purity warnings; got: {warnings:?}",
        );
    }

    // ── Null propagation ─────────────────────────────────────────────────────

    #[test]
    fn run_produces_no_null_buffers_for_valid_data() {
        let (mut exec, a_id, b_id, c_id) = three_node_exec();
        exec.mark_source_dirty(a_id, 0..512);
        exec.run().expect("run ok");

        // No nulls should be set when all inputs are valid.
        assert!(exec.null_chunk(b_id, 0).is_none());
        assert!(exec.null_chunk(b_id, 1).is_none());
        assert!(exec.null_chunk(c_id, 0).is_none());
        assert!(exec.null_chunk(c_id, 1).is_none());
    }
}
