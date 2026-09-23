//! Sparse (CSR) export of a hypergraph's incidence matrix, for interop with
//! the wider Rust sparse-linear-algebra ecosystem.
//!
//! [`crate::laplacian::dense_incidence_matrix`] materializes the `|V| x |E|`
//! incidence matrix `H` as a dense [`nalgebra::DMatrix`], which is fine for
//! small hypergraphs but wasteful once `H` is genuinely sparse (the common
//! case: a vertex is usually a member of only a handful of hyperedges out
//! of possibly many). [`incidence_matrix_csr`] instead builds `H` directly
//! in [compressed sparse row][csr] form, without ever allocating the dense
//! `n x m` matrix.
//!
//! [csr]: https://en.wikipedia.org/wiki/Sparse_matrix#Compressed_sparse_row_(CSR,_CRS_or_Yale_format)
//!
//! [`CsrMatrix`] is a small, dependency-free struct usable on its own (e.g.
//! to hand-roll a matvec, or to serialize the three arrays into whatever
//! format a downstream sparse solver expects). With the `sprs-interop`
//! feature enabled, [`incidence_matrix_sprs`] additionally builds an actual
//! [`sprs::CsMat<f64>`] for direct use with the
//! [`sprs`](https://docs.rs/sprs) sparse linear algebra crate (solvers,
//! sparse matrix products, format conversions, etc).

use crate::error::Result;
use crate::hypergraph::SpectralHypergraph;

/// A minimal, dependency-free compressed-sparse-row matrix: three parallel
/// arrays in the standard CSR layout (`row_ptr` has `rows + 1` entries;
/// `col_indices`/`values` each have `nnz` entries; row `i`'s nonzeros are
/// `col_indices[row_ptr[i]..row_ptr[i+1]]` paired with
/// `values[row_ptr[i]..row_ptr[i+1]]`, sorted by column within each row).
///
/// This is the same layout expected by essentially every sparse linear
/// algebra library's CSR constructor (including [`sprs::CsMat::new`] when
/// the `sprs-interop` feature is enabled) -- the fields are public so
/// callers can hand them off directly without copying through an
/// intermediate representation.
#[derive(Debug, Clone, PartialEq)]
pub struct CsrMatrix {
    /// `(rows, cols)`.
    pub shape: (usize, usize),
    /// Row pointer array, length `rows + 1`.
    pub row_ptr: Vec<usize>,
    /// Column index per nonzero, length `nnz`.
    pub col_indices: Vec<usize>,
    /// Value per nonzero, length `nnz`, parallel to `col_indices`.
    pub values: Vec<f64>,
}

impl CsrMatrix {
    /// Number of structurally nonzero entries.
    pub fn nnz(&self) -> usize {
        self.values.len()
    }

    /// `(rows, cols)`.
    pub fn shape(&self) -> (usize, usize) {
        self.shape
    }

    /// Dense-matvec-equivalent `y = A * x`, computed directly against the
    /// CSR arrays in `O(nnz)` without ever forming a dense matrix. Panics
    /// if `x.len() != self.shape.1`.
    pub fn matvec(&self, x: &[f64]) -> Vec<f64> {
        assert_eq!(
            x.len(),
            self.shape.1,
            "matvec: x has length {} but matrix has {} columns",
            x.len(),
            self.shape.1
        );
        let mut y = vec![0.0; self.shape.0];
        for row in 0..self.shape.0 {
            let start = self.row_ptr[row];
            let end = self.row_ptr[row + 1];
            let mut acc = 0.0;
            for i in start..end {
                acc += self.values[i] * x[self.col_indices[i]];
            }
            y[row] = acc;
        }
        y
    }

