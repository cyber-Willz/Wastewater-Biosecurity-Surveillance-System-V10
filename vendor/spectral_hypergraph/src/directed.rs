//! A weighted, **directed** hyperedge variant.
//!
//! The rest of this crate ([`crate::hypergraph::SpectralHypergraph`]) models
//! *undirected* hyperedges: a hyperedge is just a set of member vertices,
//! matching the Zhou/Huang/Schölkopf construction the [`crate::laplacian`]
//! and [`crate::spectral`] modules are built around.
//!
//! This module instead models a **directed hyperedge** (also called a
//! B-arc/hyperarc in the hypergraph literature) as a pair of disjoint
//! vertex sets:
//!
//! * a **tail** `T(e)` (the "sources") and
//! * a **head** `H(e)` (the "targets"),
//!
//! with a single weight `w(e)` interpreted as flowing from every vertex in
//! `T(e)` to every vertex in `H(e)`. This generalizes an ordinary directed
//! edge (`|T(e)| = |H(e)| = 1`) the same way an undirected hyperedge
//! generalizes an ordinary undirected edge.
//!
//! Like [`crate::hypergraph::SpectralHypergraph`], [`DirectedHypergraph`] is
//! represented internally as a bipartite graph (here [`petgraph::graph::DiGraph`]):
//! an edge `vertex -> hyperedge` records tail membership, an edge
//! `hyperedge -> vertex` records head membership. [`VertexId`] is shared
//! with the undirected type so the two can reference the same vertex space
//! if a caller wants both an undirected and directed view of related data;
//! [`DirectedHyperEdgeId`] is a distinct type since directed and undirected
//! hyperedges are never interchangeable.
//!
//! This module intentionally does not provide a directed analogue of
//! [`crate::laplacian`]/[`crate::spectral`] -- there is no single canonical
//! directed hypergraph Laplacian the way there is in the undirected case
//! (see e.g. Chung's directed graph Laplacian and its various proposed
//! hypergraph generalizations, which disagree on normalization choices).
//! [`DirectedHypergraph::clique_expansion_adjacency`] instead reduces to an
//! ordinary weighted *directed graph* adjacency matrix, which downstream
//! code can feed into whichever directed-graph spectral method fits its
//! use case.

use std::collections::HashMap;

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::error::{HypergraphError, Result};
pub use crate::hypergraph::VertexId;

/// Opaque handle to a directed hyperedge. Distinct from
/// [`crate::hypergraph::HyperEdgeId`] -- directed and undirected hyperedges
/// are never interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde-support", derive(serde::Serialize, serde::Deserialize))]
pub struct DirectedHyperEdgeId(pub usize);

/// The two kinds of node stored in the underlying bipartite [`DiGraph`]
/// (mirrors [`crate::hypergraph::NodeKind`] for the directed bipartite
/// representation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A hypergraph vertex, carrying its [`VertexId`] index.
    Vertex(usize),
    /// A directed hyperedge, carrying its [`DirectedHyperEdgeId`] index.
    HyperEdge(usize),
}

#[derive(Debug, Clone)]
struct VertexRecord {
    label: String,
    weight: f64,
    node_index: NodeIndex,
}

#[derive(Debug, Clone)]
struct DirectedHyperEdgeRecord {
    label: Option<String>,
    weight: f64,
    node_index: NodeIndex,
}

/// A weighted, directed hypergraph: vertices connected by directed
/// hyperedges, each with a tail (source) vertex set and a head (target)
/// vertex set. Construct via [`DirectedHypergraphBuilder`].
#[derive(Debug, Clone)]
pub struct DirectedHypergraph {
    graph: DiGraph<NodeKind, f64>,
    vertices: Vec<VertexRecord>,
    hyperedges: Vec<DirectedHyperEdgeRecord>,
    label_to_vertex: HashMap<String, usize>,
}

impl DirectedHypergraph {
    /// Number of vertices `|V|`.
    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// Number of directed hyperedges `|E|`.
    pub fn num_hyperedges(&self) -> usize {
        self.hyperedges.len()
    }

    /// Read-only access to the underlying bipartite [`DiGraph`]. An edge
    /// `vertex -> hyperedge` means the vertex is in that hyperedge's tail;
    /// an edge `hyperedge -> vertex` means the vertex is in its head.
    pub fn bipartite_graph(&self) -> &DiGraph<NodeKind, f64> {
        &self.graph
    }

