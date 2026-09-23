//! Spectral analysis routines: eigen-decomposition, the Fiedler vector, and
//! spectral clustering.
//!
//! Two eigensolver entry points are provided:
//!
//! * [`dense_eigen`] — wraps [`nalgebra::linalg::SymmetricEigen`] for an
//!   explicit dense `n x n` matrix. Simple, robust, `O(n^3)`. Fine up to a
//!   few thousand vertices.
//! * [`lanczos_smallest`] — matrix-free Lanczos iteration against any
//!   [`crate::operator::LinearOperator`] (in particular
//!   [`crate::laplacian::HypergraphOperator`]), with full reorthogonalization
//!   for numerical stability. Finds the `k` algebraically smallest
//!   eigenpairs in `O(k * iterations * nnz)` without ever forming an `n x n`
//!   matrix — the path to use for large, sparse hypergraphs.

use nalgebra::{DMatrix, DVector};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rand::Rng;

use crate::error::{HypergraphError, Result};
use crate::hypergraph::SpectralHypergraph;
use crate::laplacian::{dense_normalized_laplacian, HypergraphOperator};
use crate::operator::LinearOperator;

/// Eigenvalues (ascending) and corresponding eigenvectors (as columns) of a
/// dense symmetric matrix.
#[derive(Debug, Clone)]
pub struct EigenDecomposition {
    /// Eigenvalues, ascending.
    pub eigenvalues: DVector<f64>,
    /// Eigenvectors as columns, ordered to match `eigenvalues`.
    pub eigenvectors: DMatrix<f64>,
}

