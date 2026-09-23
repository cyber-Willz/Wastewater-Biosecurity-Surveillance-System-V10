//! A minimal matrix-free linear operator abstraction.
//!
//! Large hypergraphs make materializing an `n x n` dense Laplacian
//! wasteful or outright infeasible. [`LinearOperator`] lets the spectral
//! routines in [`crate::spectral`] work against any symmetric operator that
//! can apply itself to a vector in `O(nnz)`, without ever forming the dense
//! matrix. [`crate::laplacian::HypergraphOperator`] is the primary
//! implementer.

use nalgebra::DVector;

/// A symmetric linear operator on `R^n`, applied via matrix-vector products.
///
/// Implementations must be symmetric (`<Ax, y> == <x, Ay>`) for the Lanczos
/// routines in [`crate::spectral`] to produce meaningful results; this is a
/// documented precondition, not something enforced at the type level.
pub trait LinearOperator {
    /// Dimension `n` of the operator (it maps `R^n -> R^n`).
    fn dim(&self) -> usize;

    /// Apply the operator to `x`, returning `A * x`.
    fn apply(&self, x: &DVector<f64>) -> DVector<f64>;
}

/// Wraps a dense [`nalgebra::DMatrix`] so it can be used wherever a
/// [`LinearOperator`] is expected (handy for testing spectral routines
/// against a known-correct dense reference).
pub struct DenseOperator<'a> {
    matrix: &'a nalgebra::DMatrix<f64>,
}

impl<'a> DenseOperator<'a> {
    /// Wrap a square dense matrix as a [`LinearOperator`].
    pub fn new(matrix: &'a nalgebra::DMatrix<f64>) -> Self {
        assert_eq!(matrix.nrows(), matrix.ncols(), "operator must be square");
        Self { matrix }
    }
}

impl<'a> LinearOperator for DenseOperator<'a> {
    fn dim(&self) -> usize {
        self.matrix.nrows()
    }

    fn apply(&self, x: &DVector<f64>) -> DVector<f64> {
        self.matrix * x
    }
}