    /// Look up a vertex by its label.
    pub fn vertex_by_label(&self, label: &str) -> Option<VertexId> {
        self.label_to_vertex.get(label).map(|&i| VertexId(i))
    }

    /// The label a vertex was created with.
    pub fn vertex_label(&self, v: VertexId) -> Result<&str> {
        self.vertices
            .get(v.0)
            .map(|r| r.label.as_str())
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })
    }

    /// The intrinsic weight assigned to a vertex at construction time
    /// (default `1.0`).
    pub fn vertex_weight(&self, v: VertexId) -> Result<f64> {
        self.vertices
            .get(v.0)
            .map(|r| r.weight)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })
    }

    /// The weight assigned to a directed hyperedge at construction time
    /// (default `1.0`).
    pub fn hyperedge_weight(&self, e: DirectedHyperEdgeId) -> Result<f64> {
        self.hyperedges
            .get(e.0)
            .map(|r| r.weight)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })
    }

    /// Optional human-readable label attached at construction.
    pub fn hyperedge_label(&self, e: DirectedHyperEdgeId) -> Result<Option<&str>> {
        self.hyperedges
            .get(e.0)
            .map(|r| r.label.as_deref())
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })
    }

    /// The tail (source) vertex set `T(e)`, in ascending [`VertexId`] order.
    pub fn tail_members(&self, e: DirectedHyperEdgeId) -> Result<Vec<VertexId>> {
        let record = self.hyperedge_record(e)?;
        let mut members: Vec<VertexId> = self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Incoming)
            .filter_map(|edge_ref| match self.graph[edge_ref.source()] {
                NodeKind::Vertex(v_idx) => Some(VertexId(v_idx)),
                NodeKind::HyperEdge(_) => None,
            })
            .collect();
        members.sort_unstable();
        Ok(members)
    }

    /// The head (target) vertex set `H(e)`, in ascending [`VertexId`] order.
    pub fn head_members(&self, e: DirectedHyperEdgeId) -> Result<Vec<VertexId>> {
        let record = self.hyperedge_record(e)?;
        let mut members: Vec<VertexId> = self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Outgoing)
            .filter_map(|edge_ref| match self.graph[edge_ref.target()] {
                NodeKind::Vertex(v_idx) => Some(VertexId(v_idx)),
                NodeKind::HyperEdge(_) => None,
            })
            .collect();
        members.sort_unstable();
        Ok(members)
    }

    /// `|T(e)|`.
    pub fn tail_cardinality(&self, e: DirectedHyperEdgeId) -> Result<usize> {
        Ok(self.tail_members(e)?.len())
    }

    /// `|H(e)|`.
    pub fn head_cardinality(&self, e: DirectedHyperEdgeId) -> Result<usize> {
        Ok(self.head_members(e)?.len())
    }

    /// Weighted out-degree: `sum_{e : v in T(e)} w(e)`.
    pub fn out_degree(&self, v: VertexId) -> Result<f64> {
        let record = self.vertex_record(v)?;
        let mut degree = 0.0;
        for edge_ref in self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Outgoing)
        {
            if let NodeKind::HyperEdge(e_idx) = self.graph[edge_ref.target()] {
                degree += self.hyperedges[e_idx].weight;
            }
        }
        Ok(degree)
    }

    /// Weighted in-degree: `sum_{e : v in H(e)} w(e)`.
    pub fn in_degree(&self, v: VertexId) -> Result<f64> {
        let record = self.vertex_record(v)?;
        let mut degree = 0.0;
        for edge_ref in self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Incoming)
        {
            if let NodeKind::HyperEdge(e_idx) = self.graph[edge_ref.source()] {
                degree += self.hyperedges[e_idx].weight;
            }
        }
        Ok(degree)
    }

    /// All hyperedges that have `v` in their tail.
    pub fn hyperedges_with_v_in_tail(&self, v: VertexId) -> Result<Vec<DirectedHyperEdgeId>> {
        let record = self.vertex_record(v)?;
        let mut out: Vec<DirectedHyperEdgeId> = self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Outgoing)
            .filter_map(|edge_ref| match self.graph[edge_ref.target()] {
                NodeKind::HyperEdge(e_idx) => Some(DirectedHyperEdgeId(e_idx)),
                NodeKind::Vertex(_) => None,
            })
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// All hyperedges that have `v` in their head.
    pub fn hyperedges_with_v_in_head(&self, v: VertexId) -> Result<Vec<DirectedHyperEdgeId>> {
        let record = self.vertex_record(v)?;
        let mut out: Vec<DirectedHyperEdgeId> = self
            .graph
            .edges_directed(record.node_index, petgraph::Direction::Incoming)
            .filter_map(|edge_ref| match self.graph[edge_ref.source()] {
                NodeKind::HyperEdge(e_idx) => Some(DirectedHyperEdgeId(e_idx)),
                NodeKind::Vertex(_) => None,
            })
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Iterate over all vertex ids in construction order.
    pub fn vertex_ids(&self) -> impl Iterator<Item = VertexId> + '_ {
        (0..self.vertices.len()).map(VertexId)
    }

    /// Iterate over all directed hyperedge ids in construction order.
    pub fn hyperedge_ids(&self) -> impl Iterator<Item = DirectedHyperEdgeId> + '_ {
        (0..self.hyperedges.len()).map(DirectedHyperEdgeId)
    }

    /// Dense `n x n` clique-expansion adjacency of the directed hypergraph:
    /// each hyperedge `e` contributes `w(e) / (|T(e)| * |H(e)|)` to
    /// `adj[t, h]` for every `t` in `T(e)`, `h` in `H(e)`. This is the
    /// directed analogue of [`crate::laplacian::clique_expansion_adjacency`]
    /// -- it collapses the directed hypergraph into an ordinary weighted
    /// directed graph, contributions from `T(e) x H(e)` accumulating
    /// additively across hyperedges (so `adj` is generally asymmetric).
    pub fn clique_expansion_adjacency(&self) -> Result<nalgebra::DMatrix<f64>> {
        let n = self.num_vertices();
        let mut adj = nalgebra::DMatrix::<f64>::zeros(n, n);
        for e in self.hyperedge_ids() {
            let tail = self.tail_members(e)?;
            let head = self.head_members(e)?;
            if tail.is_empty() || head.is_empty() {
                continue;
            }
            let share = self.hyperedge_weight(e)? / (tail.len() as f64 * head.len() as f64);
            for &t in &tail {
                for &h in &head {
                    adj[(t.0, h.0)] += share;
                }
            }
        }
        Ok(adj)
    }

    fn vertex_record(&self, v: VertexId) -> Result<&VertexRecord> {
        self.vertices.get(v.0).ok_or(HypergraphError::IndexOutOfBounds {
            index: v.0,
            len: self.vertices.len(),
        })
    }

    fn hyperedge_record(&self, e: DirectedHyperEdgeId) -> Result<&DirectedHyperEdgeRecord> {
        self.hyperedges
            .get(e.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })
    }
}

