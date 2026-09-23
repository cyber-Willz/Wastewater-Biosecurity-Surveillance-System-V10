//! Run with: `cargo run --example basic_usage`

use spectral_hypergraph::hypergraph::HypergraphBuilder;
use spectral_hypergraph::laplacian::{clique_expansion_adjacency, dense_normalized_laplacian};
use spectral_hypergraph::spectral::{fiedler_vector, spectral_cluster};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a small research-collaboration hypergraph: each hyperedge is a
    // paper, its members are the co-authors.
    let mut b = HypergraphBuilder::new();
    let alice = b.add_vertex("alice")?;
    let bob = b.add_vertex("bob")?;
    let carol = b.add_vertex("carol")?;
    let dave = b.add_vertex("dave")?;
    let erin = b.add_vertex("erin")?;
    let frank = b.add_vertex("frank")?;

    b.add_labeled_hyperedge(Some("paper1"), &[alice, bob, carol], 1.0)?;
    b.add_labeled_hyperedge(Some("paper2"), &[bob, carol], 1.0)?;
    b.add_labeled_hyperedge(Some("paper3"), &[dave, erin, frank], 1.0)?;
    b.add_labeled_hyperedge(Some("paper4"), &[erin, frank], 1.0)?;
    // A single weak cross-team collaboration.
    b.add_labeled_hyperedge(Some("paper5"), &[carol, dave], 0.1)?;

    let hg = b.build()?;
    println!(
        "hypergraph: {} authors, {} papers",
        hg.num_vertices(),
        hg.num_hyperedges()
    );

    for v in hg.vertex_ids() {
        println!(
            "  {:>6} degree={:.2}",
            hg.vertex_label(v)?,
            hg.vertex_degree(v)?
        );
    }

    let laplacian = dense_normalized_laplacian(&hg)?;
    println!("\nnormalized Laplacian is {}x{}", laplacian.nrows(), laplacian.ncols());

    let fiedler = fiedler_vector(&hg)?;
    println!("\nFiedler vector (algebraic connectivity signal):");
    for v in hg.vertex_ids() {
        println!("  {:>6}: {:+.4}", hg.vertex_label(v)?, fiedler[v.0]);
    }

    let clusters = spectral_cluster(&hg, 2, false, 42)?;
    println!("\nspectral clustering into 2 communities:");
    for v in hg.vertex_ids() {
        println!(
            "  {:>6} -> cluster {}",
            hg.vertex_label(v)?,
            clusters.assignments[v.0]
        );
    }

    let clique_adj = clique_expansion_adjacency(&hg)?;
    println!(
        "\nclique-expansion adjacency (alice, bob) = {:.3}",
        clique_adj[(alice.0, bob.0)]
    );

    Ok(())
}