    /// The transpose, in CSR form (i.e. `self^T` re-expressed row-major --
    /// equivalent to converting `self` from CSR to CSC without relabeling).
    /// `O(nnz + rows + cols)`.
    pub fn transpose(&self) -> CsrMatrix {
        let (rows, cols) = self.shape;
        let nnz = self.nnz();

        // Counting sort of nonzeros by (new) row = old column.
        let mut counts = vec![0usize; cols + 1];
        for &c in &self.col_indices {
            counts[c + 1] += 1;
        }
        for i in 0..cols {
            counts[i + 1] += counts[i];
        }
        let row_ptr = counts.clone();

        let mut col_indices = vec![0usize; nnz];
        let mut values = vec![0.0; nnz];
        let mut cursor = counts;
        for old_row in 0..rows {
            let start = self.row_ptr[old_row];
            let end = self.row_ptr[old_row + 1];
            for i in start..end {
                let old_col = self.col_indices[i];
                let dest = cursor[old_col];
                col_indices[dest] = old_row;
                values[dest] = self.values[i];
                cursor[old_col] += 1;
            }
        }

        CsrMatrix {
            shape: (cols, rows),
            row_ptr,
            col_indices,
            values,
        }
    }
}

/// Build the `|V| x |E|` incidence matrix `H` of `hg` directly in CSR form
/// (row = vertex, column = hyperedge, value = incidence weight), without
/// materializing a dense `n x m` matrix. `O(nnz(H))` time and space.
///
/// Equivalent in content to [`crate::laplacian::dense_incidence_matrix`],
/// but sparse; prefer this for hypergraphs where most vertices belong to
/// only a small fraction of the hyperedges.
pub fn incidence_matrix_csr(hg: &SpectralHypergraph) -> Result<CsrMatrix> {
    let n = hg.num_vertices();
    let m = hg.num_hyperedges();

    // Column indices per row (vertex), built by walking hyperedges once
    // (matches the incidence-collection pattern in
    // `laplacian::HypergraphOperator::new`) rather than calling
    // `incident_hyperedges` per vertex, to keep this O(nnz) rather than
    // O(nnz + n * avg_hyperedges_per_vertex).
    let mut per_row: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for e in hg.hyperedge_ids() {
        for v in hg.hyperedge_members(e)? {
            let w = hg.incidence_weight(v, e)?;
            per_row[v.0].push((e.0, w));
        }
    }

    let mut row_ptr = Vec::with_capacity(n + 1);
    let mut col_indices = Vec::new();
    let mut values = Vec::new();
    row_ptr.push(0);
    for row in per_row.iter_mut() {
        // CSR conventionally keeps column indices sorted within each row;
        // `hyperedge_members` iterates hyperedges in id order already, so
        // `row` is already sorted by hyperedge id here, but sort defensively
        // in case that iteration order contract ever changes.
        row.sort_unstable_by_key(|&(e_idx, _)| e_idx);
        for &(e_idx, w) in row.iter() {
            col_indices.push(e_idx);
            values.push(w);
        }
        row_ptr.push(col_indices.len());
    }

    Ok(CsrMatrix {
        shape: (n, m),
        row_ptr,
        col_indices,
        values,
    })
}

/// As [`incidence_matrix_csr`], but returns an actual [`sprs::CsMat<f64>`]
/// for direct use with the `sprs` sparse linear algebra crate. Only
/// compiled in behind the `sprs-interop` feature.
#[cfg(feature = "sprs-interop")]
pub fn incidence_matrix_sprs(hg: &SpectralHypergraph) -> Result<sprs::CsMat<f64>> {
    let csr = incidence_matrix_csr(hg)?;
    Ok(csr.into())
}

