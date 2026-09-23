//! Demonstrates the four newer capabilities added on top of the core
//! (undirected) spectral hypergraph type:
//!
//! * `serde-support` -- JSON round-trip of a [`SpectralHypergraph`].
//! * `directed` module -- directed hyperedges (tail -> head).
//! * `parallel` -- rayon-backed matvecs in `HypergraphOperator::apply`.
//! * `sparse` -- CSR export of the incidence matrix.
//!
//! Run with: `cargo run --example new_features --all-features`
//! (parts of this example are `cfg`-gated on the corresponding feature and
//! silently skipped if it's off).

use spectral_hypergraph::hypergraph::HypergraphBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut b = HypergraphBuilder::new();
    let alice = b.add_vertex("alice")?;
    let bob = b.add_vertex("bob")?;
    let carol = b.add_vertex("carol")?;
    b.add_labeled_hyperedge(Some("paper1"), &[alice, bob, carol], 1.0)?;
    let hg = b.build()?;

    // -- serde-support --------------------------------------------------
    #[cfg(feature = "serde-support")]
    {
        let json = serde_json::to_string_pretty(&hg)?;
        println!("--- serde-support: JSON round-trip ---");
        println!("{json}");
        let restored: spectral_hypergraph::SpectralHypergraph = serde_json::from_str(&json)?;
        assert_eq!(restored.num_vertices(), hg.num_vertices());
        println!("round-trip OK ({} vertices)\n", restored.num_vertices());
    }

    // -- directed ---------------------------------------------------------
    {
        use spectral_hypergraph::directed::DirectedHypergraphBuilder;

        println!("--- directed: a citation hypergraph ---");
        let mut db = DirectedHypergraphBuilder::new();
        let paper_a = db.add_vertex("paper_a")?;
        let paper_b = db.add_vertex("paper_b")?;
        let paper_c = db.add_vertex("paper_c")?;
        let survey = db.add_vertex("survey")?;
        // {paper_a, paper_b} are both cited by `survey` -- a single
        // hyperarc with a two-vertex tail and a one-vertex head.
        db.add_directed_hyperedge(&[paper_a, paper_b], &[survey], 1.0)?;
        // `survey` in turn cites paper_c.
        db.add_directed_hyperedge(&[survey], &[paper_c], 1.0)?;
        let dhg = db.build()?;

        println!(
            "  survey: in-degree={:.2} out-degree={:.2}",
            dhg.in_degree(survey)?,
            dhg.out_degree(survey)?
        );
        let adj = dhg.clique_expansion_adjacency()?;
        println!(
            "  clique-expansion adjacency (paper_a -> survey) = {:.3}\n",
            adj[(paper_a.0, survey.0)]
        );
    }

    // -- sparse (CSR) -------------------------------------------------------
    {
        use spectral_hypergraph::sparse::incidence_matrix_csr;

        println!("--- sparse: CSR incidence export ---");
        let csr = incidence_matrix_csr(&hg)?;
        println!(
            "  shape={:?} nnz={} row_ptr={:?}",
            csr.shape(),
            csr.nnz(),
            csr.row_ptr
        );
        let y = csr.matvec(&vec![1.0; csr.shape().1]);
        println!("  H * ones = {y:?}\n");
    }

    // -- parallel -------------------------------------------------------
    #[cfg(feature = "parallel")]
    {
        use spectral_hypergraph::laplacian::HypergraphOperator;
        use spectral_hypergraph::LinearOperator;

        println!("--- parallel: HypergraphOperator::apply (rayon-backed above a size threshold) ---");
        let op = HypergraphOperator::new(&hg)?;
        let x = nalgebra::DVector::from_element(op.dim(), 1.0);
        let y = op.apply(&x);
        println!("  Laplacian * ones = {y}");
    }

    Ok(())
}
