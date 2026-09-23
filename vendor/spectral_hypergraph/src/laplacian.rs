//! Laplacian construction for [`SpectralHypergraph`].
//!
//! Implements the normalized hypergraph Laplacian of Zhou, Huang &
//! Schölkopf (NeurIPS 2006):
//!
//! ```text
//! Delta = I - D_v^{-1/2} H W D_e^{-1} H^T D_v^{-1/2}
//! ```
//!
//! where `H` is the `|V| x |E|` incidence matrix, `W` is the diagonal
//! hyperedge-weight matrix, `D_v` is the diagonal weighted vertex-degree
//! matrix, and `D_e` is the diagonal hyperedge-cardinality (incidence-sum)
//! matrix. `Delta` is symmetric positive semi-definite with smallest
//! eigenvalue `0` (achieved by `D_v^{1/2} * 1`), exactly mirroring the
//! normalized graph Laplacian it generalizes.
//!
//! Two access patterns are provided:
//!
//! * [`HypergraphOperator`] — a matrix-free [`crate::operator::LinearOperator`]
//!   that applies `Delta` in `O(nnz(H))` per matvec without ever forming an
//!   `n x n` matrix. Use this (via [`crate::spectral::lanczos_smallest`]) for
//!   large hypergraphs.
//! * [`dense_normalized_laplacian`] / [`dense_incidence_matrix`] /
//!   [`clique_expansion_adjacency`] — dense [`nalgebra::DMatrix`] builders,
//!   convenient for small hypergraphs or for cross-checking the matrix-free
//!   path in tests.

use nalgebra::{DMatrix, DVector};

use crate::error::{HypergraphError, Result};
use crate::hypergraph::SpectralHypergraph;
use crate::operator::LinearOperator;

/// Matrix-free normalized hypergraph Laplacian operator.
///
/// Precomputes `D_v^{-1/2}`, `D_e^{-1}`, and CSR-style adjacency lists once;
/// every subsequent [`LinearOperator::apply`] call is `O(nnz(H))` with no
/// further allocation of `n x n` structures.
pub struct HypergraphOperator {
    dim: usize,
    vertex_inv_sqrt_deg: Vec<f64>,
    /// For each vertex: list of (hyperedge index, incidence weight).
    incidence_by_vertex: Vec<Vec<(usize, f64)>>,
    /// For each hyperedge: list of (vertex index, incidence weight).
    incidence_by_edge: Vec<Vec<(usize, f64)>>,
    /// Precomputed `w(e) / d_e(e)` per hyperedge.
    edge_weight_over_degree: Vec<f64>,
}

impl HypergraphOperator {
    /// Build the operator for `hg`. Fails if any vertex is isolated (degree
    /// zero), since `D_v^{-1/2}` is then undefined, or if `hg` has no
    /// hyperedges.
    pub fn new(hg: &SpectralHypergraph) -> Result<Self> {
        if hg.num_hyperedges() == 0 {
            return Err(HypergraphError::EmptyHyperEdgeSet);
        }

        let n = hg.num_vertices();
        let m = hg.num_hyperedges();

        let mut incidence_by_vertex: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut incidence_by_edge: Vec<Vec<(usize, f64)>> = vec![Vec::new(); m];

        for e in hg.hyperedge_ids() {
            for v in hg.hyperedge_members(e)? {
                let w = hg.incidence_weight(v, e)?;
                incidence_by_vertex[v.0].push((e.0, w));
                incidence_by_edge[e.0].push((v.0, w));
            }
        }

        let mut vertex_inv_sqrt_deg = vec![0.0; n];
        for v in hg.vertex_ids() {
            let deg = hg.vertex_degree(v)?;
            if deg <= 0.0 {
                return Err(HypergraphError::IsolatedVertex(
                    hg.vertex_label(v)?.to_string(),
                ));
            }
            vertex_inv_sqrt_deg[v.0] = 1.0 / deg.sqrt();
        }

        let mut edge_weight_over_degree = vec![0.0; m];
        for e in hg.hyperedge_ids() {
            let d_e = hg.hyperedge_degree(e)? as f64;
            if d_e <= 0.0 {
                // Guarded against by the >=2-member invariant enforced at
                // construction time, but checked defensively here.
                return Err(HypergraphError::DegenerateHyperEdge(0));
            }
            edge_weight_over_degree[e.0] = hg.hyperedge_weight(e)? / d_e;
        }

        Ok(Self {
            dim: n,
            vertex_inv_sqrt_deg,
            incidence_by_vertex,
            incidence_by_edge,
            edge_weight_over_degree,
        })
    }
}

