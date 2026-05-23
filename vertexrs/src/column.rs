//! Chunked column storage — the physical memory layer for VertexRS.
//!
//! Data is split into fixed-size [`AlignedChunk`]s, each backed by an
//! Arrow-aligned buffer.  A [`ChunkedColumn`] owns the ordered list of
//! chunks that makes up one full column.
//!
//! # Alignment guarantee
//! Arrow's allocator (the default [`arrow_buffer`] backend) satisfies the
//! Arrow spec §2.3 requirement of 64-byte alignment.  Every chunk's data
//! pointer is therefore safe for direct AVX-512 loads without further
//! adjustment.
//!
//! # CHUNK_SIZE choice
//! 256 elements per chunk gives:
//! - ≤ 2 KiB per chunk at 8-byte width — fits in L1D cache on modern cores.
//! - Exactly one AVX-512 "unrolled pass" over f64 (32 lanes × 8 iters).
//! - A small dirty-bitmap: one `u8` of chunk-level bits per 2048 rows.

use std::ops::Range;

use arrow_array::PrimitiveArray;
use arrow_buffer::{ArrowNativeType, MutableBuffer, ScalarBuffer};
use roaring::RoaringBitmap;

use crate::ArrowBacked;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of elements in a single [`AlignedChunk`].
pub const CHUNK_SIZE: usize = 256;

// ── AlignedChunk ──────────────────────────────────────────────────────────────

/// A fixed-size, Arrow-aligned slice of at most [`CHUNK_SIZE`] elements.
///
/// The underlying [`ScalarBuffer`] is allocated by Arrow's 64-byte-aligned
/// allocator, making every chunk's data pointer safe for SIMD loads.
///
/// # Construction
/// ```
/// use vertexrs::column::{AlignedChunk, CHUNK_SIZE};
/// let chunk = AlignedChunk::<f64>::new(&[1.0, 2.0, 3.0]);
/// assert_eq!(chunk.values(), [1.0, 2.0, 3.0]);
/// ```
#[derive(Debug, Clone)]
pub struct AlignedChunk<T: ArrowNativeType> {
    /// Arrow-backed, 64-byte-aligned buffer.  Length ≤ [`CHUNK_SIZE`].
    data: ScalarBuffer<T>,
}

impl<T: ArrowNativeType> AlignedChunk<T> {
    /// Creates a chunk from `src`.
    ///
    /// # Panics
    /// Panics if `src.len() > CHUNK_SIZE`.
    pub fn new(src: &[T]) -> Self {
        assert!(
            src.len() <= CHUNK_SIZE,
            "AlignedChunk: {} elements exceeds CHUNK_SIZE ({})",
            src.len(),
            CHUNK_SIZE,
        );
        // Arrow's MutableBuffer uses a 64-byte-aligned allocator (Arrow spec
        // §2.3). ScalarBuffer::from(Vec) wraps the Vec's allocation directly
        // and does not guarantee alignment, so we go through MutableBuffer to
        // uphold the invariant documented on this type.
        let mut buf = MutableBuffer::new(src.len() * std::mem::size_of::<T>());
        buf.extend(src.iter().copied());
        Self {
            data: ScalarBuffer::new(buf.into(), 0, src.len()),
        }
    }

    /// Returns a slice of the elements in this chunk.
    #[inline]
    pub fn values(&self) -> &[T] {
        &self.data
    }

    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `true` when the chunk holds no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `true` when the chunk holds exactly [`CHUNK_SIZE`] elements.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.data.len() == CHUNK_SIZE
    }
}

impl<T: ArrowBacked> AlignedChunk<T> {
    /// Converts this chunk to an Arrow [`PrimitiveArray`].
    ///
    /// Zero-copy: the underlying buffer is shared via `Arc`.
    pub fn to_arrow_array(&self) -> PrimitiveArray<T::ArrowType> {
        PrimitiveArray::new(self.data.clone(), None)
    }
}

// ── ChunkedColumn ─────────────────────────────────────────────────────────────

