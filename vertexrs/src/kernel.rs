//! Computation kernels — the unit of work executed per dirty chunk.
//!
//! # Architecture
//! Each kernel is a stateless, type-safe struct that implements [`Kernel<T>`].
//! The executor:
//! 1. Calls [`propagate_nulls`] to build the combined validity bitmap from
//!    all input chunks.
//! 2. Passes the value slices to [`Kernel::execute_chunk`] — the kernel does
//!    not see null information and treats every element as valid.
//! 3. Applies the combined validity bitmap to the output chunk.
//!
//! This separation keeps kernels simple and SIMD-friendly: one hot loop over
//! plain `&[T]`, no per-element branching for nulls.
//!
//! # Null semantics
//! A null in **any** input propagates a null to the corresponding output
//! element.  [`propagate_nulls`] implements this by ANDing all validity
//! bitmaps — a bit is `1` (valid) only when valid in every input.

use std::marker::PhantomData;
use std::ops::{Add, Div, Mul, Rem, Sub};

use arrow_buffer::{ArrowNativeType, BooleanBuffer, NullBuffer};

// ── ChunkContract ─────────────────────────────────────────────────────────────

/// Declares what a kernel requires from the executor.
///
/// The executor inspects the contract to decide whether border elements from
/// adjacent chunks are needed before scheduling the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkContract {
    /// Each output element depends only on the same-index input element.
    ///
    /// The executor may process partial chunks and does not need to supply
    /// border elements.  Most arithmetic and comparison kernels qualify.
    ElementIndependent,

    /// The kernel requires exactly `n` input elements to produce any output.
    ///
    /// Used for fixed-arity reductions that consume a precise number of rows.
    FixedSize(usize),

    /// The kernel's output near chunk boundaries depends on elements from
    /// adjacent chunks (e.g. rolling average, cumulative sum).
    ///
    /// The executor must provide `half`-element padding on each side.
    BoundaryDependent,
}

// ── Kernel trait ──────────────────────────────────────────────────────────────

/// The computation primitive executed per dirty chunk.
///
/// # Contract
/// - `inputs[i]` contains the value slice of the *i*-th input chunk.  All
///   slices have the same length.
/// - `chunk_idx` is the chunk's zero-based position within its column; window
///   kernels may use it to decide boundary padding requirements.
/// - The return `Vec<T>` must have the same length as the input slices.
///
/// # Null handling
/// Kernels operate on raw values and do not receive or produce null bitmaps.
/// The executor calls [`propagate_nulls`] separately and overlays the result
/// onto the output chunk.
pub trait Kernel<T: ArrowNativeType>: Send + Sync {
    /// Executes the kernel over one chunk's worth of data.
    fn execute_chunk(&self, inputs: &[&[T]], chunk_idx: usize) -> Vec<T>;

    /// Declares what this kernel requires from the executor.
    fn contract(&self) -> ChunkContract;
}

// ── BinaryKernel ──────────────────────────────────────────────────────────────

/// Applies a binary operation element-wise over two same-typed input chunks.
///
/// # Example
/// ```
/// use vertexrs::kernel::{BinaryKernel, Kernel};
/// let k = BinaryKernel::new(|a: f64, b: f64| a + b);
/// assert_eq!(k.execute_chunk(&[&[1.0, 2.0], &[10.0, 20.0]], 0), [11.0, 22.0]);
/// ```
pub struct BinaryKernel<T, F> {
    op: F,
    _phantom: PhantomData<fn(T, T) -> T>,
}

impl<T: ArrowNativeType, F: Fn(T, T) -> T + Send + Sync> BinaryKernel<T, F> {
    /// Creates a kernel from a binary closure.
    pub fn new(op: F) -> Self {
        Self { op, _phantom: PhantomData }
    }
}

impl<T: ArrowNativeType, F: Fn(T, T) -> T + Send + Sync> Kernel<T> for BinaryKernel<T, F> {
    fn execute_chunk(&self, inputs: &[&[T]], _chunk_idx: usize) -> Vec<T> {
        assert_eq!(inputs.len(), 2, "BinaryKernel requires exactly 2 inputs, got {}", inputs.len());
        let len = inputs[0].len();
        assert_eq!(inputs[1].len(), len, "BinaryKernel: input length mismatch");
        (0..len).map(|i| (self.op)(inputs[0][i], inputs[1][i])).collect()
    }

    fn contract(&self) -> ChunkContract {
        ChunkContract::ElementIndependent
    }
}

// ── UnaryKernel ───────────────────────────────────────────────────────────────