/// Below this combined vertex+hyperedge count, parallel dispatch (thread
/// pool scheduling, work splitting) costs more than it saves versus the
/// plain sequential loop; `apply` falls back to the serial path regardless
/// of the `parallel` feature. Chosen conservatively -- large enough that
/// per-vertex/per-hyperedge work is unlikely to be swamped by rayon's
/// per-call overhead on typical hardware, not tuned to a specific machine.
#[cfg(feature = "parallel")]
const PARALLEL_THRESHOLD: usize = 50_000;

impl LinearOperator for HypergraphOperator {
    fn dim(&self) -> usize {
        self.dim
    }

    fn apply(&self, x: &DVector<f64>) -> DVector<f64> {
        debug_assert_eq!(x.len(), self.dim);

        #[cfg(feature = "parallel")]
        {
            if self.dim + self.incidence_by_edge.len() >= PARALLEL_THRESHOLD {
                return self.apply_parallel(x);
            }
        }
        self.apply_serial(x)
    }
}

impl HypergraphOperator {
    /// Sequential matvec: `y = (I - D_v^{-1/2} H W D_e^{-1} H^T D_v^{-1/2}) x`.
    /// Always available; used directly when the `parallel` feature is off,
    /// and as the fallback below [`PARALLEL_THRESHOLD`] when it's on.
    fn apply_serial(&self, x: &DVector<f64>) -> DVector<f64> {
        // z = D_v^{-1/2} .* x
        let z: Vec<f64> = (0..self.dim)
            .map(|v| self.vertex_inv_sqrt_deg[v] * x[v])
            .collect();

        // u[e] = sum_v H(v,e) * z[v]
        let mut u = vec![0.0; self.incidence_by_edge.len()];
        for (e_idx, members) in self.incidence_by_edge.iter().enumerate() {
            let mut acc = 0.0;
            for &(v_idx, w) in members {
                acc += w * z[v_idx];
            }
            u[e_idx] = acc * self.edge_weight_over_degree[e_idx];
        }

        // w[v] = sum_e H(v,e) * u[e]
        let mut w_out = vec![0.0; self.dim];
        for (v_idx, edges) in self.incidence_by_vertex.iter().enumerate() {
            let mut acc = 0.0;
            for &(e_idx, w) in edges {
                acc += w * u[e_idx];
            }
            w_out[v_idx] = acc;
        }

        // y = x - D_v^{-1/2} .* w_out
        let y: Vec<f64> = (0..self.dim)
            .map(|v| x[v] - self.vertex_inv_sqrt_deg[v] * w_out[v])
            .collect();

        DVector::from_vec(y)
    }

    /// Same computation as [`Self::apply_serial`], but the two `O(nnz(H))`
    /// reduction loops (vertices -> hyperedges, hyperedges -> vertices) are
    /// each split across rayon's global thread pool via `par_iter`. Only
    /// compiled in behind the `parallel` feature.
    #[cfg(feature = "parallel")]
    fn apply_parallel(&self, x: &DVector<f64>) -> DVector<f64> {
        use rayon::prelude::*;

        // z = D_v^{-1/2} .* x
        let z: Vec<f64> = (0..self.dim)
            .into_par_iter()
            .map(|v| self.vertex_inv_sqrt_deg[v] * x[v])
            .collect();

        // u[e] = (w(e) / d_e(e)) * sum_v H(v,e) * z[v]
        let u: Vec<f64> = self
            .incidence_by_edge
            .par_iter()
            .enumerate()
            .map(|(e_idx, members)| {
                let acc: f64 = members.iter().map(|&(v_idx, w)| w * z[v_idx]).sum();
                acc * self.edge_weight_over_degree[e_idx]
            })
            .collect();

        // y[v] = x[v] - D_v^{-1/2}[v] * sum_e H(v,e) * u[e]
        let y: Vec<f64> = self
            .incidence_by_vertex
            .par_iter()
            .enumerate()
            .map(|(v_idx, edges)| {
                let acc: f64 = edges.iter().map(|&(e_idx, w)| w * u[e_idx]).sum();
                x[v_idx] - self.vertex_inv_sqrt_deg[v_idx] * acc
            })
            .collect();

        DVector::from_vec(y)
    }
}

/// Dense `|V| x |E|` incidence matrix `H`, where `H[v, e]` is the incidence
/// weight of vertex `v` in hyperedge `e` (`0.0` if not a member).
///
/// Intended for small hypergraphs, debugging, and tests. For large
/// hypergraphs prefer [`HypergraphOperator`], which never materializes an
/// `n x m` (let alone `n x n`) dense matrix.
pub fn dense_incidence_matrix(hg: &SpectralHypergraph) -> Result<DMatrix<f64>> {
    let n = hg.num_vertices();
    let m = hg.num_hyperedges();
    let mut h = DMatrix::<f64>::zeros(n, m);
    for e in hg.hyperedge_ids() {
        for v in hg.hyperedge_members(e)? {
            h[(v.0, e.0)] = hg.incidence_weight(v, e)?;
        }
    }
    Ok(h)
}

