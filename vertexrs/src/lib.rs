// ── Optional jemalloc allocator ───────────────────────────────────────────────
//
// Enable with `--features jemalloc`.  Sets jemalloc as the process-wide
// allocator, which improves throughput on multi-threaded workloads (reduced
// lock contention vs. the system allocator).  Arrow's buffer allocations will
// also go through jemalloc when this feature is active.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Allow proc-macro generated code inside this crate to reference items via
// `vertexrs::` — the same path that external users would use.
extern crate self as vertexrs;

pub mod column;
pub mod dag;
pub mod executor;
pub mod kernel;
pub mod pipeline;

pub use pipeline::{FailureMode, Pipeline, PipelineError, PipelineSettings};

use arrow_array::{
    Array, ArrowPrimitiveType, BooleanArray, PrimitiveArray, StringArray,
    types::{
        Float16Type, Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type,
        UInt8Type, UInt16Type, UInt32Type, UInt64Type,
    },
};
use arrow_buffer::{ArrowNativeType, ScalarBuffer};
use half::f16;

pub use vertexrs_macro::{node, pipeline};

// ── BoolNode ──────────────────────────────────────────────────────────────────

/// A named boolean column node backed by an Arrow [`BooleanArray`].
///
/// Arrow packs booleans as bits internally, so this type is distinct from
/// [`Node<u8>`] — it carries proper boolean semantics and is usable as a
/// mask in filter and conditional operations.
///
/// Source nodes are created with [`BoolNode::from_data`]; derived nodes are
/// produced by the [`node!`] macro when the closure has an explicit `-> bool`
/// return type.
#[derive(Debug, Clone)]
pub struct BoolNode {
    /// The node's identifier as written in the macro invocation.
    pub name: &'static str,
    /// Names of upstream nodes detected in the defining expression.
    pub deps: &'static [&'static str],
    /// Bit-packed Arrow boolean array.
    pub data: BooleanArray,
}

impl BoolNode {
    /// Creates a source boolean node from a `Vec<bool>` with no upstream dependencies.
    pub fn from_data(name: &'static str, data: Vec<bool>) -> Self {
        Self {
            name,
            deps: &[],
            data: BooleanArray::from(data),
        }
    }

    /// Creates a derived boolean node with an explicit dependency list.
    ///
    /// Called by the [`node!`] macro; prefer the macro over calling this directly.
    pub fn new_with_deps(
        name: &'static str,
        deps: &'static [&'static str],
        data: Vec<bool>,
    ) -> Self {
        Self {
            name,
            deps,
            data: BooleanArray::from(data),
        }
    }

    /// Returns the boolean value at the given row index.
    ///
    /// # Panics
    /// Panics if `idx >= self.len()`.
    pub fn value(&self, idx: usize) -> bool {
        self.data.value(idx)
    }

    /// Number of rows in this column.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the column has no rows.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Materialises the bit-packed boolean array into a `Vec<bool>`.
    pub fn to_vec(&self) -> Vec<bool> {
        (0..self.len()).map(|i| self.data.value(i)).collect()
    }
}

// ── StringNode ────────────────────────────────────────────────────────────────

/// A named UTF-8 string column node backed by an Arrow [`StringArray`].
///
/// String columns are read-only input sources in Phase 2.5.  String-output
/// derived nodes (where a `node!` closure produces `String` values) are
/// deferred to a later phase.
///
/// Source nodes are created with [`StringNode::from_data`].
#[derive(Debug, Clone)]
pub struct StringNode {
    /// The node's identifier as written in the macro invocation.
    pub name: &'static str,
    /// Names of upstream nodes detected in the defining expression.
    pub deps: &'static [&'static str],
    /// Contiguous UTF-8 bytes with an offset buffer, per Arrow `StringArray` layout.
    pub data: StringArray,
}

impl StringNode {
    /// Creates a source string node from a slice of string values with no
    /// upstream dependencies.
    pub fn from_data(name: &'static str, data: &[&str]) -> Self {
        let array: StringArray = data.iter().map(|&s| Some(s)).collect();
        Self {
            name,
            deps: &[],
            data: array,
        }
    }

    /// Creates a derived string node with an explicit dependency list.
    ///
    /// Called by the [`node!`] macro; prefer the macro over calling this directly.
    pub fn new_with_deps(
        name: &'static str,
        deps: &'static [&'static str],
        data: Vec<String>,
    ) -> Self {
        let array: StringArray = data.iter().map(|s| Some(s.as_str())).collect();
        Self {
            name,
            deps,
            data: array,
        }
    }

    /// Borrows the string value at the given row index.
    ///
    /// # Panics
    /// Panics if `idx >= self.len()`.
    pub fn value(&self, idx: usize) -> &str {
        self.data.value(idx)
    }

    /// Number of rows in this column.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the column has no rows.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns all string values as a `Vec<&str>` with lifetimes tied to `&self`.
    pub fn values(&self) -> Vec<&str> {
        (0..self.len()).map(|i| self.data.value(i)).collect()
    }
}

mod private {
    pub trait Sealed {}

    /// Enables [`Frame::get`] to downcast a type-erased [`AnyNode`] to a typed
    /// slice.  Implemented for all [`ArrowBacked`] types by [`impl_arrow_backed!`].
    pub trait Extract {
        fn try_extract(any: &super::AnyNode) -> Option<&[Self]>
        where
            Self: Sized;
    }
}

/// Links a native Rust scalar type to its Arrow primitive array type.
///
/// This trait is sealed — it cannot be implemented outside this crate — and is
/// satisfied by the six types that `Node` supports: `f32`, `f64`, `i32`,
/// `i64`, `u32`, `u64`.
pub trait ArrowBacked: private::Sealed + private::Extract + ArrowNativeType {
    /// The corresponding Arrow primitive type (e.g. `Float64Type` for `f64`).
    type ArrowType: ArrowPrimitiveType<Native = Self>;
}

// ── Node ─────────────────────────────────────────────────────────────────────

