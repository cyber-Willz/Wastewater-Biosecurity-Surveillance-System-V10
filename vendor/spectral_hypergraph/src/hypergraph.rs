//! Core hypergraph data structure.
//!
//! A hypergraph `H = (V, E)` generalizes a graph by allowing each "edge" to
//! connect an arbitrary number of vertices (>= 2). Internally this crate
//! represents the hypergraph as a **bipartite graph** over two node kinds —
//! [`NodeKind::Vertex`] and [`NodeKind::HyperEdge`] — using [`petgraph`]'s
//! `UnGraph`. An undirected edge `(v, e)` in the bipartite graph, weighted by
//! `w`, means "vertex `v` participates in hyperedge `e` with incidence
//! weight `w`". This is the standard construction used to reduce hypergraph
//! algorithms to graph algorithms (Zhou, Huang & Schölkopf, 2006), and it is
//! what lets this crate reuse a battle-tested graph data structure rather
//! than hand-rolling incidence storage.
//!
//! The type is intentionally immutable-by-value for the hot path: build it
//! with [`HypergraphBuilder`], then query/analyze the resulting
//! [`SpectralHypergraph`]. `SpectralHypergraph` is `Clone + Send + Sync`, so
//! it can be shared across threads (e.g. wrapped in an `Arc`) without any
//! internal locking.

use std::collections::HashMap;

use petgraph::graph::{NodeIndex, UnGraph};
use petgraph::visit::EdgeRef;

use crate::error::{HypergraphError, Result};

/// Opaque handle to a vertex. Stable for the lifetime of the hypergraph that
/// produced it (indices are never reused after removal because this crate
/// does not support removal — see the module docs on immutability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde-support", derive(serde::Serialize, serde::Deserialize))]
pub struct VertexId(pub usize);

/// Opaque handle to a hyperedge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde-support", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperEdgeId(pub usize);

/// The two kinds of node stored in the underlying bipartite [`petgraph`]
/// graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A hypergraph vertex, carrying its [`VertexId`].
    Vertex(usize),
    /// A hyperedge, carrying its [`HyperEdgeId`].
    HyperEdge(usize),
}

#[derive(Debug, Clone)]
struct VertexRecord {
    label: String,
    weight: f64,
    node_index: NodeIndex,
}

#[derive(Debug, Clone)]
struct HyperEdgeRecord {
    label: Option<String>,
    weight: f64,
    node_index: NodeIndex,
}

/// A production-oriented spectral hypergraph.
///
/// Construct via [`HypergraphBuilder`]. Query degrees, incident sets, and
/// the underlying bipartite graph here; hand this struct to
/// [`crate::laplacian`] and [`crate::spectral`] for spectral analysis.
#[derive(Debug, Clone)]
pub struct SpectralHypergraph {
    graph: UnGraph<NodeKind, f64>,
    vertices: Vec<VertexRecord>,
    hyperedges: Vec<HyperEdgeRecord>,
    label_to_vertex: HashMap<String, usize>,
}

impl SpectralHypergraph {
    /// Number of vertices `|V|`.
    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// Number of hyperedges `|E|`.
    pub fn num_hyperedges(&self) -> usize {
        self.hyperedges.len()
    }