/// Builder for [`DirectedHypergraph`], validating inputs as they are added.
///
/// ```
/// use spectral_hypergraph::directed::DirectedHypergraphBuilder;
///
/// let mut builder = DirectedHypergraphBuilder::new();
/// let a = builder.add_vertex("a").unwrap();
/// let b = builder.add_vertex("b").unwrap();
/// let c = builder.add_vertex("c").unwrap();
/// // hyperarc {a, b} -> {c}
/// builder.add_directed_hyperedge(&[a, b], &[c], 1.0).unwrap();
/// let hg = builder.build().unwrap();
/// assert_eq!(hg.num_vertices(), 3);
/// assert_eq!(hg.num_hyperedges(), 1);
/// ```
#[derive(Debug, Default)]
pub struct DirectedHypergraphBuilder {
    graph: DiGraph<NodeKind, f64>,
    vertices: Vec<VertexRecord>,
    hyperedges: Vec<DirectedHyperEdgeRecord>,
    label_to_vertex: HashMap<String, usize>,
}

impl DirectedHypergraphBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            vertices: Vec::new(),
            hyperedges: Vec::new(),
            label_to_vertex: HashMap::new(),
        }
    }

    /// Pre-allocate storage for `n` vertices and `m` hyperedges (with an
    /// average combined tail+head cardinality hint `avg_card`).
    pub fn with_capacity(n: usize, m: usize, avg_card: usize) -> Self {
        Self {
            graph: DiGraph::with_capacity(n + m, m * avg_card.max(1)),
            vertices: Vec::with_capacity(n),
            hyperedges: Vec::with_capacity(m),
            label_to_vertex: HashMap::with_capacity(n),
        }
    }

    /// Add a vertex with default weight `1.0`. Errors if `label` was already
    /// used.
    pub fn add_vertex(&mut self, label: impl Into<String>) -> Result<VertexId> {
        self.add_weighted_vertex(label, 1.0)
    }

    /// Add a vertex with an explicit non-negative, finite weight.
    pub fn add_weighted_vertex(&mut self, label: impl Into<String>, weight: f64) -> Result<VertexId> {
        validate_weight(weight)?;
        let label = label.into();
        if self.label_to_vertex.contains_key(&label) {
            return Err(HypergraphError::DuplicateVertex(label));
        }
        let id = self.vertices.len();
        let node_index = self.graph.add_node(NodeKind::Vertex(id));
        self.vertices.push(VertexRecord {
            label: label.clone(),
            weight,
            node_index,
        });
        self.label_to_vertex.insert(label, id);
        Ok(VertexId(id))
    }

    /// Get-or-create a vertex by label.
    pub fn get_or_add_vertex(&mut self, label: impl Into<String>) -> Result<VertexId> {
        let label = label.into();
        if let Some(&id) = self.label_to_vertex.get(&label) {
            return Ok(VertexId(id));
        }
        self.add_vertex(label)
    }

    /// Add a directed hyperedge (hyperarc) `tail -> head` with weight
    /// `weight`. Both `tail` and `head` must be non-empty (after
    /// deduplication); `tail` and `head` are permitted to overlap (a vertex
    /// may act as both a source and a target of the same hyperarc), which
    /// callers can reject upstream if their model forbids it.
    pub fn add_directed_hyperedge(
        &mut self,
        tail: &[VertexId],
        head: &[VertexId],
        weight: f64,
    ) -> Result<DirectedHyperEdgeId> {
        self.add_labeled_directed_hyperedge(None::<String>, tail, head, weight)
    }

    /// Like [`Self::add_directed_hyperedge`] but attaches a human-readable
    /// label.
    pub fn add_labeled_directed_hyperedge(
        &mut self,
        label: Option<impl Into<String>>,
        tail: &[VertexId],
        head: &[VertexId],
        weight: f64,
    ) -> Result<DirectedHyperEdgeId> {
        validate_weight(weight)?;

        let mut tail_unique: Vec<VertexId> = tail.to_vec();
        tail_unique.sort_unstable();
        tail_unique.dedup();
        let mut head_unique: Vec<VertexId> = head.to_vec();
        head_unique.sort_unstable();
        head_unique.dedup();

        if tail_unique.is_empty() || head_unique.is_empty() {
            return Err(HypergraphError::DegenerateDirectedHyperEdge {
                tail_len: tail_unique.len(),
                head_len: head_unique.len(),
            });
        }
        for &v in tail_unique.iter().chain(head_unique.iter()) {
            if v.0 >= self.vertices.len() {
                return Err(HypergraphError::IndexOutOfBounds {
                    index: v.0,
                    len: self.vertices.len(),
                });
            }
        }

        let e_id = self.hyperedges.len();
        let e_node = self.graph.add_node(NodeKind::HyperEdge(e_id));
        for v in &tail_unique {
            let v_node = self.vertices[v.0].node_index;
            self.graph.add_edge(v_node, e_node, 1.0);
        }
        for v in &head_unique {
            let v_node = self.vertices[v.0].node_index;
            self.graph.add_edge(e_node, v_node, 1.0);
        }
        self.hyperedges.push(DirectedHyperEdgeRecord {
            label: label.map(Into::into),
            weight,
            node_index: e_node,
        });
        Ok(DirectedHyperEdgeId(e_id))
    }

    /// Number of vertices added so far.
    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// Consume the builder, validating global invariants, and produce the
    /// immutable [`DirectedHypergraph`].
    pub fn build(self) -> Result<DirectedHypergraph> {
        if self.vertices.is_empty() {
            return Err(HypergraphError::EmptyVertexSet);
        }
        Ok(DirectedHypergraph {
            graph: self.graph,
            vertices: self.vertices,
            hyperedges: self.hyperedges,
            label_to_vertex: self.label_to_vertex,
        })
    }
}