/// An ordered column of [`AlignedChunk`]s with chunk-level dirty tracking.
///
/// All interior chunks are full ([`CHUNK_SIZE`] elements); the last chunk may
/// be partial.  This invariant holds for columns built via [`from_slice`] and
/// is *advisory* for columns assembled with [`push_chunk`] — callers are
/// responsible for maintaining it if SIMD kernel correctness depends on it.
///
/// # Dirty tracking
/// A [`RoaringBitmap`] records which *chunk indices* contain stale data.
/// Call [`mark_dirty`] with a row range after any mutation; the executor
/// queries [`dirty_chunks`] to find work, then calls [`clear_dirty`] once
/// the relevant output columns have been recomputed.
///
/// [`from_slice`]: ChunkedColumn::from_slice
/// [`push_chunk`]: ChunkedColumn::push_chunk
/// [`mark_dirty`]: ChunkedColumn::mark_dirty
/// [`dirty_chunks`]: ChunkedColumn::dirty_chunks
/// [`clear_dirty`]: ChunkedColumn::clear_dirty
#[derive(Debug, Clone)]
pub struct ChunkedColumn<T: ArrowNativeType> {
    chunks: Vec<AlignedChunk<T>>,
    /// Chunk indices that need recomputation.  Stored as `u32` to match
    /// `RoaringBitmap`'s element type; a column would need > 16 billion
    /// rows before a chunk index could overflow `u32`.
    dirty: RoaringBitmap,
}

impl<T: ArrowNativeType> ChunkedColumn<T> {
    /// Creates an empty column.
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            dirty: RoaringBitmap::new(),
        }
    }

    /// Builds a column from a flat slice, splitting into [`CHUNK_SIZE`]-element chunks.
    pub fn from_slice(data: &[T]) -> Self {
        Self {
            chunks: data.chunks(CHUNK_SIZE).map(AlignedChunk::new).collect(),
            dirty: RoaringBitmap::new(),
        }
    }

    /// Total number of elements across all chunks.
    pub fn len(&self) -> usize {
        self.chunks.iter().map(AlignedChunk::len).sum()
    }

    /// `true` when there are no elements.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Appends a pre-built chunk.
    pub fn push_chunk(&mut self, chunk: AlignedChunk<T>) {
        self.chunks.push(chunk);
    }

    /// Iterates over chunks in order.
    pub fn iter_chunks(&self) -> impl Iterator<Item = &AlignedChunk<T>> {
        self.chunks.iter()
    }

    /// Returns the element at `row_idx`, or `None` if out of bounds.
    pub fn get(&self, mut row_idx: usize) -> Option<T> {
        for chunk in &self.chunks {
            if row_idx < chunk.len() {
                return Some(chunk.values()[row_idx]);
            }
            row_idx -= chunk.len();
        }
        None
    }

    // ── Dirty tracking ────────────────────────────────────────────────────────

    /// Marks every chunk that overlaps `row_range` as dirty.
    ///
    /// The range is in *row* space; this method converts it to the corresponding
    /// chunk-index range.  Does nothing if `row_range` is empty.
    pub fn mark_dirty(&mut self, row_range: Range<usize>) {
        if row_range.is_empty() {
            return;
        }
        let first = (row_range.start / CHUNK_SIZE) as u32;
        let last = ((row_range.end - 1) / CHUNK_SIZE) as u32;
        self.dirty.insert_range(first..=last);
    }

    /// Marks every existing chunk as dirty.
    pub fn mark_all_dirty(&mut self) {
        if self.chunks.is_empty() {
            return;
        }
        let last = (self.chunks.len() - 1) as u32;
        self.dirty.insert_range(0..=last);
    }

    /// Returns `true` if any chunks are marked dirty.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Iterates over `(chunk_idx, chunk)` for every dirty chunk.
    ///
    /// Chunk indices that are out of bounds are silently skipped.
    pub fn dirty_chunks(&self) -> impl Iterator<Item = (usize, &AlignedChunk<T>)> {
        let chunks = &self.chunks;
        self.dirty.iter().filter_map(move |idx| {
            let i = idx as usize;
            chunks.get(i).map(|c| (i, c))
        })
    }

    /// Clears all dirty flags.
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Replaces the chunk at `chunk_idx` with `chunk`.
    ///
    /// # Panics
    /// Panics if `chunk_idx >= self.chunk_count()`.
    pub fn replace_chunk(&mut self, chunk_idx: usize, chunk: AlignedChunk<T>) {
        assert!(
            chunk_idx < self.chunks.len(),
            "ChunkedColumn::replace_chunk: index {} out of bounds (len {})",
            chunk_idx,
            self.chunks.len(),
        );
        self.chunks[chunk_idx] = chunk;
    }

    /// Removes the dirty flag for a single chunk index.
    ///
    /// Has no effect if the chunk was not dirty.
    #[inline]
    pub fn clear_dirty_chunk(&mut self, chunk_idx: usize) {
        self.dirty.remove(chunk_idx as u32);
    }
}

