//! Error types for the spectral hypergraph crate.

use thiserror::Error;

/// Errors that can occur while building or operating on a [`crate::hypergraph::SpectralHypergraph`].
#[derive(Debug, Error, Clone, PartialEq)]
pub enum HypergraphError {
    /// A hyperedge was declared with fewer than 2 distinct member vertices.
    #[error("hyperedge must contain at least 2 distinct vertices, got {0}")]
    DegenerateHyperEdge(usize),

    /// A vertex label used when constructing a hyperedge does not exist.
    #[error("unknown vertex label: {0:?}")]
    UnknownVertex(String),

    /// A vertex or hyperedge index was out of bounds for the current graph.
    #[error("index {index} out of bounds (len = {len})")]
    IndexOutOfBounds {
        /// The offending index.
        index: usize,
        /// The valid length at the time of the call.
        len: usize,
    },

    /// A weight parameter was invalid (e.g. negative or non-finite).
    #[error("invalid weight {0}: weights must be finite and non-negative")]
    InvalidWeight(f64),

    /// Attempted to build a hypergraph with zero vertices.
    #[error("hypergraph must contain at least one vertex")]
    EmptyVertexSet,

    /// Attempted to run a spectral routine on a hypergraph with no hyperedges.
    #[error("operation requires at least one hyperedge")]
    EmptyHyperEdgeSet,

    /// A vertex has zero weighted degree, so `D_v^{-1/2}` is undefined.
    #[error("vertex {0:?} is isolated (zero degree); normalize with `drop_isolated` or add incident hyperedges")]
    IsolatedVertex(String),

    /// A requested spectral computation asked for more eigenpairs than exist.
    #[error("requested {requested} eigenpairs but the operator has dimension {dimension}")]
    TooManyEigenpairsRequested {
        /// Number of eigenpairs requested.
        requested: usize,
        /// Dimension of the underlying operator.
        dimension: usize,
    },

    /// An iterative eigensolver failed to converge within its iteration budget.
    #[error("eigensolver failed to converge after {iterations} iterations (residual {residual:.3e}, tolerance {tolerance:.3e})")]
    ConvergenceFailure {
        /// Iterations attempted.
        iterations: usize,
        /// Final residual norm achieved.
        residual: f64,
        /// Requested tolerance.
        tolerance: f64,
    },

    /// A duplicate vertex label was inserted.
    #[error("duplicate vertex label: {0:?}")]
    DuplicateVertex(String),

    /// A [`crate::directed::DirectedHypergraph`] hyperedge was declared with
    /// an empty tail and/or head vertex set (both must be non-empty).
    #[error("directed hyperedge must have a non-empty tail and head, got tail={tail_len}, head={head_len}")]
    DegenerateDirectedHyperEdge {
        /// Tail (source) set size after deduplication.
        tail_len: usize,
        /// Head (target) set size after deduplication.
        head_len: usize,
    },

    /// A requested cluster count was invalid for spectral clustering.
    #[error("invalid cluster count {k}: must satisfy 1 <= k <= n ({n})")]
    InvalidClusterCount {
        /// Requested number of clusters.
        k: usize,
        /// Number of points available to cluster.
        n: usize,
    },
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, HypergraphError>;