/// A named, typed column node in the computation DAG.
///
/// Column data is stored in an Arrow [`ScalarBuffer<T>`], a reference-counted,
/// 64-byte-aligned buffer that is layout-compatible with Arrow's `PrimitiveArray`.
/// Use [`Node::to_arrow_array`] / [`Node::from_arrow_array`] for zero-copy
/// interop with the wider Arrow ecosystem.
///
/// The `.row()` / `.col()` API operates directly on the buffer's native slice.
#[derive(Debug, Clone)]
pub struct Node<T: ArrowNativeType> {
    /// The node's identifier as written in the macro invocation.
    pub name: &'static str,
    /// Names of upstream nodes detected in the defining expression.
    pub deps: &'static [&'static str],
    /// Arrow-backed column of computed values.
    pub data: ScalarBuffer<T>,
}

impl<T: ArrowNativeType> Node<T> {
    /// Creates a derived node with an explicit dependency list and computed data.
    ///
    /// Called by the [`node!`] macro; prefer the macro over calling this directly.
    pub fn new_with_deps(name: &'static str, deps: &'static [&'static str], data: Vec<T>) -> Self {
        Self {
            name,
            deps,
            data: ScalarBuffer::from(data),
        }
    }

    /// Creates a source node from existing data with no upstream dependencies.
    pub fn from_data(name: &'static str, data: Vec<T>) -> Self {
        Self {
            name,
            deps: &[],
            data: ScalarBuffer::from(data),
        }
    }

    /// Returns a slice over this column's values.
    pub fn values(&self) -> &[T] {
        &self.data
    }

    /// Number of rows in this column.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Row-wise map: applies `f` to each element and collects into a new column.
    ///
    /// This is the runtime implementation for the single-node case.  For
    /// expressions that reference other nodes (e.g. `node!(b = a.row(|x| x + c))`),
    /// the [`node!`] macro expands the closure body directly, shadowing each extra
    /// node reference with its per-row value, so this method is never called with
    /// a closure that captures another `Node<T>` at runtime.
    pub fn row<U: Copy, F: Fn(T) -> U>(&self, f: F) -> Vec<U> {
        self.data.iter().map(|&x| f(x)).collect()
    }

    /// Column-wise operation: passes a [`ColRef`] to `f` and returns the result.
    ///
    /// Suitable for operations that may change the column length: sort, filter,
    /// group-by, join.  The result type `U` is typically `Vec<T>` but can be any
    /// value (e.g. a scalar aggregate).
    pub fn col<U, F: FnOnce(ColRef<'_, T>) -> U>(&self, f: F) -> U {
        f(ColRef { data: &self.data })
    }

    /// Internal: borrow this node's data as a [`ColRef`].
    ///
    /// Used by the [`node!`] macro when generating row-kernel expansions that
    /// reference multiple nodes.
    pub fn col_ref(&self) -> ColRef<'_, T> {
        ColRef { data: &self.data }
    }
}

impl<T: ArrowBacked> Node<T> {
    /// Returns the data as an Arrow [`PrimitiveArray`].
    ///
    /// The conversion is zero-copy: the underlying [`ScalarBuffer`] is shared
    /// via `Arc`, so this is `O(1)`.
    pub fn to_arrow_array(&self) -> PrimitiveArray<T::ArrowType> {
        PrimitiveArray::new(self.data.clone(), None)
    }

    /// Creates a source node from an Arrow [`PrimitiveArray`].
    ///
    /// The conversion is zero-copy: the buffer inside the array is cloned at
    /// the `Arc` level.
    pub fn from_arrow_array(name: &'static str, array: &PrimitiveArray<T::ArrowType>) -> Self {
        Self {
            name,
            deps: &[],
            data: array.values().clone(),
        }
    }
}

// ── ColRef ────────────────────────────────────────────────────────────────────

/// A borrowed view over a node's column data.
///
/// Produced by [`Node::col`] and [`Node::col_ref`].  Arithmetic between two
/// [`ColRef`]s, or between a [`ColRef`] and a scalar, yields a `Vec<T>`.
/// Transformation methods like [`sort`](ColRef::sort) and
/// [`filter`](ColRef::filter) return a new `Vec<T>`, resetting the index space.
#[derive(Debug, Clone, Copy)]
pub struct ColRef<'a, T: Copy> {
    pub data: &'a [T],
}

impl<'a, T: Copy + PartialOrd> ColRef<'a, T> {
    /// Returns a new column sorted ascending.
    ///
    /// NaN/incomparable values are sorted to the front.
    pub fn sort(&self) -> Vec<T> {
        let mut v = self.data.to_vec();
        v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Less));
        v
    }
}

impl<'a, T: Copy> ColRef<'a, T> {
    /// Returns a new column containing only elements satisfying the predicate.
    pub fn filter<F: Fn(&T) -> bool>(&self, f: F) -> Vec<T> {
        self.data.iter().filter(|x| f(x)).copied().collect()
    }
}

macro_rules! impl_col_ops {
    ($($Trait:ident, $method:ident);* $(;)?) => {
        $(
            // ColRef op ColRef → Vec
            impl<'a, T: Copy + std::ops::$Trait<Output = T>> std::ops::$Trait for ColRef<'a, T> {
                type Output = Vec<T>;
                fn $method(self, rhs: ColRef<'a, T>) -> Vec<T> {
                    self.data
                        .iter()
                        .zip(rhs.data)
                        .map(|(&a, &b)| <T as std::ops::$Trait>::$method(a, b))
                        .collect()
                }
            }

            // ColRef op scalar → Vec (broadcasts scalar across the column)
            impl<'a, T: Copy + std::ops::$Trait<Output = T>> std::ops::$Trait<T> for ColRef<'a, T> {
                type Output = Vec<T>;
                fn $method(self, rhs: T) -> Vec<T> {
                    self.data
                        .iter()
                        .map(|&a| <T as std::ops::$Trait>::$method(a, rhs))
                        .collect()
                }
            }
        )*
    };
}