    /// Read-only access to the underlying bipartite [`petgraph`] graph
    /// (vertices and hyperedges as nodes; an edge `(v, e)` means `v` is
    /// incident to hyperedge `e`, weighted by the incidence weight).
    pub fn bipartite_graph(&self) -> &UnGraph<NodeKind, f64> {
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
    /// (default `1.0`). This is distinct from the *degree* of the vertex.
    pub fn vertex_weight(&self, v: VertexId) -> Result<f64> {
        self.vertices
            .get(v.0)
            .map(|r| r.weight)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })
    }

    /// The weight assigned to a hyperedge at construction time (default
    /// `1.0`). Used in the normalized hypergraph Laplacian.
    pub fn hyperedge_weight(&self, e: HyperEdgeId) -> Result<f64> {
        self.hyperedges
            .get(e.0)
            .map(|r| r.weight)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })
    }

    /// Weighted vertex degree: `d(v) = sum_{e in E, v in e} w(e)`.
    pub fn vertex_degree(&self, v: VertexId) -> Result<f64> {
        let record = self
            .vertices
            .get(v.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })?;
        let mut degree = 0.0;
        for edge_ref in self.graph.edges(record.node_index) {
            if let NodeKind::HyperEdge(e_idx) = self.graph[edge_ref.target()] {
                degree += self.hyperedges[e_idx].weight;
            }
        }
        Ok(degree)
    }

    /// Cardinality (number of member vertices) of a hyperedge, i.e. `|e|`.
    pub fn hyperedge_degree(&self, e: HyperEdgeId) -> Result<usize> {
        let record = self
            .hyperedges
            .get(e.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })?;
        Ok(self.graph.edges(record.node_index).count())
    }

    /// All vertices incident to a hyperedge, in insertion order.
    pub fn hyperedge_members(&self, e: HyperEdgeId) -> Result<Vec<VertexId>> {
        let record = self
            .hyperedges
            .get(e.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })?;
        let mut members: Vec<VertexId> = self
            .graph
            .edges(record.node_index)
            .filter_map(|edge_ref| match self.graph[edge_ref.target()] {
                NodeKind::Vertex(v_idx) => Some(VertexId(v_idx)),
                NodeKind::HyperEdge(_) => None,
            })
            .collect();
        members.sort_unstable();
        Ok(members)
    }

    /// All hyperedges incident to a vertex, in insertion order.
    pub fn incident_hyperedges(&self, v: VertexId) -> Result<Vec<HyperEdgeId>> {
        let record = self
            .vertices
            .get(v.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })?;
        let mut incident: Vec<HyperEdgeId> = self
            .graph
            .edges(record.node_index)
            .filter_map(|edge_ref| match self.graph[edge_ref.target()] {
                NodeKind::HyperEdge(e_idx) => Some(HyperEdgeId(e_idx)),
                NodeKind::Vertex(_) => None,
            })
            .collect();
        incident.sort_unstable();
        Ok(incident)
    }

    /// The incidence weight of `(v, e)` — `0.0` if `v` is not a member of `e`.
    pub fn incidence_weight(&self, v: VertexId, e: HyperEdgeId) -> Result<f64> {
        let v_record = self
            .vertices
            .get(v.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: v.0,
                len: self.vertices.len(),
            })?;
        let e_record = self
            .hyperedges
            .get(e.0)
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })?;
        Ok(self
            .graph
            .edges(v_record.node_index)
            .find(|edge_ref| edge_ref.target() == e_record.node_index)
            .map(|edge_ref| *edge_ref.weight())
            .unwrap_or(0.0))
    }

    /// Iterate over all vertex ids in construction order.
    pub fn vertex_ids(&self) -> impl Iterator<Item = VertexId> + '_ {
        (0..self.vertices.len()).map(VertexId)
    }

    /// Iterate over all hyperedge ids in construction order.
    pub fn hyperedge_ids(&self) -> impl Iterator<Item = HyperEdgeId> + '_ {
        (0..self.hyperedges.len()).map(HyperEdgeId)
    }

    /// `true` if every vertex has strictly positive degree. Most spectral
    /// routines (which need `D_v^{-1/2}`) require this; call
    /// [`SpectralHypergraph::isolated_vertices`] to find offenders.
    pub fn is_degree_normalizable(&self) -> bool {
        self.vertex_ids()
            .all(|v| self.vertex_degree(v).unwrap_or(0.0) > 0.0)
    }

    /// Vertices with zero weighted degree (not a member of any hyperedge).
    pub fn isolated_vertices(&self) -> Vec<VertexId> {
        self.vertex_ids()
            .filter(|&v| self.vertex_degree(v).unwrap_or(0.0) == 0.0)
            .collect()
    }

    /// The [`NodeIndex`] of `v` within [`Self::bipartite_graph`], for callers
    /// that want to run their own `petgraph` algorithms directly against the
    /// bipartite representation.
    pub fn vertex_node_index(&self, v: VertexId) -> Option<NodeIndex> {
        self.vertices.get(v.0).map(|r| r.node_index)
    }

    /// The [`NodeIndex`] of `e` within [`Self::bipartite_graph`].
    pub fn hyperedge_node_index(&self, e: HyperEdgeId) -> Option<NodeIndex> {
        self.hyperedges.get(e.0).map(|r| r.node_index)
    }
}

