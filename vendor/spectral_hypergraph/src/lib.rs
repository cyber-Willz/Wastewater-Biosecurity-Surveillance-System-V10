//! # spectral_hypergraph
//!
//! A production-oriented **spectral hypergraph** data structure for Rust.
//!
//! A hypergraph generalizes a graph by letting each "edge" connect any
//! number (>= 2) of vertices. This crate represents a hypergraph as a
//! **bipartite graph** (vertices and hyperedges are both nodes, connected
//! when a vertex is a member of a hyperedge) built on top of
//! [`petgraph`](https://docs.rs/petgraph)'s `UnGraph`, and layers on:
//!
//! * A validated, immutable [`hypergraph::SpectralHypergraph`] type built
//!   via [`hypergraph::HypergraphBuilder`] (duplicate-label detection,
//!   degenerate-hyperedge rejection, weight validation).
//! * [`laplacian`] -- the normalized hypergraph Laplacian of Zhou, Huang &
//!   Schoelkopf (2006), available both as a dense [`nalgebra::DMatrix`] and
//!   as a matrix-free [`operator::LinearOperator`] that never materializes
//!   an `n x n` matrix, plus the classical clique-expansion reduction to an
//!   ordinary weighted graph.
//! * [`spectral`] -- dense eigen-decomposition, a matrix-free Lanczos
//!   solver (full reorthogonalization) for large sparse hypergraphs, the
//!   Fiedler vector, and spectral clustering (k-means on the Laplacian
//!   embedding).
//! * [`directed`] -- a weighted, **directed** hyperedge variant
//!   ([`directed::DirectedHypergraph`]), where each hyperedge has a tail
//!   (source) and head (target) vertex set rather than a single undirected
//!   member set.
//! * [`sparse`] -- CSR export of the incidence matrix
//!   ([`sparse::incidence_matrix_csr`]) for interop with other sparse
//!   linear algebra crates, without ever materializing a dense matrix.
//!
//! ## Optional features
//!
//! * `serde-support` -- [`serde::Serialize`]/[`serde::Deserialize`] for
//!   [`hypergraph::SpectralHypergraph`] via a small, stable JSON-friendly
//!   schema (not a derive on the internal `petgraph` representation).
//! * `parallel` -- routes [`laplacian::HypergraphOperator::apply`] through
//!   [`rayon`] for large hypergraphs (see the `laplacian` module docs for
//!   the size threshold below which it stays sequential).
//! * `sprs-interop` -- [`sparse::incidence_matrix_sprs`], building an actual
//!   [`sprs::CsMat<f64>`] instead of the dependency-free [`sparse::CsrMatrix`].
//!
//! ## Quick start
//!
//! ```
//! use spectral_hypergraph::hypergraph::HypergraphBuilder;
//! use spectral_hypergraph::spectral::fiedler_vector;
//!
//! let mut b = HypergraphBuilder::new();
//! let a = b.add_vertex("a").unwrap();
//! let v = b.add_vertex("b").unwrap();
//! let c = b.add_vertex("c").unwrap();
//! let d = b.add_vertex("d").unwrap();
//! b.add_hyperedge(&[a, v, c], 1.0).unwrap();
//! b.add_hyperedge(&[c, d], 1.0).unwrap();
//! let hg = b.build().unwrap();
//!
//! let fiedler = fiedler_vector(&hg).unwrap();
//! assert_eq!(fiedler.len(), 4);
//! ```
//!
//! ## Design notes / production considerations
//!
//! * **Immutability**: [`hypergraph::SpectralHypergraph`] has no vertex or
//!   hyperedge *removal* API. This is deliberate -- it keeps `VertexId` /
//!   `HyperEdgeId` stable for the object's lifetime and keeps the type
//!   trivially `Send + Sync` (no interior mutability, so it is safe to
//!   share behind an `Arc` across threads without locking). Build a new
//!   hypergraph if you need to remove elements.
//! * **Numerical scale**: use [`laplacian::dense_normalized_laplacian`] +
//!   [`spectral::dense_eigen`] for hypergraphs up to a few thousand
//!   vertices; beyond that, use [`laplacian::HypergraphOperator`] +
//!   [`spectral::lanczos_smallest`], which is `O(nnz)` per iteration and
//!   never forms an `n x n` matrix.
//! * **Error handling**: every fallible operation returns
//!   [`error::HypergraphError`] via [`error::Result`] rather than panicking
//!   (isolated vertices, degenerate hyperedges, out-of-range ids, and
//!   eigensolver non-convergence are all reported, not silently ignored).

pub mod directed;
pub mod error;
pub mod hypergraph;
pub mod laplacian;
pub mod operator;
pub mod sparse;
pub mod spectral;

pub use error::{HypergraphError, Result};
pub use hypergraph::{HyperEdgeId, HypergraphBuilder, SpectralHypergraph, VertexId};
pub use operator::LinearOperator;