impl_col_ops! { Add, add; Sub, sub; Mul, mul; Div, div; Rem, rem; }

// ── T op Node<T>  (rust-analyzer compatibility) ───────────────────────────────
//
// These impls make expressions like `|x: f64| x + some_node` type-check when
// rust-analyzer analyses the pre-expansion source of a `node!()` call.
// The `node!` macro rewrites every node reference in a row-closure body to its
// per-row scalar value before the closure executes, so these impls are never
// reached at runtime.  Any call would indicate a bug — hence the panics.
macro_rules! impl_node_rhs_ops {
    ($($P:ty),* $(;)?) => {
        $(
            impl std::ops::Add<Node<$P>> for $P {
                type Output = $P;
                fn add(self, _: Node<$P>) -> $P { panic!("Node<T> operand used outside node!()") }
            }
            impl std::ops::Sub<Node<$P>> for $P {
                type Output = $P;
                fn sub(self, _: Node<$P>) -> $P { panic!("Node<T> operand used outside node!()") }
            }
            impl std::ops::Mul<Node<$P>> for $P {
                type Output = $P;
                fn mul(self, _: Node<$P>) -> $P { panic!("Node<T> operand used outside node!()") }
            }
            impl std::ops::Div<Node<$P>> for $P {
                type Output = $P;
                fn div(self, _: Node<$P>) -> $P { panic!("Node<T> operand used outside node!()") }
            }
            impl std::ops::Rem<Node<$P>> for $P {
                type Output = $P;
                fn rem(self, _: Node<$P>) -> $P { panic!("Node<T> operand used outside node!()") }
            }
        )*
    };
}

impl_node_rhs_ops!(f16, f32, f64, i8, i16, i32, i64, u8, u16, u32, u64);

// ── AnyNode ───────────────────────────────────────────────────────────────────

/// A type-erased wrapper around a [`Node<T>`] for any of the eleven supported
/// native types, plus [`BoolNode`] and [`StringNode`].
///
/// Used by [`Frame`] to store heterogeneous columns in a single collection.
/// Each variant preserves the full node value including its name and deps.
#[derive(Debug, Clone)]
pub enum AnyNode {
    F16(Node<f16>),
    F32(Node<f32>),
    F64(Node<f64>),
    I8(Node<i8>),
    I16(Node<i16>),
    I32(Node<i32>),
    I64(Node<i64>),
    U8(Node<u8>),
    U16(Node<u16>),
    U32(Node<u32>),
    U64(Node<u64>),
    Bool(BoolNode),
    Str(StringNode),
}

impl AnyNode {
    /// The node's name.
    pub fn name(&self) -> &str {
        match self {
            AnyNode::F16(n) => n.name,
            AnyNode::F32(n) => n.name,
            AnyNode::F64(n) => n.name,
            AnyNode::I8(n) => n.name,
            AnyNode::I16(n) => n.name,
            AnyNode::I32(n) => n.name,
            AnyNode::I64(n) => n.name,
            AnyNode::U8(n) => n.name,
            AnyNode::U16(n) => n.name,
            AnyNode::U32(n) => n.name,
            AnyNode::U64(n) => n.name,
            AnyNode::Bool(n) => n.name,
            AnyNode::Str(n) => n.name,
        }
    }

    /// Number of rows in this column.
    pub fn len(&self) -> usize {
        match self {
            AnyNode::F16(n) => n.len(),
            AnyNode::F32(n) => n.len(),
            AnyNode::F64(n) => n.len(),
            AnyNode::I8(n) => n.len(),
            AnyNode::I16(n) => n.len(),
            AnyNode::I32(n) => n.len(),
            AnyNode::I64(n) => n.len(),
            AnyNode::U8(n) => n.len(),
            AnyNode::U16(n) => n.len(),
            AnyNode::U32(n) => n.len(),
            AnyNode::U64(n) => n.len(),
            AnyNode::Bool(n) => n.len(),
            AnyNode::Str(n) => n.len(),
        }
    }

    /// Returns `true` if the column has no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The name of the native type stored in this column (e.g. `"f64"`, `"bool"`, `"str"`).
    pub fn data_type(&self) -> &'static str {
        match self {
            AnyNode::F16(_) => "f16",
            AnyNode::F32(_) => "f32",
            AnyNode::F64(_) => "f64",
            AnyNode::I8(_) => "i8",
            AnyNode::I16(_) => "i16",
            AnyNode::I32(_) => "i32",
            AnyNode::I64(_) => "i64",
            AnyNode::U8(_) => "u8",
            AnyNode::U16(_) => "u16",
            AnyNode::U32(_) => "u32",
            AnyNode::U64(_) => "u64",
            AnyNode::Bool(_) => "bool",
            AnyNode::Str(_) => "str",
        }
    }
}

impl From<Node<f16>> for AnyNode {
    fn from(n: Node<f16>) -> Self {
        AnyNode::F16(n)
    }
}
impl From<Node<f32>> for AnyNode {
    fn from(n: Node<f32>) -> Self {
        AnyNode::F32(n)
    }
}
impl From<Node<f64>> for AnyNode {
    fn from(n: Node<f64>) -> Self {
        AnyNode::F64(n)
    }
}
impl From<Node<i8>> for AnyNode {
    fn from(n: Node<i8>) -> Self {
        AnyNode::I8(n)
    }
}
impl From<Node<i16>> for AnyNode {
    fn from(n: Node<i16>) -> Self {
        AnyNode::I16(n)
    }
}
impl From<Node<i32>> for AnyNode {
    fn from(n: Node<i32>) -> Self {
        AnyNode::I32(n)
    }
}
impl From<Node<i64>> for AnyNode {
    fn from(n: Node<i64>) -> Self {
        AnyNode::I64(n)
    }
}
impl From<Node<u8>> for AnyNode {
    fn from(n: Node<u8>) -> Self {
        AnyNode::U8(n)
    }
}
impl From<Node<u16>> for AnyNode {
    fn from(n: Node<u16>) -> Self {
        AnyNode::U16(n)
    }
}
impl From<Node<u32>> for AnyNode {
    fn from(n: Node<u32>) -> Self {
        AnyNode::U32(n)
    }
}
impl From<Node<u64>> for AnyNode {
    fn from(n: Node<u64>) -> Self {
        AnyNode::U64(n)
    }
}
impl From<BoolNode> for AnyNode {
    fn from(n: BoolNode) -> Self {
        AnyNode::Bool(n)
    }
}
impl From<StringNode> for AnyNode {
    fn from(n: StringNode) -> Self {
        AnyNode::Str(n)
    }
}