/// Dense normalized hypergraph Laplacian
/// `Delta = I - D_v^{-1/2} H W D_e^{-1} H^T D_v^{-1/2}`.
///
/// Errors if any vertex is isolated or the hypergraph has no hyperedges.
/// For hypergraphs beyond a few thousand vertices, use
/// [`HypergraphOperator`] with the iterative solvers in
/// [`crate::spectral`] instead of materializing this `n x n` matrix.
pub fn dense_normalized_laplacian(hg: &SpectralHypergraph) -> Result<DMatrix<f64>> {
    if hg.num_hyperedges() == 0 {
        return Err(HypergraphError::EmptyHyperEdgeSet);
    }
    let n = hg.num_vertices();
    let m = hg.num_hyperedges();

    let h = dense_incidence_matrix(hg)?;

    let mut dv_inv_sqrt = DMatrix::<f64>::zeros(n, n);
    for v in hg.vertex_ids() {
        let deg = hg.vertex_degree(v)?;
        if deg <= 0.0 {
            return Err(HypergraphError::IsolatedVertex(
                hg.vertex_label(v)?.to_string(),
            ));
        }
        dv_inv_sqrt[(v.0, v.0)] = 1.0 / deg.sqrt();
    }

    let mut w = DMatrix::<f64>::zeros(m, m);
    let mut de_inv = DMatrix::<f64>::zeros(m, m);
    for e in hg.hyperedge_ids() {
        w[(e.0, e.0)] = hg.hyperedge_weight(e)?;
        let d_e = hg.hyperedge_degree(e)? as f64;
        de_inv[(e.0, e.0)] = 1.0 / d_e;
    }

    let core = &dv_inv_sqrt * &h * &w * &de_inv * h.transpose() * &dv_inv_sqrt;
    let identity = DMatrix::<f64>::identity(n, n);
    Ok(identity - core)
}

/// Dense clique-expansion adjacency matrix: each hyperedge `e` of weight
/// `w(e)` and cardinality `|e|` contributes `w(e) / (|e| - 1)` to the
/// adjacency of every pair of its member vertices. This is the classical
/// "flatten the hypergraph into an ordinary weighted graph" reduction,
/// provided for comparison against the normalized hypergraph Laplacian and
/// for interop with plain-graph tooling.
pub fn clique_expansion_adjacency(hg: &SpectralHypergraph) -> Result<DMatrix<f64>> {
    let n = hg.num_vertices();
    let mut adj = DMatrix::<f64>::zeros(n, n);
    for e in hg.hyperedge_ids() {
        let members = hg.hyperedge_members(e)?;
        let card = members.len();
        if card < 2 {
            continue;
        }
        let share = hg.hyperedge_weight(e)? / (card as f64 - 1.0);
        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                let (a, b) = (members[i].0, members[j].0);
                adj[(a, b)] += share;
                adj[(b, a)] += share;
            }
        }
    }
    Ok(adj)
}

/// Dense unnormalized clique-expansion graph Laplacian `L = D - A`, built
/// from [`clique_expansion_adjacency`]. Useful as a classical baseline
/// against the normalized hypergraph Laplacian, and as a Fiedler-vector
/// source for hypergraph partitioning that some pipelines prefer over the
/// normalized variant.
pub fn clique_expansion_laplacian(hg: &SpectralHypergraph) -> Result<DMatrix<f64>> {
    let adj = clique_expansion_adjacency(hg)?;
    let n = adj.nrows();
    let mut degrees = DVector::<f64>::zeros(n);
    for i in 0..n {
        degrees[i] = adj.row(i).sum();
    }
    let d = DMatrix::from_diagonal(&degrees);
    Ok(d - adj)
}

/// Convenience: weighted degree vector `D_v` as a dense diagonal-friendly
/// [`DVector`], in vertex-id order.
pub fn vertex_degree_vector(hg: &SpectralHypergraph) -> Result<DVector<f64>> {
    let n = hg.num_vertices();
    let mut degrees = DVector::<f64>::zeros(n);
    for v in hg.vertex_ids() {
        degrees[v.0] = hg.vertex_degree(v)?;
    }
    Ok(degrees)
}