/// Full dense symmetric eigen-decomposition, sorted ascending by eigenvalue.
///
/// `matrix` must be symmetric (not checked beyond a debug assertion — pass
/// the output of [`crate::laplacian::dense_normalized_laplacian`] or similar).
pub fn dense_eigen(matrix: &DMatrix<f64>) -> EigenDecomposition {
    debug_assert_eq!(matrix.nrows(), matrix.ncols());
    let eig = nalgebra::linalg::SymmetricEigen::new(matrix.clone());

    let n = eig.eigenvalues.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        eig.eigenvalues[a]
            .partial_cmp(&eig.eigenvalues[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut eigenvalues = DVector::<f64>::zeros(n);
    let mut eigenvectors = DMatrix::<f64>::zeros(n, n);
    for (new_idx, &old_idx) in order.iter().enumerate() {
        eigenvalues[new_idx] = eig.eigenvalues[old_idx];
        eigenvectors.set_column(new_idx, &eig.eigenvectors.column(old_idx));
    }

    EigenDecomposition {
        eigenvalues,
        eigenvectors,
    }
}

/// Matrix-free Lanczos iteration for the `k` algebraically smallest
/// eigenpairs of a symmetric [`LinearOperator`].
///
/// Runs up to `max_iter` Lanczos steps (capped at the operator's dimension)
/// with full reorthogonalization, diagonalizes the resulting tridiagonal
/// matrix, and returns the `k` Ritz pairs with smallest Ritz value. Returns
/// [`HypergraphError::ConvergenceFailure`] if, after `max_iter` steps, any
/// of the `k` requested Ritz pairs has residual `||A y - lambda y||` above
/// `tol`.
///
/// `seed` makes the (randomized) starting vector reproducible.
pub fn lanczos_smallest(
    op: &dyn LinearOperator,
    k: usize,
    max_iter: usize,
    tol: f64,
    seed: u64,
) -> Result<EigenDecomposition> {
    let n = op.dim();
    if k == 0 || k > n {
        return Err(HypergraphError::TooManyEigenpairsRequested {
            requested: k,
            dimension: n,
        });
    }

    let steps = max_iter.min(n);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);

    let mut start = DVector::<f64>::zeros(n);
    for i in 0..n {
        start[i] = rng.gen_range(-1.0..1.0);
    }
    let norm = start.norm();
    if norm < f64::EPSILON {
        start[0] = 1.0;
    } else {
        start /= norm;
    }

    // Full-reorthogonalization Lanczos.
    let mut basis: Vec<DVector<f64>> = Vec::with_capacity(steps);
    let mut alphas: Vec<f64> = Vec::with_capacity(steps);
    let mut betas: Vec<f64> = Vec::with_capacity(steps.saturating_sub(1));

    basis.push(start.clone());
    let mut w = op.apply(&basis[0]);
    let mut alpha = basis[0].dot(&w);
    alphas.push(alpha);
    w -= &basis[0] * alpha;

    let mut effective_steps = 1usize;
    for _ in 1..steps {
        let beta = w.norm();
        if beta < 1e-12 {
            break; // Krylov subspace exhausted (invariant subspace found).
        }
        let mut v_next = &w / beta;
        // Full reorthogonalization against the whole basis so far.
        for v_prev in &basis {
            let proj = v_next.dot(v_prev);
            v_next -= v_prev * proj;
        }
        let renorm = v_next.norm();
        if renorm < 1e-12 {
            break;
        }
        v_next /= renorm;

        w = op.apply(&v_next);
        alpha = v_next.dot(&w);
        w -= &v_next * alpha;
        w -= &basis[basis.len() - 1] * beta;

        betas.push(beta);
        alphas.push(alpha);
        basis.push(v_next);
        effective_steps += 1;
    }

    // Build and diagonalize the (small) tridiagonal matrix T.
    let m = effective_steps;
    let mut t = DMatrix::<f64>::zeros(m, m);
    for i in 0..m {
        t[(i, i)] = alphas[i];
    }
    for i in 0..m.saturating_sub(1) {
        t[(i, i + 1)] = betas[i];
        t[(i + 1, i)] = betas[i];
    }
    let t_eig = dense_eigen(&t);

    // Ritz vectors: V (n x m) * (eigenvectors of T, m x m) -> n x m.
    let v_matrix = DMatrix::from_columns(&basis);
    let k = k.min(m);
    let mut ritz_values = DVector::<f64>::zeros(k);
    let mut ritz_vectors = DMatrix::<f64>::zeros(n, k);
    let mut max_residual = 0.0f64;
    for i in 0..k {
        ritz_values[i] = t_eig.eigenvalues[i];
        let y = &v_matrix * t_eig.eigenvectors.column(i);
        let y_norm = y.norm();
        let y = if y_norm > 1e-12 { &y / y_norm } else { y };
        let residual = (op.apply(&y) - &y * ritz_values[i]).norm();
        max_residual = max_residual.max(residual);
        ritz_vectors.set_column(i, &y);
    }

    if max_residual > tol {
        return Err(HypergraphError::ConvergenceFailure {
            iterations: effective_steps,
            residual: max_residual,
            tolerance: tol,
        });
    }

    Ok(EigenDecomposition {
        eigenvalues: ritz_values,
        eigenvectors: ritz_vectors,
    })
}

/// The Fiedler vector of the normalized hypergraph Laplacian: the
/// eigenvector associated with the smallest *nonzero* eigenvalue
/// (algebraic connectivity). Small values vs. large values in the returned
/// vector indicate the two sides of the sparsest normalized cut, exactly as
/// for the classical graph Fiedler vector.
///
/// Uses [`lanczos_smallest`] under the hood (matrix-free, so this scales to
/// large hypergraphs). For hypergraphs with more than one connected
/// component the two smallest eigenvalues can both be (numerically) zero;
/// in that regime prefer inspecting the full spectrum via
/// [`dense_eigen`]`(&`[`crate::laplacian::dense_normalized_laplacian`]`(hg)?)`.
pub fn fiedler_vector(hg: &SpectralHypergraph) -> Result<DVector<f64>> {
    let op = HypergraphOperator::new(hg)?;
    let n = op.dim();
    if n < 2 {
        return Err(HypergraphError::TooManyEigenpairsRequested {
            requested: 2,
            dimension: n,
        });
    }
    let max_iter = (2 * n).clamp(20, 500);
    let decomp = lanczos_smallest(&op, 2.min(n), max_iter, 1e-6, 0xF1ED1E5)?;
    Ok(decomp.eigenvectors.column(decomp.eigenvalues.len() - 1).into_owned())
}

/// Result of [`spectral_cluster`]: a cluster assignment per vertex (indexed
/// by [`crate::hypergraph::VertexId`]`.0`) plus the spectral embedding used.
#[derive(Debug, Clone)]
pub struct ClusterAssignment {
    /// `assignments[v.0]` is the cluster index (`0..k`) assigned to vertex `v`.
    pub assignments: Vec<usize>,
    /// The `n x k_embed` spectral embedding that was clustered (rows =
    /// vertices, columns = the smallest nontrivial Laplacian eigenvectors
    /// used as coordinates).
    pub embedding: DMatrix<f64>,
}

/// Spectral clustering of the hypergraph's vertices into `k` clusters.
///
/// Follows the standard recipe (Ng-Jordan-Weiss, generalized to
/// hypergraphs by Zhou et al.): embed each vertex using the `k` smallest
/// eigenvectors of the normalized hypergraph Laplacian (including the
/// trivial constant eigenvector, which contributes no separating signal but
/// is harmless), row-normalize, then run k-means.
///
/// For small-to-medium hypergraphs (say, up to a few thousand vertices)
/// this uses the dense eigensolver for robustness; pass `use_lanczos: true`
/// to instead use the matrix-free [`lanczos_smallest`] path for large
/// hypergraphs.
pub fn spectral_cluster(
    hg: &SpectralHypergraph,
    k: usize,
    use_lanczos: bool,
    seed: u64,
) -> Result<ClusterAssignment> {
    let n = hg.num_vertices();
    if k == 0 || k > n {
        return Err(HypergraphError::InvalidClusterCount { k, n });
    }

    let embedding = if use_lanczos {
        let op = HypergraphOperator::new(hg)?;
        let max_iter = (4 * k).max(30).min(n).max(k);
        let decomp = lanczos_smallest(&op, k, max_iter.max(k), 1e-6, seed)?;
        decomp.eigenvectors
    } else {
        let laplacian = dense_normalized_laplacian(hg)?;
        let decomp = dense_eigen(&laplacian);
        decomp.eigenvectors.columns(0, k).into_owned()
    };

    // Row-normalize each vertex's embedding to unit length (standard
    // normalized-spectral-clustering preprocessing; guards against the
    // degree-weighted scaling baked into the normalized Laplacian's
    // eigenvectors).
    let mut normalized = embedding.clone();
    for i in 0..normalized.nrows() {
        let row_norm = normalized.row(i).norm();
        if row_norm > 1e-12 {
            for j in 0..normalized.ncols() {
                normalized[(i, j)] /= row_norm;
            }
        }
    }

    let assignments = kmeans(&normalized, k, seed, 100)?;

    Ok(ClusterAssignment {
        assignments,
        embedding,
    })
}

/// Lloyd's-algorithm k-means with k-means++ initialization, operating on
/// the rows of `points` (`n x d`). Returns a cluster index per row.
fn kmeans(points: &DMatrix<f64>, k: usize, seed: u64, max_iter: usize) -> Result<Vec<usize>> {
    let n = points.nrows();
    if k == 0 || k > n {
        return Err(HypergraphError::InvalidClusterCount { k, n });
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0xC1057E12);

    // k-means++ initialization.
    let mut centroids: Vec<DVector<f64>> = Vec::with_capacity(k);
    let first = rng.gen_range(0..n);
    centroids.push(points.row(first).transpose());
    while centroids.len() < k {
        let dists: Vec<f64> = (0..n)
            .map(|i| {
                let p = points.row(i).transpose();
                centroids
                    .iter()
                    .map(|c| (&p - c).norm_squared())
                    .fold(f64::INFINITY, f64::min)
            })
            .collect();
        let total: f64 = dists.iter().sum();
        if total <= 0.0 {
            // All remaining points coincide with an existing centroid;
            // pick arbitrarily to keep k distinct centroids.
            let idx = rng.gen_range(0..n);
            centroids.push(points.row(idx).transpose());
            continue;
        }
        let mut target = rng.gen_range(0.0..total);
        let mut chosen = n - 1;
        for (i, &d) in dists.iter().enumerate() {
            if target <= d {
                chosen = i;
                break;
            }
            target -= d;
        }
        centroids.push(points.row(chosen).transpose());
    }

    let mut assignments = vec![0usize; n];
    for _ in 0..max_iter {
        let mut changed = false;
        for i in 0..n {
            let p = points.row(i).transpose();
            let mut best = 0usize;
            let mut best_dist = f64::INFINITY;
            for (c_idx, c) in centroids.iter().enumerate() {
                let d = (&p - c).norm_squared();
                if d < best_dist {
                    best_dist = d;
                    best = c_idx;
                }
            }
            if assignments[i] != best {
                changed = true;
            }
            assignments[i] = best;
        }

        let dim = points.ncols();
        let mut sums = vec![DVector::<f64>::zeros(dim); k];
        let mut counts = vec![0usize; k];
        for i in 0..n {
            let c = assignments[i];
            sums[c] += points.row(i).transpose();
            counts[c] += 1;
        }
        for c in 0..k {
            if counts[c] > 0 {
                centroids[c] = &sums[c] / counts[c] as f64;
            }
        }

        if !changed {
            break;
        }
    }

    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypergraph::HypergraphBuilder;
    use approx::assert_relative_eq;

    #[test]
    fn dense_eigen_sorted_ascending() {
        let m = DMatrix::from_row_slice(3, 3, &[2.0, 0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0]);
        let decomp = dense_eigen(&m);
        assert_relative_eq!(decomp.eigenvalues[0], 1.0, epsilon = 1e-9);
        assert_relative_eq!(decomp.eigenvalues[1], 2.0, epsilon = 1e-9);
        assert_relative_eq!(decomp.eigenvalues[2], 5.0, epsilon = 1e-9);
    }

    #[test]
    fn lanczos_matches_dense_on_small_operator() {
        let m = DMatrix::from_row_slice(
            4,
            4,
            &[
                4.0, 1.0, 0.0, 0.0, //
                1.0, 3.0, 1.0, 0.0, //
                0.0, 1.0, 2.0, 1.0, //
                0.0, 0.0, 1.0, 1.0,
            ],
        );
        let op = crate::operator::DenseOperator::new(&m);
        let dense = dense_eigen(&m);
        let lanczos = lanczos_smallest(&op, 4, 20, 1e-8, 42).unwrap();
        for i in 0..4 {
            assert_relative_eq!(lanczos.eigenvalues[i], dense.eigenvalues[i], epsilon = 1e-6);
        }
    }

    fn two_clique_hypergraph() -> SpectralHypergraph {
        // Two well-separated triangles {a,b,c} and {d,e,f}, connected by a
        // single weak bridge hyperedge {c,d}. Spectral clustering with
        // k=2 should cleanly separate {a,b,c} from {d,e,f}.
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let c = b.add_vertex("c").unwrap();
        let d = b.add_vertex("d").unwrap();
        let e = b.add_vertex("e").unwrap();
        let f = b.add_vertex("f").unwrap();
        b.add_hyperedge(&[a, v, c], 5.0).unwrap();
        b.add_hyperedge(&[d, e, f], 5.0).unwrap();
        b.add_hyperedge(&[c, d], 0.1).unwrap();
        b.build().unwrap()
    }

    #[test]
    fn fiedler_vector_separates_two_cliques() {
        let hg = two_clique_hypergraph();
        let fiedler = fiedler_vector(&hg).unwrap();
        let left_sign = fiedler[0].signum();
        assert_eq!(fiedler[1].signum(), left_sign);
        assert_eq!(fiedler[2].signum(), left_sign);
        let right_sign = fiedler[3].signum();
        assert_eq!(fiedler[4].signum(), right_sign);
        assert_eq!(fiedler[5].signum(), right_sign);
        assert_ne!(left_sign, right_sign);
    }

    #[test]
    fn spectral_cluster_recovers_two_cliques() {
        let hg = two_clique_hypergraph();
        let result = spectral_cluster(&hg, 2, false, 7).unwrap();
        assert_eq!(result.assignments[0], result.assignments[1]);
        assert_eq!(result.assignments[1], result.assignments[2]);
        assert_eq!(result.assignments[3], result.assignments[4]);
        assert_eq!(result.assignments[4], result.assignments[5]);
        assert_ne!(result.assignments[0], result.assignments[3]);
    }

    #[test]
    fn spectral_cluster_rejects_bad_k() {
        let hg = two_clique_hypergraph();
        assert!(matches!(
            spectral_cluster(&hg, 0, false, 1),
            Err(HypergraphError::InvalidClusterCount { .. })
        ));
        assert!(matches!(
            spectral_cluster(&hg, 100, false, 1),
            Err(HypergraphError::InvalidClusterCount { .. })
        ));
    }
}