// ── Frame ─────────────────────────────────────────────────────────────────────

/// An ordered, named collection of heterogeneous [`Node<T>`] columns.
///
/// All columns must have the same length.  Column types are erased via
/// [`AnyNode`] and recovered through [`Frame::get::<T>`].
///
/// # Example
/// ```
/// use vertexrs::{Frame, Node};
///
/// let frame = Frame::new()
///     .append(Node::from_data("price",    vec![10.0_f64, 20.0]))
///     .append(Node::from_data("quantity", vec![2_i32,    5   ]));
///
/// assert_eq!(frame.get::<f64>("price").unwrap(),    &[10.0, 20.0]);
/// assert_eq!(frame.get::<i32>("quantity").unwrap(), &[2, 5]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Frame {
    columns: Vec<(String, AnyNode)>,
}

impl Frame {
    /// Creates an empty frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a column to the frame (builder pattern).
    ///
    /// # Panics
    /// - If a column with the same name already exists.
    /// - If the new column's length differs from existing columns.
    pub fn append(mut self, node: impl Into<AnyNode>) -> Self {
        let any = node.into();
        assert!(
            !self.columns.iter().any(|(n, _)| n == any.name()),
            "Frame already contains a column named '{}'",
            any.name(),
        );
        if let Some((_, first)) = self.columns.first() {
            assert_eq!(
                first.len(),
                any.len(),
                "Frame column length mismatch: existing columns have {} rows but '{}' has {}",
                first.len(),
                any.name(),
                any.len(),
            );
        }
        self.columns.push((any.name().to_owned(), any));
        self
    }

    /// Returns a typed slice for the named column.
    ///
    /// Returns `None` if no column with that name exists, or if the column is
    /// stored under a different native type than `T`.
    pub fn get<T: ArrowBacked>(&self, name: &str) -> Option<&[T]> {
        let (_, any) = self.columns.iter().find(|(n, _)| n == name)?;
        T::try_extract(any)
    }