/// Convenience: hyperedge cardinality vector `D_e`, in hyperedge-id order.
pub fn hyperedge_degree_vector(hg: &SpectralHypergraph) -> Result<DVector<f64>> {
    let m = hg.num_hyperedges();
    let mut degrees = DVector::<f64>::zeros(m);
    for e in hg.hyperedge_ids() {
        degrees[e.0] = hg.hyperedge_degree(e)? as f64;
    }
    Ok(degrees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypergraph::HypergraphBuilder;
    use approx::assert_relative_eq;

    fn triangle_plus_edge() -> SpectralHypergraph {
        // Hyperedge {a,b,c} and a normal pairwise edge {c,d} (as a 2-member
        // hyperedge), matching the classic small example used in hypergraph
        // spectral clustering papers.
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let c = b.add_vertex("c").unwrap();
        let d = b.add_vertex("d").unwrap();
        b.add_hyperedge(&[a, v, c], 1.0).unwrap();
        b.add_hyperedge(&[c, d], 1.0).unwrap();
        b.build().unwrap()
    }

    #[test]
    fn dense_laplacian_is_symmetric_and_psd_diag() {
        let hg = triangle_plus_edge();
        let l = dense_normalized_laplacian(&hg).unwrap();
        assert_relative_eq!(l.clone(), l.transpose(), epsilon = 1e-10);
        // Diagonal of a normalized Laplacian lies in [0, 1] (in fact here
        // it's exactly 1 - 1/d(v) * sum_e w(e)/d_e(e) restricted to v, so
        // strictly it's in (0, 1]).
        for i in 0..l.nrows() {
            assert!(l[(i, i)] >= -1e-9 && l[(i, i)] <= 1.0 + 1e-9);
        }
    }

    #[test]
    fn matrix_free_operator_matches_dense_laplacian() {
        let hg = triangle_plus_edge();
        let dense = dense_normalized_laplacian(&hg).unwrap();
        let op = HypergraphOperator::new(&hg).unwrap();

        for i in 0..hg.num_vertices() {
            let mut e_i = DVector::<f64>::zeros(hg.num_vertices());
            e_i[i] = 1.0;
            let via_op = op.apply(&e_i);
            let via_dense = &dense * &e_i;
            assert_relative_eq!(via_op, via_dense, epsilon = 1e-10);
        }
    }

    #[test]
    fn isolated_vertex_rejected() {
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        b.add_vertex("iso").unwrap();
        b.add_hyperedge(&[a, v], 1.0).unwrap();
        let hg = b.build().unwrap();
        assert!(matches!(
            HypergraphOperator::new(&hg),
            Err(HypergraphError::IsolatedVertex(_))
        ));
        assert!(matches!(
            dense_normalized_laplacian(&hg),
            Err(HypergraphError::IsolatedVertex(_))
        ));
    }

    #[test]
    #[cfg(feature = "parallel")]
    fn parallel_matvec_matches_serial() {
        // Exercises `apply_parallel` directly (below the size threshold
        // `apply` would otherwise route through `apply_serial`), against a
        // hypergraph large enough to touch multiple hyperedges per vertex.
        let mut b = HypergraphBuilder::new();
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(b.add_vertex(format!("v{i}")).unwrap());
        }
        // A ring of overlapping triples guarantees every vertex is a member
        // of at least one hyperedge (no isolated vertices), plus a handful
        // of extra chord hyperedges so several vertices sit in more than
        // one hyperedge.
        for i in 0..40 {
            let members = [ids[i], ids[(i + 1) % 40], ids[(i + 2) % 40]];
            b.add_hyperedge(&members, 1.0 + i as f64 * 0.05).unwrap();
        }
        for e in 0..10 {
            let members = [ids[e * 3 % 40], ids[(e * 3 + 11) % 40], ids[(e * 3 + 23) % 40]];
            b.add_hyperedge(&members, 0.5).unwrap();
        }
        let hg = b.build().unwrap();
        let op = HypergraphOperator::new(&hg).unwrap();

        for seed in 0..5u64 {
            let mut x = DVector::<f64>::zeros(hg.num_vertices());
            let mut state = seed.wrapping_mul(2654435761).wrapping_add(1);
            for i in 0..x.len() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                x[i] = ((state >> 33) as f64 / u32::MAX as f64) - 0.5;
            }
            let serial = op.apply_serial(&x);
            let parallel = op.apply_parallel(&x);
            assert_relative_eq!(serial, parallel, epsilon = 1e-10);
        }
    }

    #[test]
    fn clique_expansion_matches_hand_computation() {
        let hg = triangle_plus_edge();
        let adj = clique_expansion_adjacency(&hg).unwrap();
        // {a,b,c} hyperedge: each pair gets weight 1/(3-1) = 0.5
        assert_relative_eq!(adj[(0, 1)], 0.5, epsilon = 1e-12);
        assert_relative_eq!(adj[(0, 2)], 0.5, epsilon = 1e-12);
        assert_relative_eq!(adj[(1, 2)], 0.5, epsilon = 1e-12);
        // {c,d} hyperedge: weight 1/(2-1) = 1.0
        assert_relative_eq!(adj[(2, 3)], 1.0, epsilon = 1e-12);
        assert_relative_eq!(adj[(0, 3)], 0.0, epsilon = 1e-12);
    }
}