#[cfg(feature = "sprs-interop")]
impl From<CsrMatrix> for sprs::CsMat<f64> {
    fn from(csr: CsrMatrix) -> Self {
        sprs::CsMat::new(csr.shape, csr.row_ptr, csr.col_indices, csr.values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypergraph::HypergraphBuilder;
    use crate::laplacian::dense_incidence_matrix;
    use approx::assert_relative_eq;

    fn triangle_plus_edge() -> SpectralHypergraph {
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let c = b.add_vertex("c").unwrap();
        let d = b.add_vertex("d").unwrap();
        b.add_hyperedge(&[a, v, c], 1.0).unwrap();
        b.add_hyperedge(&[c, d], 2.5).unwrap();
        b.build().unwrap()
    }

    #[test]
    fn csr_matches_dense_incidence_matrix() {
        let hg = triangle_plus_edge();
        let dense = dense_incidence_matrix(&hg).unwrap();
        let csr = incidence_matrix_csr(&hg).unwrap();

        assert_eq!(csr.shape(), (dense.nrows(), dense.ncols()));
        for i in 0..dense.nrows() {
            for j in 0..dense.ncols() {
                let start = csr.row_ptr[i];
                let end = csr.row_ptr[i + 1];
                let sparse_val = (start..end)
                    .find(|&k| csr.col_indices[k] == j)
                    .map(|k| csr.values[k])
                    .unwrap_or(0.0);
                assert_relative_eq!(sparse_val, dense[(i, j)], epsilon = 1e-12);
            }
        }
    }

    #[test]
    fn csr_matvec_matches_dense() {
        let hg = triangle_plus_edge();
        let dense = dense_incidence_matrix(&hg).unwrap();
        let csr = incidence_matrix_csr(&hg).unwrap();

        let x = vec![1.0, 2.0]; // m = 2 hyperedges
        let y = csr.matvec(&x);
        let y_dense = &dense * nalgebra::DVector::from_vec(x);
        for i in 0..y.len() {
            assert_relative_eq!(y[i], y_dense[i], epsilon = 1e-12);
        }
    }

    #[test]
    fn transpose_round_trips() {
        let hg = triangle_plus_edge();
        let csr = incidence_matrix_csr(&hg).unwrap();
        let back = csr.transpose().transpose();
        assert_eq!(csr, back);
    }

    #[test]
    fn transpose_matches_dense_transpose() {
        let hg = triangle_plus_edge();
        let dense = dense_incidence_matrix(&hg).unwrap();
        let csr_t = incidence_matrix_csr(&hg).unwrap().transpose();
        assert_eq!(csr_t.shape(), (dense.ncols(), dense.nrows()));
        for i in 0..dense.ncols() {
            for j in 0..dense.nrows() {
                let start = csr_t.row_ptr[i];
                let end = csr_t.row_ptr[i + 1];
                let sparse_val = (start..end)
                    .find(|&k| csr_t.col_indices[k] == j)
                    .map(|k| csr_t.values[k])
                    .unwrap_or(0.0);
                assert_relative_eq!(sparse_val, dense[(j, i)], epsilon = 1e-12);
            }
        }
    }

    #[test]
    #[cfg(feature = "sprs-interop")]
    fn converts_to_sprs_csmat() {
        use sprs::TriMat;

        let hg = triangle_plus_edge();
        let csr = incidence_matrix_csr(&hg).unwrap();
        let expected_nnz = csr.nnz();
        let mat: sprs::CsMat<f64> = incidence_matrix_sprs(&hg).unwrap();

        assert_eq!(mat.shape(), (hg.num_vertices(), hg.num_hyperedges()));
        assert_eq!(mat.nnz(), expected_nnz);

        // Cross-check against a matrix built independently via sprs's own
        // triplet API, to make sure `From<CsrMatrix>` didn't silently
        // transpose or otherwise misinterpret the layout.
        let mut tri = TriMat::new((hg.num_vertices(), hg.num_hyperedges()));
        for e in hg.hyperedge_ids() {
            for v in hg.hyperedge_members(e).unwrap() {
                let w = hg.incidence_weight(v, e).unwrap();
                tri.add_triplet(v.0, e.0, w);
            }
        }
        let expected: sprs::CsMat<f64> = tri.to_csr();
        for i in 0..hg.num_vertices() {
            for j in 0..hg.num_hyperedges() {
                assert_relative_eq!(
                    mat.get(i, j).copied().unwrap_or(0.0),
                    expected.get(i, j).copied().unwrap_or(0.0),
                    epsilon = 1e-12
                );
            }
        }
    }
}