    /// Returns a reference to the named boolean column, if present.
    ///
    /// Returns `None` if the column does not exist or is not a [`BoolNode`].
    pub fn get_bool(&self, name: &str) -> Option<&BoolNode> {
        self.columns.iter().find_map(|(n, any)| {
            if n == name {
                if let AnyNode::Bool(b) = any {
                    Some(b)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Returns a reference to the named string column, if present.
    ///
    /// Returns `None` if the column does not exist or is not a [`StringNode`].
    pub fn get_str(&self, name: &str) -> Option<&StringNode> {
        self.columns.iter().find_map(|(n, any)| {
            if n == name {
                if let AnyNode::Str(s) = any {
                    Some(s)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Number of rows in each column.  Returns `0` for an empty frame.
    pub fn len(&self) -> usize {
        self.columns.first().map_or(0, |(_, n)| n.len())
    }

    /// Returns `true` if the frame has no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Iterator over column names in insertion order.
    pub fn column_names(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().map(|(n, _)| n.as_str())
    }

    /// Number of columns in the frame.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Appends an [`AnyNode`] to the frame in-place.
    ///
    /// Used internally by macro-generated pipeline code.  Applies the same
    /// duplicate-name and length-mismatch checks as [`append`](Frame::append).
    #[doc(hidden)]
    pub fn push_node(&mut self, any: AnyNode) {
        assert!(
            !self.columns.iter().any(|(n, _)| n == any.name()),
            "Frame already contains a column named '{}'",
            any.name(),
        );
        if let Some((_, first)) = self.columns.first() {
            assert_eq!(
                first.len(),
                any.len(),
                "Frame column length mismatch: existing columns have {} rows but '{}' has {}",
                first.len(),
                any.name(),
                any.len(),
            );
        }
        self.columns.push((any.name().to_owned(), any));
    }
}

// ── ArrowBacked + private::Extract impls ─────────────────────────────────────

macro_rules! impl_arrow_backed {
    ($($native:ty => $arrow:ty, $variant:ident),* $(,)?) => {
        $(
            impl private::Sealed for $native {}
            impl private::Extract for $native {
                fn try_extract(any: &AnyNode) -> Option<&[$native]> {
                    if let AnyNode::$variant(n) = any { Some(n.values()) } else { None }
                }
            }
            impl ArrowBacked for $native {
                type ArrowType = $arrow;
            }
        )*
    };
}

impl_arrow_backed!(
    f16 => Float16Type, F16,
    f32 => Float32Type, F32,
    f64 => Float64Type, F64,
    i8  => Int8Type,    I8,
    i16 => Int16Type,   I16,
    i32 => Int32Type,   I32,
    i64 => Int64Type,   I64,
    u8  => UInt8Type,   U8,
    u16 => UInt16Type,  U16,
    u32 => UInt32Type,  U32,
    u64 => UInt64Type,  U64,
);

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Direct API tests (no macro) ───────────────────────────────────────────

    #[test]
    fn node_row_single_dep() {
        let price = Node::from_data("price", vec![10.0_f64, 20.0, 30.0]);
        let data = price.row(|x| x * 0.2);
        assert_eq!(data, vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn node_col_sort() {
        let price = Node::from_data("price", vec![3.0_f64, 1.0, 2.0]);
        let sorted = price.col(|c| c.sort());
        assert_eq!(sorted, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn node_col_filter() {
        let price = Node::from_data("price", vec![1.0_f64, -2.0, 3.0, -4.0]);
        let positive = price.col(|c| c.filter(|x| *x > 0.0));
        assert_eq!(positive, vec![1.0, 3.0]);
    }

    // ── Macro tests ───────────────────────────────────────────────────────────

    #[test]
    fn macro_row_scalar_op() {
        let price = Node::from_data("price", vec![10.0_f64, 20.0, 30.0]);
        node!(tax = price.row(|x| x * 0.2));
        assert_eq!(tax.values(), [2.0, 4.0, 6.0]);
        assert_eq!(tax.deps, &["price"]);
    }

    #[test]
    fn macro_row_two_nodes() {
        let price = Node::from_data("price", vec![10.0_f64, 20.0, 30.0]);
        node!(tax = price.row(|x| x * 0.2));
        node!(total = price.row(|x| x + tax));
        assert_eq!(total.values(), [12.0, 24.0, 36.0]);
        assert_eq!(total.deps, &["price", "tax"]);
    }

    #[test]
    fn macro_col_sort() {
        let price = Node::from_data("price", vec![3.0_f64, 1.0, 2.0]);
        node!(sorted = price.col(|c| c.sort()));
        assert_eq!(sorted.values(), [1.0, 2.0, 3.0]);
        assert_eq!(sorted.deps, &["price"]);
    }

    #[test]
    fn macro_col_filter() {
        let price = Node::from_data("price", vec![1.0_f64, -2.0, 3.0, -4.0]);
        node!(positive = price.col(|c| c.filter(|x| *x > 0.0)));
        assert_eq!(positive.values(), [1.0, 3.0]);
    }

    #[test]
    fn macro_row_after_col() {
        let price = Node::from_data("price", vec![3.0_f64, 1.0, 2.0]);
        node!(sorted = price.col(|c| c.sort()));
        node!(tax_on_sorted = sorted.row(|x| x * 0.1));
        for (got, expected) in tax_on_sorted.values().iter().zip([0.1, 0.2, 0.3]) {
            assert!((got - expected).abs() < 1e-10, "{got} ≠ {expected}");
        }
    }

    #[test]
    fn macro_row_mixed_types() {
        // Row mode with deps of different native types: prices: f64, flags: i32.
        // The macro generates `let flags = flags.data[__vtx_i]` (i32) while
        // `x` is f64 — Rust's type inference handles the mismatch cleanly.
        let prices = Node::from_data("prices", vec![10.0_f64, 20.0, 30.0]);
        let flags = Node::from_data("flags", vec![1_i32, 0, 1]);
        node!(filtered = prices.row(|x| if flags > 0 { x } else { 0.0 }));
        assert_eq!(filtered.values(), [10.0, 0.0, 30.0]);
        assert_eq!(filtered.deps, &["prices", "flags"]);
    }

    #[test]
    fn macro_col_captures_dep_from_body() {
        // Col closure references `weights` directly (not inside a nested closure),
        // so it should appear in the deps array.
        let values = Node::from_data("values", vec![1.0_f64, 2.0, 3.0, 4.0]);
        let weights = Node::from_data("weights", vec![10.0_f64, 20.0, 30.0, 40.0]);
        node!(
            combined = values.col(|col_v| {
                col_v
                    .data
                    .iter()
                    .zip(weights.data.iter())
                    .map(|(&a, &b)| a + b)
                    .collect::<Vec<_>>()
            })
        );
        assert_eq!(combined.values(), [11.0, 22.0, 33.0, 44.0]);
        assert!(combined.deps.contains(&"values"));
        assert!(combined.deps.contains(&"weights"));
    }

    // ── Arrow interop tests ────────────────────────────────────────────────

    #[test]
    fn arrow_round_trip() {
        let original = Node::from_data("x", vec![1.0_f64, 2.0, 3.0]);
        let arrow_array = original.to_arrow_array();
        let restored = Node::from_arrow_array("x", &arrow_array);
        assert_eq!(original.values(), restored.values());
    }

    #[test]
    fn arrow_buffer_is_shared() {
        // to_arrow_array is zero-copy: both point at the same Arc<Buffer>.
        let node = Node::from_data("x", vec![1.0_f64, 2.0, 3.0]);
        let arr = node.to_arrow_array();
        // The ScalarBuffer inside the PrimitiveArray and node.data share the
        // same allocation; clone of ScalarBuffer is O(1) (Arc bump).
        assert_eq!(arr.values().as_ref(), node.values());
    }

    // ── AnyNode tests ─────────────────────────────────────────────────────────

    #[test]
    fn any_node_name_and_len() {
        let n: AnyNode = Node::from_data("price", vec![1.0_f64, 2.0]).into();
        assert_eq!(n.name(), "price");
        assert_eq!(n.len(), 2);
        assert!(!n.is_empty());
    }

    #[test]
    fn any_node_data_type() {
        let f32_node: AnyNode = Node::from_data("a", vec![1.0_f32]).into();
        let i64_node: AnyNode = Node::from_data("b", vec![1_i64]).into();
        assert_eq!(f32_node.data_type(), "f32");
        assert_eq!(i64_node.data_type(), "i64");
    }

    #[test]
    fn any_node_is_empty() {
        let empty: AnyNode = Node::from_data("e", Vec::<f64>::new()).into();
        assert!(empty.is_empty());
    }

    // ── Frame tests ───────────────────────────────────────────────────────────

    #[test]
    fn frame_basic_get() {
        let frame = Frame::new()
            .append(Node::from_data("price", vec![10.0_f64, 20.0]))
            .append(Node::from_data("qty", vec![2_i32, 5]));

        assert_eq!(frame.get::<f64>("price").unwrap(), &[10.0, 20.0]);
        assert_eq!(frame.get::<i32>("qty").unwrap(), &[2, 5]);
    }

    #[test]
    fn frame_get_wrong_type_returns_none() {
        let frame = Frame::new().append(Node::from_data("x", vec![1.0_f64]));
        assert!(frame.get::<f32>("x").is_none());
    }

    #[test]
    fn frame_get_missing_column_returns_none() {
        let frame = Frame::new().append(Node::from_data("x", vec![1.0_f64]));
        assert!(frame.get::<f64>("y").is_none());
    }

    #[test]
    fn frame_len_and_column_count() {
        let frame = Frame::new()
            .append(Node::from_data("a", vec![1.0_f64, 2.0, 3.0]))
            .append(Node::from_data("b", vec![10_u32, 20, 30]));
        assert_eq!(frame.len(), 3);
        assert_eq!(frame.column_count(), 2);
        assert!(!frame.is_empty());
    }

    #[test]
    fn frame_empty() {
        let frame = Frame::new();
        assert_eq!(frame.len(), 0);
        assert_eq!(frame.column_count(), 0);
        assert!(frame.is_empty());
    }

    #[test]
    fn frame_column_names() {
        let frame = Frame::new()
            .append(Node::from_data("x", vec![1.0_f64]))
            .append(Node::from_data("y", vec![2_i32]));
        let names: Vec<&str> = frame.column_names().collect();
        assert_eq!(names, vec!["x", "y"]);
    }

    #[test]
    #[should_panic(expected = "Frame already contains a column named 'x'")]
    fn frame_add_duplicate_panics() {
        Frame::new()
            .append(Node::from_data("x", vec![1.0_f64]))
            .append(Node::from_data("x", vec![2.0_f64]));
    }

    #[test]
    #[should_panic(expected = "Frame column length mismatch")]
    fn frame_add_length_mismatch_panics() {
        Frame::new()
            .append(Node::from_data("a", vec![1.0_f64, 2.0]))
            .append(Node::from_data("b", vec![1_i32]));
    }

    // ── Macro tests: Frame multi-column row mode ──────────────────────────────

    #[test]
    fn macro_frame_row_multi_col() {
        let frame = Frame::new()
            .append(Node::from_data("price", vec![10.0_f64, 20.0, 30.0]))
            .append(Node::from_data("qty", vec![2_i64, 3, 4]));
        node!(revenue = frame.row(|price: f64, qty: i64| price * qty as f64));
        assert_eq!(revenue.values(), [20.0, 60.0, 120.0]);
        assert_eq!(revenue.deps, &["price", "qty"]);
    }

    // ── Macro tests: Frame col mode ───────────────────────────────────────────

    #[test]
    fn macro_frame_col_typed() {
        let frame = Frame::new().append(Node::from_data("price", vec![3.0_f64, 1.0, 2.0]));
        node!(sorted = frame.col(|price: f64| price.sort()));
        assert_eq!(sorted.values(), [1.0, 2.0, 3.0]);
        assert_eq!(sorted.deps, &["price"]);
    }

    // ── Pipeline tests ────────────────────────────────────────────────────────

    #[test]
    fn pipeline_basic_round_trip() {
        let frame = Frame::new().append(Node::from_data("price", vec![10.0_f64, 20.0, 30.0]));

        let mut p = pipeline! {
            source!(price: f64);
            node!(tax = price.row(|x| x * 0.1));
            output!(tax)
        };

        p.push(&frame);
        p.compute().unwrap();
        let out = p.output();
        assert_eq!(out.get::<f64>("tax").unwrap(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn pipeline_multi_source_and_derived() {
        let frame = Frame::new()
            .append(Node::from_data("price", vec![10.0_f64, 20.0, 30.0]))
            .append(Node::from_data("qty", vec![2_i32, 3, 4]));

        let mut p = pipeline! {
            source!(price: f64, qty: i32);
            node!(tax   = price.row(|x| x * 0.1));
            node!(total = price.row(|x| x + tax));
            output!(tax, total)
        };

        p.push(&frame);
        p.compute().unwrap();
        let out = p.output();
        assert_eq!(out.get::<f64>("tax").unwrap(), &[1.0, 2.0, 3.0]);
        assert_eq!(out.get::<f64>("total").unwrap(), &[11.0, 22.0, 33.0]);
        // `qty` was declared as a source but not in output — should be absent.
        assert!(out.get::<i32>("qty").is_none());
    }

    #[test]
    fn pipeline_output_is_only_declared_columns() {
        let frame = Frame::new().append(Node::from_data("x", vec![1.0_f64, 2.0]));

        let mut p = pipeline! {
            source!(x: f64);
            node!(y = x.row(|v| v * 2.0));
            node!(z = x.row(|v| v * 3.0));
            output!(y)   // z is computed but not exposed
        };

        p.push(&frame);
        p.compute().unwrap();
        let out = p.output();
        assert_eq!(out.get::<f64>("y").unwrap(), &[2.0, 4.0]);
        assert!(out.get::<f64>("z").is_none());
        assert_eq!(out.column_count(), 1);
    }

    #[test]
    fn pipeline_missing_source_returns_error() {
        let frame = Frame::new(); // no columns

        let mut p = pipeline! {
            source!(price: f64);
            node!(tax = price.row(|x| x * 0.1));
            output!(tax)
        };

        p.push(&frame);
        let err = p.compute().unwrap_err();
        assert!(matches!(err, PipelineError::MissingSource("price")));
    }

    #[test]
    fn pipeline_push_then_recompute() {
        let frame1 = Frame::new().append(Node::from_data("x", vec![1.0_f64, 2.0]));
        let frame2 = Frame::new().append(Node::from_data("x", vec![10.0_f64, 20.0]));

        let mut p = pipeline! {
            source!(x: f64);
            node!(doubled = x.row(|v| v * 2.0));
            output!(doubled)
        };

        p.push(&frame1);
        p.compute().unwrap();
        assert_eq!(p.output().get::<f64>("doubled").unwrap(), &[2.0, 4.0]);

        p.push(&frame2);
        p.compute().unwrap();
        assert_eq!(p.output().get::<f64>("doubled").unwrap(), &[20.0, 40.0]);
    }

    // ── Nested pipeline! tests ────────────────────────────────────────────────

    #[test]
    fn pipeline_nested_basic() {
        let frame = Frame::new().append(Node::from_data("price", vec![10.0_f64, 20.0, 40.0]));

        let mut p = pipeline! {
            source!(price: f64);
            pipeline!(normaliser {
                source!(price: f64);
                node!(norm = price.row(|x| x / 10.0));
                output!(norm)
            });
            node!(tax = normaliser.row(|norm: f64| norm * 0.5));
            output!(tax)
        };

        p.push(&frame);
        p.compute().unwrap();
        // norm = [1.0, 2.0, 4.0]; tax = norm * 0.5 = [0.5, 1.0, 2.0]
        assert_eq!(p.output().get::<f64>("tax").unwrap(), &[0.5, 1.0, 2.0]);
    }

    #[test]
    fn pipeline_nested_internal_nodes_invisible() {
        // Internal nodes of the nested pipeline are not in its output Frame.
        let frame = Frame::new().append(Node::from_data("price", vec![10.0_f64, 20.0]));

        let mut p = pipeline! {
            source!(price: f64);
            pipeline!(sub_p {
                source!(price: f64);
                node!(internal = price.row(|x| x * 999.0)); // not in output!
                node!(visible  = price.row(|x| x * 2.0));
                output!(visible)
            });
            node!(out = sub_p.row(|visible: f64| visible + 1.0));
            output!(out)
        };

        p.push(&frame);
        p.compute().unwrap();
        // visible = [20.0, 40.0]; out = visible + 1.0 = [21.0, 41.0]
        assert_eq!(p.output().get::<f64>("out").unwrap(), &[21.0, 41.0]);
        // "internal" must not appear in sub_p's output
        assert!(
            p.output().get::<f64>("internal").is_none(),
            "internal node should not be visible from parent output"
        );
    }

    #[test]
    fn pipeline_nested_isolate_failure_parent_continues() {
        let frame = Frame::new().append(Node::from_data("price", vec![10.0_f64, 20.0]));

        let mut p = pipeline! {
            source!(price: f64);
            pipeline!(failing_sub {
                settings { failure: Isolate }
                source!(missing_col: f64); // missing from frame — will error
                node!(out = missing_col.row(|x| x * 2.0));
                output!(out)
            });
            // `result` doesn't depend on failing_sub
            node!(result = price.row(|x| x * 3.0));
            output!(result)
        };

        p.push(&frame);
        p.compute().unwrap(); // must NOT propagate the nested error
        assert_eq!(p.output().get::<f64>("result").unwrap(), &[30.0, 60.0]);
        assert_eq!(p.errors().len(), 1, "isolated error should be recorded");
    }

    // ── sub! tests ────────────────────────────────────────────────────────────

    fn make_normaliser() -> Pipeline {
        pipeline! {
            source!(price: f64);
            node!(norm = price.row(|x| x / 100.0));
            output!(norm)
        }
    }

    #[test]
    fn pipeline_sub_basic() {
        let frame = Frame::new().append(Node::from_data("price", vec![100.0_f64, 200.0, 400.0]));

        let mut p = pipeline! {
            source!(price: f64);
            sub!(make_normaliser() => norm: f64);
            node!(tax = norm.row(|x| x * 0.5));
            output!(tax)
        };

        p.push(&frame);
        p.compute().unwrap();
        // norm = [1.0, 2.0, 4.0]; tax = norm * 0.5 = [0.5, 1.0, 2.0]
        assert_eq!(p.output().get::<f64>("tax").unwrap(), &[0.5, 1.0, 2.0]);
    }

    #[test]
    fn pipeline_sub_multiple_outputs() {
        fn make_two_outputs() -> Pipeline {
            pipeline! {
                source!(price: f64);
                node!(doubled = price.row(|x| x * 2.0));
                node!(tripled = price.row(|x| x * 3.0));
                output!(doubled, tripled)
            }
        }

        let frame = Frame::new().append(Node::from_data("price", vec![10.0_f64, 20.0]));

        let mut p = pipeline! {
            source!(price: f64);
            sub!(make_two_outputs() => doubled: f64, tripled: f64);
            node!(sum = doubled.row(|x| x + tripled));
            output!(sum)
        };

        p.push(&frame);
        p.compute().unwrap();
        // doubled=[20,40], tripled=[30,60], sum=[50,100]
        assert_eq!(p.output().get::<f64>("sum").unwrap(), &[50.0, 100.0]);
    }

    #[test]
    fn pipeline_two_independent_subs() {
        fn make_doubler() -> Pipeline {
            pipeline! { source!(x: f64); node!(d = x.row(|v| v * 2.0)); output!(d) }
        }
        fn make_tripler() -> Pipeline {
            pipeline! { source!(x: f64); node!(t = x.row(|v| v * 3.0)); output!(t) }
        }

        let frame = Frame::new().append(Node::from_data("x", vec![10.0_f64, 20.0]));

        let mut p = pipeline! {
            source!(x: f64);
            sub!(make_doubler() => d: f64);
            sub!(make_tripler() => t: f64);
            node!(sum = d.row(|v| v + t));
            output!(sum)
        };

        p.push(&frame);
        p.compute().unwrap();
        // d=[20,40], t=[30,60], sum=[50,100]
        assert_eq!(p.output().get::<f64>("sum").unwrap(), &[50.0, 100.0]);
    }

    #[test]
    fn pipeline_sub_missing_output_returns_error() {
        fn wrong_sub() -> Pipeline {
            pipeline! {
                source!(price: f64);
                node!(out = price.row(|x| x));
                output!(out) // exports "out", not "missing"
            }
        }

        let frame = Frame::new().append(Node::from_data("price", vec![1.0_f64]));

        let mut p = pipeline! {
            source!(price: f64);
            sub!(wrong_sub() => missing: f64); // "missing" absent from sub output
            output!(missing)
        };

        p.push(&frame);
        let err = p.compute().unwrap_err();
        assert!(
            matches!(err, PipelineError::MissingSource("missing")),
            "expected MissingSource(\"missing\"), got {err:?}"
        );
    }

    // ── Phase 2.5: BoolNode ───────────────────────────────────────────────────

    #[test]
    fn bool_node_from_data_round_trip() {
        let b = BoolNode::from_data("mask", vec![true, false, true]);
        assert_eq!(b.name, "mask");
        assert_eq!(b.len(), 3);
        assert!(!b.is_empty());
        assert_eq!(b.to_vec(), vec![true, false, true]);
    }

    #[test]
    fn bool_node_value_indexing() {
        let b = BoolNode::from_data("flags", vec![false, true, false, true]);
        assert_eq!(b.value(0), false);
        assert_eq!(b.value(1), true);
        assert_eq!(b.value(3), true);
    }

    #[test]
    fn bool_node_is_empty() {
        let b = BoolNode::from_data("empty", vec![]);
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn bool_node_new_with_deps() {
        let b = BoolNode::new_with_deps("gt", &["price"], vec![true, false]);
        assert_eq!(b.name, "gt");
        assert_eq!(b.deps, &["price"]);
        assert_eq!(b.to_vec(), vec![true, false]);
    }

    // ── Phase 2.5: StringNode ─────────────────────────────────────────────────

    #[test]
    fn string_node_from_data_round_trip() {
        let s = StringNode::from_data("labels", &["foo", "bar", "baz"]);
        assert_eq!(s.name, "labels");
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
        assert_eq!(s.values(), vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn string_node_value_indexing() {
        let s = StringNode::from_data("tags", &["alpha", "beta", "gamma"]);
        assert_eq!(s.value(0), "alpha");
        assert_eq!(s.value(2), "gamma");
    }

    #[test]
    fn string_node_is_empty() {
        let s = StringNode::from_data("empty", &[]);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn string_node_new_with_deps() {
        let s = StringNode::new_with_deps("label", &["id"], vec!["x".to_string(), "y".to_string()]);
        assert_eq!(s.name, "label");
        assert_eq!(s.deps, &["id"]);
        assert_eq!(s.values(), vec!["x", "y"]);
    }

    // ── Phase 2.5: AnyNode Bool/Str variants ─────────────────────────────────

    #[test]
    fn any_node_bool_metadata() {
        let b = BoolNode::from_data("mask", vec![true, false]);
        let any: AnyNode = b.into();
        assert_eq!(any.name(), "mask");
        assert_eq!(any.len(), 2);
        assert!(!any.is_empty());
        assert_eq!(any.data_type(), "bool");
    }

    #[test]
    fn any_node_str_metadata() {
        let s = StringNode::from_data("labels", &["a", "b", "c"]);
        let any: AnyNode = s.into();
        assert_eq!(any.name(), "labels");
        assert_eq!(any.len(), 3);
        assert!(!any.is_empty());
        assert_eq!(any.data_type(), "str");
    }

    // ── Phase 2.5: Frame::get_bool / get_str ─────────────────────────────────

    #[test]
    fn frame_get_bool_happy_path() {
        let b = BoolNode::from_data("active", vec![true, false, true]);
        let frame = Frame::new().append(b);
        let got = frame
            .get_bool("active")
            .expect("get_bool should return Some");
        assert_eq!(got.to_vec(), vec![true, false, true]);
    }

    #[test]
    fn frame_get_bool_missing_column() {
        let frame = Frame::new();
        assert!(frame.get_bool("missing").is_none());
    }

    #[test]
    fn frame_get_bool_wrong_type() {
        let n = Node::from_data("price", vec![1.0_f64]);
        let frame = Frame::new().append(n);
        // Column exists but is not a BoolNode.
        assert!(frame.get_bool("price").is_none());
    }

    #[test]
    fn frame_get_str_happy_path() {
        let s = StringNode::from_data("names", &["alice", "bob"]);
        let frame = Frame::new().append(s);
        let got = frame.get_str("names").expect("get_str should return Some");
        assert_eq!(got.values(), vec!["alice", "bob"]);
    }

    #[test]
    fn frame_get_str_missing_column() {
        let frame = Frame::new();
        assert!(frame.get_str("missing").is_none());
    }

    #[test]
    fn frame_get_str_wrong_type() {
        let b = BoolNode::from_data("mask", vec![true]);
        let frame = Frame::new().append(b);
        // Column exists but is not a StringNode.
        assert!(frame.get_str("mask").is_none());
    }

    // ── Phase 2.5: node! macro -> bool output ────────────────────────────────

    #[test]
    #[allow(unused_braces)] // syn requires a block body for closures with explicit return types
    fn node_macro_bool_output_row_mode() {
        let price = Node::from_data("price", vec![90.0_f64, 100.0, 110.0]);
        node!(above = price.row(|x| -> bool { x > 100.0 }));
        assert_eq!(above.name, "above");
        assert_eq!(above.deps, &["price"]);
        assert_eq!(above.to_vec(), vec![false, false, true]);
    }

    #[test]
    #[allow(unused_braces)] // syn requires a block body for closures with explicit return types
    fn node_macro_bool_output_uses_extra_dep() {
        // The body references a second node; the macro should collect it as a dep.
        let a = Node::from_data("a", vec![1.0_f64, 2.0, 3.0]);
        let b = Node::from_data("b", vec![1.5_f64, 1.5, 3.5]);
        node!(gt = a.row(|x| -> bool { x > b }));
        assert_eq!(gt.deps, &["a", "b"]);
        assert_eq!(gt.to_vec(), vec![false, true, false]);
    }

    #[test]
    #[allow(unused_braces)] // syn requires a block body for closures with explicit return types
    fn node_macro_bool_output_without_annotation_still_compiles() {
        // Without `-> bool`, the macro uses Node::new_with_deps, which would fail
        // to compile for a bool-valued body (bool: !ArrowNativeType).  This test
        // verifies the annotated path compiles and yields the correct result.
        let vals = Node::from_data("v", vec![0_i32, 1, 2, 3]);
        node!(nonzero = vals.row(|x| -> bool { x != 0 }));
        assert_eq!(nonzero.to_vec(), vec![false, true, true, true]);
    }
}