impl<T: ArrowNativeType> Default for ChunkedColumn<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ArrowBacked> ChunkedColumn<T> {
    /// Converts each chunk to an Arrow [`PrimitiveArray`].
    ///
    /// Each conversion is zero-copy (Arc buffer clone).
    pub fn to_arrow_arrays(&self) -> Vec<PrimitiveArray<T::ArrowType>> {
        self.chunks
            .iter()
            .map(AlignedChunk::to_arrow_array)
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── AlignedChunk ─────────────────────────────────────────────────────────

    #[test]
    fn chunk_stores_values() {
        let chunk = AlignedChunk::new(&[1.0_f64, 2.0, 3.0]);
        assert_eq!(chunk.values(), [1.0, 2.0, 3.0]);
        assert_eq!(chunk.len(), 3);
        assert!(!chunk.is_full());
        assert!(!chunk.is_empty());
    }

    #[test]
    fn chunk_full_at_chunk_size() {
        let data: Vec<f64> = (0..CHUNK_SIZE).map(|i| i as f64).collect();
        let chunk = AlignedChunk::new(&data);
        assert!(chunk.is_full());
        assert_eq!(chunk.len(), CHUNK_SIZE);
    }

    #[test]
    #[should_panic(expected = "exceeds CHUNK_SIZE")]
    fn chunk_panics_if_oversized() {
        let data = vec![0.0_f64; CHUNK_SIZE + 1];
        let _ = AlignedChunk::new(&data);
    }

    #[test]
    fn chunk_buffer_is_64byte_aligned() {
        let data: Vec<f64> = (0..128).map(|i| i as f64).collect();
        let chunk = AlignedChunk::new(&data);
        let ptr = chunk.values().as_ptr() as usize;
        assert_eq!(ptr % 64, 0, "buffer not 64-byte aligned (ptr = {ptr:#x})");
    }

    #[test]
    fn chunk_arrow_roundtrip() {
        let chunk = AlignedChunk::new(&[10.0_f64, 20.0, 30.0]);
        let arr = chunk.to_arrow_array();
        assert_eq!(arr.values().as_ref(), [10.0_f64, 20.0, 30.0]);
    }

    // ── ChunkedColumn ────────────────────────────────────────────────────────

    #[test]
    fn column_from_slice_splits_at_chunk_size() {
        // 600 / 256 = 2 full (256 each) + 1 partial (88)
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let col = ChunkedColumn::from_slice(&data);

        assert_eq!(col.chunk_count(), 3);
        assert_eq!(col.len(), 600);
        assert!(col.iter_chunks().take(2).all(AlignedChunk::is_full));
        assert!(!col.iter_chunks().last().unwrap().is_full());
    }

    #[test]
    fn column_get_returns_correct_element() {
        let data: Vec<i64> = (0..600).map(|i| i as i64).collect();
        let col = ChunkedColumn::from_slice(&data);

        assert_eq!(col.get(0), Some(0_i64));
        assert_eq!(col.get(255), Some(255_i64)); // last of first chunk
        assert_eq!(col.get(256), Some(256_i64)); // first of second chunk
        assert_eq!(col.get(599), Some(599_i64)); // last element
        assert_eq!(col.get(600), None); // out of bounds
    }

    #[test]
    fn column_push_chunk() {
        let mut col = ChunkedColumn::new();
        col.push_chunk(AlignedChunk::new(&[1.0_f64, 2.0, 3.0]));
        col.push_chunk(AlignedChunk::new(&[4.0_f64, 5.0]));

        assert_eq!(col.chunk_count(), 2);
        assert_eq!(col.len(), 5);
        assert_eq!(col.get(3), Some(4.0_f64));
    }

    #[test]
    fn column_to_arrow_arrays() {
        // 300 / 256 = 1 full (256) + 1 partial (44)
        let data: Vec<f64> = (0..300).map(|i| i as f64).collect();
        let col = ChunkedColumn::from_slice(&data);
        let arrays = col.to_arrow_arrays();

        assert_eq!(arrays.len(), 2);
        assert_eq!(arrays[0].len(), 256);
        assert_eq!(arrays[1].len(), 44);
        assert_eq!(arrays[1].values()[0], 256.0_f64);
    }

    #[test]
    fn column_default_is_empty() {
        let col = ChunkedColumn::<f32>::default();
        assert!(col.is_empty());
        assert_eq!(col.len(), 0);
        assert_eq!(col.chunk_count(), 0);
    }

    // ── Dirty tracking ────────────────────────────────────────────────────────

    #[test]
    fn new_column_is_clean() {
        let col = ChunkedColumn::<f64>::new();
        assert!(!col.is_dirty());
        assert_eq!(col.dirty_chunks().count(), 0);
    }

    #[test]
    fn from_slice_column_is_clean() {
        let col = ChunkedColumn::from_slice(&[1.0_f64, 2.0, 3.0]);
        assert!(!col.is_dirty());
    }

    #[test]
    fn mark_dirty_single_row_marks_correct_chunk() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_dirty(100..150); // fully within chunk 0 (rows 0..256)
        assert!(col.is_dirty());
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [0]);
    }

    #[test]
    fn mark_dirty_range_spanning_two_chunks() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        // rows 250..300: row 250 → chunk 0, row 299 → chunk 1
        col.mark_dirty(250..300);
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [0, 1]);
    }

    #[test]
    fn mark_dirty_empty_range_is_noop() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_dirty(100..100); // empty range
        assert!(!col.is_dirty());
    }

    #[test]
    fn dirty_chunks_yields_correct_pairs() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_dirty(0..1); // chunk 0
        col.mark_dirty(512..513); // chunk 2
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [0, 2]);

        // verify the chunk data is correct
        let (_, chunk0) = col.dirty_chunks().next().unwrap();
        assert_eq!(chunk0.len(), CHUNK_SIZE);
    }

    #[test]
    fn clear_dirty_removes_all_flags() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_all_dirty();
        assert!(col.is_dirty());

        col.clear_dirty();
        assert!(!col.is_dirty());
        assert_eq!(col.dirty_chunks().count(), 0);
    }

    #[test]
    fn mark_all_dirty_marks_every_chunk() {
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect(); // 3 chunks
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_all_dirty();
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [0, 1, 2]);
        assert_eq!(col.dirty_chunks().count(), col.chunk_count());
    }

    #[test]
    fn append_delta_marks_correct_chunks() {
        // simulate: column has 768 rows (3 full chunks), then rows 512..768
        // are "newly appended" — only chunk 2 should be dirty
        let data: Vec<f64> = (0..768).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_dirty(512..768); // the appended range
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [2]);
    }

    #[test]
    fn mutation_delta_marks_correct_chunks() {
        // rows 100..300 mutated → spans chunks 0 (0..256) and 1 (256..512)
        let data: Vec<f64> = (0..600).map(|i| i as f64).collect();
        let mut col = ChunkedColumn::from_slice(&data);

        col.mark_dirty(100..300);
        let dirty: Vec<usize> = col.dirty_chunks().map(|(i, _)| i).collect();
        assert_eq!(dirty, [0, 1]);
        assert!(!dirty.contains(&2)); // chunk 2 unaffected
    }
}