/// Applies a unary operation element-wise over a single input chunk.
///
/// # Example
/// ```
/// use vertexrs::kernel::{UnaryKernel, Kernel};
/// let k = UnaryKernel::new(|x: f64| x * 2.0);
/// assert_eq!(k.execute_chunk(&[&[1.0, 2.0, 3.0]], 0), [2.0, 4.0, 6.0]);
/// ```
pub struct UnaryKernel<T, F> {
    op: F,
    _phantom: PhantomData<fn(T) -> T>,
}

impl<T: ArrowNativeType, F: Fn(T) -> T + Send + Sync> UnaryKernel<T, F> {
    /// Creates a kernel from a unary closure.
    pub fn new(op: F) -> Self {
        Self { op, _phantom: PhantomData }
    }
}

impl<T: ArrowNativeType, F: Fn(T) -> T + Send + Sync> Kernel<T> for UnaryKernel<T, F> {
    fn execute_chunk(&self, inputs: &[&[T]], _chunk_idx: usize) -> Vec<T> {
        assert_eq!(inputs.len(), 1, "UnaryKernel requires exactly 1 input, got {}", inputs.len());
        inputs[0].iter().copied().map(|v| (self.op)(v)).collect()
    }

    fn contract(&self) -> ChunkContract {
        ChunkContract::ElementIndependent
    }
}

// ── CondKernel ────────────────────────────────────────────────────────────────

/// Element-wise conditional (if/else) over three input chunks.
///
/// Selects `then[i]` when `mask[i] != T::default()` (nonzero = true),
/// otherwise selects `else_branch[i]`.
///
/// Inputs must be ordered: `[mask, then_branch, else_branch]`.
#[derive(Debug, Clone, Default)]
pub struct CondKernel<T>(PhantomData<T>);

impl<T: ArrowNativeType> CondKernel<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: ArrowNativeType> Kernel<T> for CondKernel<T> {
    /// `inputs[0]` = mask (nonzero = true), `inputs[1]` = then, `inputs[2]` = else.
    fn execute_chunk(&self, inputs: &[&[T]], _chunk_idx: usize) -> Vec<T> {
        assert_eq!(
            inputs.len(), 3,
            "CondKernel requires exactly 3 inputs (mask, then, else), got {}",
            inputs.len(),
        );
        let len = inputs[0].len();
        let zero = T::default();
        (0..len)
            .map(|i| if inputs[0][i] != zero { inputs[1][i] } else { inputs[2][i] })
            .collect()
    }

    fn contract(&self) -> ChunkContract {
        ChunkContract::ElementIndependent
    }
}

// ── IsNullKernel ──────────────────────────────────────────────────────────────

/// Produces `one` where the corresponding entry in `validity` is null,
/// `zero` otherwise.
///
/// This is a *meta-kernel*: it works on the null bitmap, not the value buffer,
/// and therefore does not implement `Kernel<T>` directly.  The executor builds
/// an `is_null` column by inspecting the input's [`NullBuffer`].
///
/// Provided as a free function for use in tests and the executor.
pub fn is_null_mask(validity: &NullBuffer, one: f64, zero: f64) -> Vec<f64> {
    (0..validity.len())
        .map(|i| if validity.is_null(i) { one } else { zero })
        .collect()
}

// ── Arithmetic factory functions ──────────────────────────────────────────────

/// Element-wise addition kernel.
pub fn add<T: ArrowNativeType + Add<Output = T>>() -> impl Kernel<T> {
    BinaryKernel::new(|a: T, b: T| a + b)
}

/// Element-wise subtraction kernel.
pub fn sub<T: ArrowNativeType + Sub<Output = T>>() -> impl Kernel<T> {
    BinaryKernel::new(|a: T, b: T| a - b)
}

/// Element-wise multiplication kernel.
pub fn mul<T: ArrowNativeType + Mul<Output = T>>() -> impl Kernel<T> {
    BinaryKernel::new(|a: T, b: T| a * b)
}

/// Element-wise division kernel.
pub fn div<T: ArrowNativeType + Div<Output = T>>() -> impl Kernel<T> {
    BinaryKernel::new(|a: T, b: T| a / b)
}

/// Element-wise remainder kernel.
pub fn rem<T: ArrowNativeType + Rem<Output = T>>() -> impl Kernel<T> {
    BinaryKernel::new(|a: T, b: T| a % b)
}

/// Element-wise conditional kernel (mask, then, else).
pub fn cond<T: ArrowNativeType>() -> impl Kernel<T> {
    CondKernel::new()
}

// ── Null propagation ──────────────────────────────────────────────────────────