/// Builder for [`SpectralHypergraph`], validating inputs as they are added.
///
/// ```
/// use spectral_hypergraph::hypergraph::HypergraphBuilder;
///
/// let mut builder = HypergraphBuilder::new();
/// let a = builder.add_vertex("a").unwrap();
/// let b = builder.add_vertex("b").unwrap();
/// let c = builder.add_vertex("c").unwrap();
/// builder.add_hyperedge(&[a, b, c], 1.0).unwrap();
/// let hg = builder.build().unwrap();
/// assert_eq!(hg.num_vertices(), 3);
/// assert_eq!(hg.num_hyperedges(), 1);
/// ```
#[derive(Debug, Default)]
pub struct HypergraphBuilder {
    graph: UnGraph<NodeKind, f64>,
    vertices: Vec<VertexRecord>,
    hyperedges: Vec<HyperEdgeRecord>,
    label_to_vertex: HashMap<String, usize>,
}

impl HypergraphBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self {
            graph: UnGraph::new_undirected(),
            vertices: Vec::new(),
            hyperedges: Vec::new(),
            label_to_vertex: HashMap::new(),
        }
    }

    /// Pre-allocate storage for `n` vertices and `m` hyperedges (with an
    /// average cardinality hint `avg_card` for the bipartite edge count).
    pub fn with_capacity(n: usize, m: usize, avg_card: usize) -> Self {
        Self {
            graph: UnGraph::with_capacity(n + m, m * avg_card.max(1)),
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

    /// Get-or-create a vertex by label, useful when streaming hyperedges
    /// whose member vertices are not known to exist yet.
    pub fn get_or_add_vertex(&mut self, label: impl Into<String>) -> Result<VertexId> {
        let label = label.into();
        if let Some(&id) = self.label_to_vertex.get(&label) {
            return Ok(VertexId(id));
        }
        self.add_vertex(label)
    }

    /// Add a hyperedge over `members` (each vertex may appear at most once)
    /// with uniform incidence weight `weight`. Requires at least 2 distinct
    /// members. Returns the new hyperedge's id.
    pub fn add_hyperedge(&mut self, members: &[VertexId], weight: f64) -> Result<HyperEdgeId> {
        self.add_labeled_hyperedge(None::<String>, members, weight)
    }

    /// Like [`Self::add_hyperedge`] but attaches a human-readable label.
    pub fn add_labeled_hyperedge(
        &mut self,
        label: Option<impl Into<String>>,
        members: &[VertexId],
        weight: f64,
    ) -> Result<HyperEdgeId> {
        validate_weight(weight)?;
        let mut unique: Vec<VertexId> = members.to_vec();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() < 2 {
            return Err(HypergraphError::DegenerateHyperEdge(unique.len()));
        }
        for &v in &unique {
            if v.0 >= self.vertices.len() {
                return Err(HypergraphError::IndexOutOfBounds {
                    index: v.0,
                    len: self.vertices.len(),
                });
            }
        }

        let e_id = self.hyperedges.len();
        let e_node = self.graph.add_node(NodeKind::HyperEdge(e_id));
        for v in &unique {
            let v_node = self.vertices[v.0].node_index;
            self.graph.add_edge(v_node, e_node, 1.0);
        }
        self.hyperedges.push(HyperEdgeRecord {
            label: label.map(Into::into),
            weight,
            node_index: e_node,
        });
        Ok(HyperEdgeId(e_id))
    }

    /// Number of vertices added so far.
    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// Consume the builder, validating global invariants, and produce the
    /// immutable [`SpectralHypergraph`].
    pub fn build(self) -> Result<SpectralHypergraph> {
        if self.vertices.is_empty() {
            return Err(HypergraphError::EmptyVertexSet);
        }
        Ok(SpectralHypergraph {
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

// Re-export the hyperedge label accessor without requiring the whole record
// to be public.
impl SpectralHypergraph {
    /// Optional human-readable label attached at construction.
    pub fn hyperedge_label(&self, e: HyperEdgeId) -> Result<Option<&str>> {
        self.hyperedges
            .get(e.0)
            .map(|r| r.label.as_deref())
            .ok_or(HypergraphError::IndexOutOfBounds {
                index: e.0,
                len: self.hyperedges.len(),
            })
    }
}

/// Serde support, gated behind the `serde-support` feature.
///
/// [`SpectralHypergraph`] is *not* serialized via a derive on its internal
/// `petgraph` representation (node indices, bipartite structure) — that
/// would leak an implementation detail into the wire format and couple it
/// to `petgraph`'s own (independently versioned) `serde-1` schema. Instead
/// this implements [`serde::Serialize`] / [`serde::Deserialize`] by hand
/// against a small, stable schema:
///
/// ```json
/// {
///   "vertices": [{"label": "a", "weight": 1.0}, ...],
///   "hyperedges": [{"label": null, "weight": 1.0, "members": [0, 2, 3]}, ...]
/// }
/// ```
///
/// `members` are vertex *indices* (i.e. [`VertexId::0`]) into the
/// `vertices` array, in the order it appears in the document. Deserializing
/// replays the hypergraph through [`HypergraphBuilder`], so every
/// construction-time invariant (duplicate labels, degenerate hyperedges,
/// out-of-range members, invalid weights) is re-validated rather than
/// trusted from the wire — a malformed or hand-edited document produces a
/// [`crate::error::HypergraphError`] via `serde::de::Error::custom`, not a
/// silently inconsistent [`SpectralHypergraph`].
#[cfg(feature = "serde-support")]
mod serde_support {
    use serde::de::Error as _;
    use serde::ser::SerializeStruct;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{HypergraphBuilder, SpectralHypergraph};

    #[derive(Serialize, Deserialize)]
    struct WireVertex {
        label: String,
        weight: f64,
    }

    #[derive(Serialize, Deserialize)]
    struct WireHyperEdge {
        label: Option<String>,
        weight: f64,
        members: Vec<usize>,
    }

    #[derive(Serialize, Deserialize)]
    struct WireHypergraph {
        vertices: Vec<WireVertex>,
        hyperedges: Vec<WireHyperEdge>,
    }

    impl Serialize for SpectralHypergraph {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let vertices: Vec<WireVertex> = self
                .vertex_ids()
                .map(|v| WireVertex {
                    label: self.vertex_label(v).expect("valid vertex id").to_string(),
                    weight: self.vertex_weight(v).expect("valid vertex id"),
                })
                .collect();

            let hyperedges: Vec<WireHyperEdge> = self
                .hyperedge_ids()
                .map(|e| WireHyperEdge {
                    label: self
                        .hyperedge_label(e)
                        .expect("valid hyperedge id")
                        .map(str::to_string),
                    weight: self.hyperedge_weight(e).expect("valid hyperedge id"),
                    members: self
                        .hyperedge_members(e)
                        .expect("valid hyperedge id")
                        .into_iter()
                        .map(|v| v.0)
                        .collect(),
                })
                .collect();

            // Struct wrapper kept explicit (rather than deriving directly on
            // WireHypergraph) so the top-level serialized shape is a plain
            // `{"vertices": [...], "hyperedges": [...]}` object regardless
            // of format.
            let mut state = serializer.serialize_struct("SpectralHypergraph", 2)?;
            state.serialize_field("vertices", &vertices)?;
            state.serialize_field("hyperedges", &hyperedges)?;
            state.end()
        }
    }

    impl<'de> Deserialize<'de> for SpectralHypergraph {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let wire = WireHypergraph::deserialize(deserializer)?;

            let mut builder = HypergraphBuilder::with_capacity(
                wire.vertices.len(),
                wire.hyperedges.len(),
                2,
            );
            for v in wire.vertices {
                builder
                    .add_weighted_vertex(v.label, v.weight)
                    .map_err(D::Error::custom)?;
            }
            for e in wire.hyperedges {
                let members: Vec<super::VertexId> =
                    e.members.into_iter().map(super::VertexId).collect();
                builder
                    .add_labeled_hyperedge(e.label, &members, e.weight)
                    .map_err(D::Error::custom)?;
            }
            builder.build().map_err(D::Error::custom)
        }
    }

    #[cfg(test)]
    mod tests {
        use crate::hypergraph::HypergraphBuilder;

        #[test]
        fn round_trips_through_json() {
            let mut b = HypergraphBuilder::new();
            let a = b.add_vertex("a").unwrap();
            let v = b.add_weighted_vertex("b", 2.5).unwrap();
            let c = b.add_vertex("c").unwrap();
            b.add_labeled_hyperedge(Some("triangle"), &[a, v, c], 3.0)
                .unwrap();
            let hg = b.build().unwrap();

            let json = serde_json::to_string(&hg).unwrap();
            let restored: crate::hypergraph::SpectralHypergraph =
                serde_json::from_str(&json).unwrap();

            assert_eq!(restored.num_vertices(), hg.num_vertices());
            assert_eq!(restored.num_hyperedges(), hg.num_hyperedges());
            assert_eq!(
                restored.vertex_weight(v).unwrap(),
                hg.vertex_weight(v).unwrap()
            );
            assert_eq!(
                restored.hyperedge_label(super::super::HyperEdgeId(0)).unwrap(),
                Some("triangle")
            );
            assert_eq!(
                restored.vertex_by_label("b").map(|id| id.0),
                hg.vertex_by_label("b").map(|id| id.0)
            );
        }

        #[test]
        fn rejects_malformed_document() {
            // A hyperedge member index that doesn't exist among the
            // declared vertices must surface as a deserialization error,
            // not a panicking or silently-truncated hypergraph.
            let bad = r#"{
                "vertices": [{"label": "a", "weight": 1.0}],
                "hyperedges": [{"label": null, "weight": 1.0, "members": [0, 7]}]
            }"#;
            let result: Result<crate::hypergraph::SpectralHypergraph, _> =
                serde_json::from_str(bad);
            assert!(result.is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_simple_hypergraph() {
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let c = b.add_vertex("c").unwrap();
        let d = b.add_vertex("d").unwrap();
        b.add_hyperedge(&[a, v, c], 1.0).unwrap();
        b.add_hyperedge(&[v, d], 2.0).unwrap();
        let hg = b.build().unwrap();

        assert_eq!(hg.num_vertices(), 4);
        assert_eq!(hg.num_hyperedges(), 2);
        assert_eq!(hg.vertex_degree(v).unwrap(), 3.0); // 1.0 + 2.0
        assert_eq!(hg.hyperedge_degree(HyperEdgeId(0)).unwrap(), 3);
        assert!(hg.is_degree_normalizable());
    }

    #[test]
    fn rejects_degenerate_hyperedge() {
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let err = b.add_hyperedge(&[a], 1.0).unwrap_err();
        assert_eq!(err, HypergraphError::DegenerateHyperEdge(1));
    }

    #[test]
    fn rejects_duplicate_vertex_label() {
        let mut b = HypergraphBuilder::new();
        b.add_vertex("a").unwrap();
        let err = b.add_vertex("a").unwrap_err();
        assert_eq!(err, HypergraphError::DuplicateVertex("a".to_string()));
    }

    #[test]
    fn rejects_unknown_vertex_in_hyperedge() {
        let mut b = HypergraphBuilder::new();
        b.add_vertex("a").unwrap();
        let ghost = VertexId(99);
        let err = b.add_hyperedge(&[VertexId(0), ghost], 1.0).unwrap_err();
        assert!(matches!(err, HypergraphError::IndexOutOfBounds { .. }));
    }

    #[test]
    fn detects_isolated_vertices() {
        let mut b = HypergraphBuilder::new();
        let a = b.add_vertex("a").unwrap();
        let v = b.add_vertex("b").unwrap();
        let iso = b.add_vertex("iso").unwrap();
        b.add_hyperedge(&[a, v], 1.0).unwrap();
        let hg = b.build().unwrap();
        assert_eq!(hg.isolated_vertices(), vec![iso]);
        assert!(!hg.is_degree_normalizable());
    }

    #[test]
    fn get_or_add_is_idempotent() {
        let mut b = HypergraphBuilder::new();
        let a1 = b.get_or_add_vertex("a").unwrap();
        let a2 = b.get_or_add_vertex("a").unwrap();
        assert_eq!(a1, a2);
        assert_eq!(b.num_vertices(), 1);
    }
}
