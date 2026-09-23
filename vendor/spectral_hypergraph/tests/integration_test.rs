use spectral_hypergraph::hypergraph::HypergraphBuilder;
use spectral_hypergraph::laplacian::{dense_normalized_laplacian, HypergraphOperator};
use spectral_hypergraph::operator::LinearOperator;
use spectral_hypergraph::spectral::{dense_eigen, lanczos_smallest, spectral_cluster};

/// Builds a hypergraph with `num_clusters` disjoint dense "communities" of
/// `cluster_size` vertices each (every community is one big hyperedge),
/// linked into a single connected component by weak bridging hyperedges
/// between consecutive communities.
fn clustered_hypergraph(
    num_clusters: usize,
    cluster_size: usize,
) -> spectral_hypergraph::SpectralHypergraph {
    let mut b = HypergraphBuilder::new();
    let mut clusters = Vec::with_capacity(num_clusters);
    for c in 0..num_clusters {
        let mut members = Vec::with_capacity(cluster_size);
        for i in 0..cluster_size {
            members.push(b.add_vertex(format!("c{c}_v{i}")).unwrap());
        }
        b.add_hyperedge(&members, 10.0).unwrap();
        clusters.push(members);
    }
    for c in 0..num_clusters.saturating_sub(1) {
        let bridge = [clusters[c][0], clusters[c + 1][0]];
        b.add_hyperedge(&bridge, 0.01).unwrap();
    }
    b.build().unwrap()
}

#[test]
fn matrix_free_and_dense_paths_agree_at_moderate_scale() {
    let hg = clustered_hypergraph(4, 15); // 60 vertices, sparse structure.
    assert_eq!(hg.num_vertices(), 60);

    let dense = dense_normalized_laplacian(&hg).unwrap();
    let dense_decomp = dense_eigen(&dense);

    let op = HypergraphOperator::new(&hg).unwrap();
    let lanczos_decomp = lanczos_smallest(&op, 6, 120, 1e-5, 123).unwrap();

    for i in 0..6 {
        assert!(
            (dense_decomp.eigenvalues[i] - lanczos_decomp.eigenvalues[i]).abs() < 1e-3,
            "eigenvalue {i} mismatch: dense={} lanczos={}",
            dense_decomp.eigenvalues[i],
            lanczos_decomp.eigenvalues[i]
        );
    }
}

#[test]
fn spectral_clustering_recovers_communities_at_scale() {
    let num_clusters = 5;
    let cluster_size = 12;
    let hg = clustered_hypergraph(num_clusters, cluster_size);

    let result = spectral_cluster(&hg, num_clusters, true, 99).unwrap();

    // Every vertex within a community must land in the same cluster label
    // (label identity across communities is arbitrary, so we only check
    // internal consistency and cross-community separation).
    for c in 0..num_clusters {
        let start = c * cluster_size;
        let label = result.assignments[start];
        for i in 0..cluster_size {
            assert_eq!(
                result.assignments[start + i],
                label,
                "vertex {} not grouped with its community",
                start + i
            );
        }
    }
    let distinct_labels: std::collections::HashSet<usize> =
        result.assignments.iter().copied().collect();
    assert_eq!(distinct_labels.len(), num_clusters);
}

#[test]
fn operator_dimension_matches_vertex_count() {
    let hg = clustered_hypergraph(3, 8);
    let op = HypergraphOperator::new(&hg).unwrap();
    assert_eq!(op.dim(), hg.num_vertices());
}

#[test]
fn end_to_end_builder_to_fiedler_vector() {
    use spectral_hypergraph::spectral::fiedler_vector;

    let mut b = HypergraphBuilder::new();
    let verts: Vec<_> = (0..10).map(|i| b.add_vertex(format!("v{i}")).unwrap()).collect();
    b.add_hyperedge(&verts[0..5], 1.0).unwrap();
    b.add_hyperedge(&verts[5..10], 1.0).unwrap();
    b.add_hyperedge(&[verts[4], verts[5]], 0.05).unwrap();
    let hg = b.build().unwrap();

    let fiedler = fiedler_vector(&hg).unwrap();
    assert_eq!(fiedler.len(), 10);
    let first_half_sign = fiedler[0].signum();
    for i in 1..5 {
        assert_eq!(fiedler[i].signum(), first_half_sign);
    }
    let second_half_sign = fiedler[5].signum();
    for i in 6..10 {
        assert_eq!(fiedler[i].signum(), second_half_sign);
    }
    assert_ne!(first_half_sign, second_half_sign);
}