fn validate_weight(weight: f64) -> Result<()> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(HypergraphError::InvalidWeight(weight));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn source_fanout_hypergraph() -> DirectedHypergraph {
        // hyperarc {a} -> {b, c}, weight 2.0
        // hyperarc {b, c} -> {d}, weight 1.0
        let mut b = DirectedHypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let c = b.add_vertex("c").unwrap();
        let d = b.add_vertex("d").unwrap();
        b.add_directed_hyperedge(&[a], &[v, c], 2.0).unwrap();
        b.add_directed_hyperedge(&[v, c], &[d], 1.0).unwrap();
        b.build().unwrap()
    }

    #[test]
    fn tail_and_head_members_are_correct() {
        let hg = source_fanout_hypergraph();
        let e0 = DirectedHyperEdgeId(0);
        assert_eq!(hg.tail_members(e0).unwrap(), vec![VertexId(0)]);
        assert_eq!(
            hg.head_members(e0).unwrap(),
            vec![VertexId(1), VertexId(2)]
        );
    }

    #[test]
    fn degrees_match_hand_computation() {
        let hg = source_fanout_hypergraph();
        let a = hg.vertex_by_label("a").unwrap();
        let v = hg.vertex_by_label("b").unwrap();
        let d = hg.vertex_by_label("d").unwrap();

        assert_relative_eq!(hg.out_degree(a).unwrap(), 2.0);
        assert_relative_eq!(hg.in_degree(a).unwrap(), 0.0);

        // b is in the head of e0 (weight 2.0) and the tail of e1 (weight 1.0)
        assert_relative_eq!(hg.in_degree(v).unwrap(), 2.0);
        assert_relative_eq!(hg.out_degree(v).unwrap(), 1.0);

        assert_relative_eq!(hg.in_degree(d).unwrap(), 1.0);
        assert_relative_eq!(hg.out_degree(d).unwrap(), 0.0);
    }

    #[test]
    fn rejects_empty_tail_or_head() {
        let mut b = DirectedHypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let err = b.add_directed_hyperedge(&[], &[a], 1.0).unwrap_err();
        assert!(matches!(
            err,
            HypergraphError::DegenerateDirectedHyperEdge { tail_len: 0, .. }
        ));
        let err = b.add_directed_hyperedge(&[a], &[], 1.0).unwrap_err();
        assert!(matches!(
            err,
            HypergraphError::DegenerateDirectedHyperEdge { head_len: 0, .. }
        ));
    }

    #[test]
    fn rejects_unknown_vertex() {
        let mut b = DirectedHypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let ghost = VertexId(99);
        let err = b
            .add_directed_hyperedge(&[a], &[ghost], 1.0)
            .unwrap_err();
        assert!(matches!(err, HypergraphError::IndexOutOfBounds { .. }));
    }

    #[test]
    fn clique_expansion_matches_hand_computation() {
        let hg = source_fanout_hypergraph();
        let adj = hg.clique_expansion_adjacency().unwrap();
        // e0: {a} -> {b, c}, weight 2.0, share = 2.0 / (1 * 2) = 1.0
        assert_relative_eq!(adj[(0, 1)], 1.0, epsilon = 1e-12);
        assert_relative_eq!(adj[(0, 2)], 1.0, epsilon = 1e-12);
        // e1: {b, c} -> {d}, weight 1.0, share = 1.0 / (2 * 1) = 0.5
        assert_relative_eq!(adj[(1, 3)], 0.5, epsilon = 1e-12);
        assert_relative_eq!(adj[(2, 3)], 0.5, epsilon = 1e-12);
        // no direct a -> d contribution
        assert_relative_eq!(adj[(0, 3)], 0.0, epsilon = 1e-12);
        // asymmetric: b -> a has no contribution even though a -> b does not
        // exist either here, but check the matrix isn't accidentally
        // symmetrized
        assert_relative_eq!(adj[(1, 0)], 0.0, epsilon = 1e-12);
    }

    #[test]
    fn overlapping_tail_and_head_is_allowed() {
        let mut b = DirectedHypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        // a appears in both tail and head -- allowed, not rejected.
        let e = b.add_directed_hyperedge(&[a, v], &[a], 1.0).unwrap();
        let hg = b.build().unwrap();
        assert_eq!(hg.tail_members(e).unwrap(), vec![a, v]);
        assert_eq!(hg.head_members(e).unwrap(), vec![a]);
    }
}
