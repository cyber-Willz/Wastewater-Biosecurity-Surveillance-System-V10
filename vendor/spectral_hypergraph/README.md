# spectral_hypergraph

A production-oriented **spectral hypergraph** data structure for Rust.

A hypergraph generalizes a graph by letting an "edge" connect any number
(>= 2) of vertices instead of exactly two. This crate represents a
hypergraph as a **bipartite graph** — vertices and hyperedges are both
nodes, joined when a vertex is a member of a hyperedge — built directly on
top of [`petgraph`](https://docs.rs/petgraph)'s `UnGraph`. That's the
"using a graph data structure" part: rather than hand-rolling incidence
storage, hypergraph structure is graph structure, so all of `petgraph`'s
graph machinery (traversal, `NodeIndex`, etc.) is available on it directly
via `SpectralHypergraph::bipartite_graph()`.

On top of that data structure sit two layers of spectral machinery:

- **`laplacian`** — the normalized hypergraph Laplacian of Zhou, Huang &
  Schölkopf (NeurIPS 2006), `Delta = I - D_v^{-1/2} H W D_e^{-1} H^T D_v^{-1/2}`,
  available both as a dense `nalgebra::DMatrix` and as a **matrix-free**
  `LinearOperator` that applies `Delta` in `O(nnz(H))` per vector without
  ever forming an `n x n` matrix. Also includes the classical
  clique-expansion reduction to an ordinary weighted graph.
- **`spectral`** — dense eigen-decomposition (`nalgebra::SymmetricEigen`),
  a matrix-free Lanczos solver with full reorthogonalization for large
  sparse hypergraphs, the Fiedler vector (algebraic connectivity /
  bipartitioning signal), and spectral clustering (k-means over the
  Laplacian embedding, k-means++ initialized).

## Why this design

- **Correctness first**: every fallible operation returns
  `Result<_, HypergraphError>` — degenerate hyperedges (< 2 members),
  duplicate vertex labels, out-of-range ids, isolated vertices (where
  `D_v^{-1/2}` is undefined), and eigensolver non-convergence are all
  reported explicitly, never silently ignored or papered over with panics.
- **Scales**: the matrix-free `HypergraphOperator` + `lanczos_smallest`
  path never materializes an `n x n` matrix, so spectral analysis of
  large, sparse hypergraphs stays `O(nnz)` per iteration rather than
  `O(n^2)` or `O(n^3)`. The dense path (`dense_normalized_laplacian` +
  `dense_eigen`) remains available for small hypergraphs where its
  simplicity and robustness are preferable.
- **Thread-safe by construction**: `SpectralHypergraph` has no interior
  mutability and no removal API, so `VertexId`/`HyperEdgeId` stay stable
  and the whole structure is `Clone + Send + Sync` — share it behind an
  `Arc` across threads without locking.
- **Validated construction**: `HypergraphBuilder` rejects invariant
  violations (degenerate hyperedges, duplicate labels, non-finite/negative
  weights, references to nonexistent vertices) at build time rather than
  producing a hypergraph that later panics or produces silently wrong
  spectral results.

## Quick start

```rust
use spectral_hypergraph::hypergraph::HypergraphBuilder;
use spectral_hypergraph::spectral::{fiedler_vector, spectral_cluster};

let mut b = HypergraphBuilder::new();
let alice = b.add_vertex("alice").unwrap();
let bob = b.add_vertex("bob").unwrap();
let carol = b.add_vertex("carol").unwrap();
let dave = b.add_vertex("dave").unwrap();

b.add_hyperedge(&[alice, bob, carol], 1.0).unwrap(); // e.g. co-authors on a paper
b.add_hyperedge(&[carol, dave], 0.1).unwrap();        // a weak cross-team link

let hg = b.build().unwrap();

let fiedler = fiedler_vector(&hg).unwrap();
let clusters = spectral_cluster(&hg, 2, /* use_lanczos */ false, /* seed */ 42).unwrap();
```

Run `cargo run --example basic_usage` for a fuller worked example
(co-authorship hypergraph → degrees → Laplacian → Fiedler vector →
spectral clustering → clique-expansion adjacency).

## Choosing dense vs. matrix-free

| Hypergraph size            | Use                                                        |
|-----------------------------|-------------------------------------------------------------|
| Up to a few thousand vertices | `dense_normalized_laplacian` + `dense_eigen`               |
| Large / sparse               | `HypergraphOperator` + `lanczos_smallest` (or `spectral_cluster(.., use_lanczos: true, ..)`) |

Both paths are cross-checked against each other in the test suite
(`tests/integration_test.rs`, `laplacian::tests::matrix_free_operator_matches_dense_laplacian`).

## Module layout

```
src/
  error.rs       HypergraphError, Result
  hypergraph.rs  SpectralHypergraph, HypergraphBuilder, VertexId, HyperEdgeId
  operator.rs    LinearOperator trait (+ DenseOperator adapter for testing)
  laplacian.rs   HypergraphOperator, dense_normalized_laplacian,
                 dense_incidence_matrix, clique_expansion_adjacency/laplacian
  spectral.rs    dense_eigen, lanczos_smallest, fiedler_vector, spectral_cluster
```

## Testing

```
cargo test              # 15 unit tests + 4 integration tests + 2 doctests
cargo run --example basic_usage
```

## License

MIT OR Apache-2.0