/// Computes the combined validity bitmap for a set of inputs.
///
/// Returns `None` if every input is fully valid (no nulls anywhere).
///
/// Otherwise ANDs all provided validity bitmaps: an output element is valid
/// only when valid in **all** inputs.  This implements Arrow's standard null
/// propagation rule.
///
/// # Example
/// ```
/// use arrow_buffer::{BooleanBuffer, NullBuffer};
/// use vertexrs::kernel::propagate_nulls;
///
/// // Two inputs: first has a null at index 1, second at index 2.
/// let a = NullBuffer::new(BooleanBuffer::from(vec![true, false, true, true]));
/// let b = NullBuffer::new(BooleanBuffer::from(vec![true, true, false, true]));
/// let out = propagate_nulls(&[Some(&a), Some(&b)]).unwrap();
/// assert!(out.is_valid(0));
/// assert!(out.is_null(1));   // null from `a`
/// assert!(out.is_null(2));   // null from `b`
/// assert!(out.is_valid(3));
/// ```
pub fn propagate_nulls(inputs: &[Option<&NullBuffer>]) -> Option<NullBuffer> {
    let null_bufs: Vec<&BooleanBuffer> =
        inputs.iter().filter_map(|n| n.map(NullBuffer::inner)).collect();

    if null_bufs.is_empty() {
        return None; // all inputs fully valid
    }

    // AND all validity bitmaps: valid only when valid in every input.
    let combined = null_bufs[1..]
        .iter()
        .fold(null_bufs[0].clone(), |acc, &b| &acc & b);

    Some(NullBuffer::new(combined))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ChunkContract ─────────────────────────────────────────────────────────

    #[test]
    fn chunk_contract_variants_eq() {
        assert_eq!(ChunkContract::ElementIndependent, ChunkContract::ElementIndependent);
        assert_eq!(ChunkContract::FixedSize(4), ChunkContract::FixedSize(4));
        assert_ne!(ChunkContract::FixedSize(4), ChunkContract::FixedSize(8));
        assert_eq!(ChunkContract::BoundaryDependent, ChunkContract::BoundaryDependent);
    }

    // ── BinaryKernel ─────────────────────────────────────────────────────────

    #[test]
    fn binary_kernel_add_f64() {
        let k = BinaryKernel::new(|a: f64, b: f64| a + b);
        let a = [1.0_f64, 2.0, 3.0];
        let b = [10.0_f64, 20.0, 30.0];
        assert_eq!(k.execute_chunk(&[&a, &b], 0), [11.0, 22.0, 33.0]);
        assert_eq!(k.contract(), ChunkContract::ElementIndependent);
    }

    #[test]
    fn binary_kernel_mul_i64() {
        let k = BinaryKernel::new(|a: i64, b: i64| a * b);
        let a = [2_i64, 3, 4];
        let b = [5_i64, 6, 7];
        assert_eq!(k.execute_chunk(&[&a, &b], 0), [10, 18, 28]);
    }

    #[test]
    #[should_panic(expected = "requires exactly 2 inputs")]
    fn binary_kernel_panics_wrong_input_count() {
        let k = BinaryKernel::new(|a: f64, _b: f64| a);
        let a = [1.0_f64];
        k.execute_chunk(&[&a], 0); // only 1 input
    }

    // ── UnaryKernel ──────────────────────────────────────────────────────────

    #[test]
    fn unary_kernel_double() {
        let k = UnaryKernel::new(|x: f64| x * 2.0);
        let a = [1.0_f64, 2.0, 3.0];
        assert_eq!(k.execute_chunk(&[&a], 0), [2.0, 4.0, 6.0]);
        assert_eq!(k.contract(), ChunkContract::ElementIndependent);
    }

    #[test]
    fn unary_kernel_negate_i32() {
        let k = UnaryKernel::new(|x: i32| -x);
        let a = [1_i32, -2, 3];
        assert_eq!(k.execute_chunk(&[&a], 0), [-1, 2, -3]);
    }

    // ── CondKernel ───────────────────────────────────────────────────────────

    #[test]
    fn cond_kernel_selects_correctly() {
        let k = CondKernel::<f64>::new();
        let mask = [1.0_f64, 0.0, 1.0, 0.0]; // nonzero = true
        let then = [10.0_f64, 20.0, 30.0, 40.0];
        let else_ = [100.0_f64, 200.0, 300.0, 400.0];
        assert_eq!(
            k.execute_chunk(&[&mask, &then, &else_], 0),
            [10.0, 200.0, 30.0, 400.0],
        );
    }

    #[test]
    fn cond_kernel_all_true() {
        let k = CondKernel::<i32>::new();
        let mask = [1_i32, 2, 3]; // all nonzero
        let then = [10_i32, 20, 30];
        let else_ = [0_i32, 0, 0];
        assert_eq!(k.execute_chunk(&[&mask, &then, &else_], 0), [10, 20, 30]);
    }

    #[test]
    #[should_panic(expected = "requires exactly 3 inputs")]
    fn cond_kernel_panics_wrong_input_count() {
        let k = CondKernel::<f64>::new();
        k.execute_chunk(&[&[1.0_f64], &[2.0_f64]], 0); // only 2 inputs
    }

    // ── Factory kernels ───────────────────────────────────────────────────────

    #[test]
    fn factory_add_kernel() {
        let k = add::<f64>();
        let a = [3.0_f64, 4.0];
        let b = [1.0_f64, 2.0];
        assert_eq!(k.execute_chunk(&[&a, &b], 0), [4.0, 6.0]);
    }

    #[test]
    fn factory_sub_kernel() {
        let k = sub::<f64>();
        assert_eq!(k.execute_chunk(&[&[10.0_f64], &[3.0_f64]], 0), [7.0]);
    }

    #[test]
    fn factory_mul_kernel() {
        let k = mul::<i64>();
        assert_eq!(k.execute_chunk(&[&[6_i64], &[7_i64]], 0), [42]);
    }

    #[test]
    fn factory_div_kernel() {
        let k = div::<f64>();
        assert_eq!(k.execute_chunk(&[&[10.0_f64], &[4.0_f64]], 0), [2.5]);
    }

    #[test]
    fn factory_rem_kernel() {
        let k = rem::<i32>();
        assert_eq!(k.execute_chunk(&[&[10_i32], &[3_i32]], 0), [1]);
    }

    #[test]
    fn factory_cond_kernel() {
        let k = cond::<f64>();
        let mask = [0.0_f64, 1.0];
        let then = [99.0_f64, 99.0];
        let else_ = [0.0_f64, 0.0];
        assert_eq!(k.execute_chunk(&[&mask, &then, &else_], 0), [0.0, 99.0]);
    }

    // ── is_null_mask ─────────────────────────────────────────────────────────

    #[test]
    fn is_null_mask_marks_nulls() {
        // validity: [valid, null, valid, null]
        let validity = NullBuffer::new(BooleanBuffer::from(vec![true, false, true, false]));
        let result = is_null_mask(&validity, 1.0, 0.0);
        assert_eq!(result, [0.0, 1.0, 0.0, 1.0]);
    }

    // ── propagate_nulls ───────────────────────────────────────────────────────

    #[test]
    fn propagate_nulls_all_valid_returns_none() {
        assert!(propagate_nulls(&[None, None, None]).is_none());
    }

    #[test]
    fn propagate_nulls_empty_returns_none() {
        assert!(propagate_nulls(&[]).is_none());
    }

    #[test]
    fn propagate_nulls_single_buffer() {
        // validity: [null, valid, valid]
        let b = NullBuffer::new(BooleanBuffer::from(vec![false, true, true]));
        let out = propagate_nulls(&[None, Some(&b)]).unwrap();
        assert!(out.is_null(0));
        assert!(out.is_valid(1));
        assert!(out.is_valid(2));
        assert_eq!(out.null_count(), 1);
    }

    #[test]
    fn propagate_nulls_ands_two_bitmaps() {
        // a: [valid, null,  valid, valid]
        // b: [valid, valid, null,  valid]
        // AND →  [valid, null,  null,  valid]
        let a = NullBuffer::new(BooleanBuffer::from(vec![true, false, true, true]));
        let b = NullBuffer::new(BooleanBuffer::from(vec![true, true, false, true]));
        let out = propagate_nulls(&[Some(&a), Some(&b)]).unwrap();
        assert!(out.is_valid(0));
        assert!(out.is_null(1));  // null from a
        assert!(out.is_null(2));  // null from b
        assert!(out.is_valid(3));
        assert_eq!(out.null_count(), 2);
    }

    #[test]
    fn propagate_nulls_three_inputs_mixed() {
        // a: [valid, null,  valid]
        // b: [valid, valid, valid]   (None — all valid)
        // c: [valid, valid, null]
        // AND → [valid, null, null]
        let a = NullBuffer::new(BooleanBuffer::from(vec![true, false, true]));
        let c = NullBuffer::new(BooleanBuffer::from(vec![true, true, false]));
        let out = propagate_nulls(&[Some(&a), None, Some(&c)]).unwrap();
        assert!(out.is_valid(0));
        assert!(out.is_null(1));
        assert!(out.is_null(2));
    }
}
