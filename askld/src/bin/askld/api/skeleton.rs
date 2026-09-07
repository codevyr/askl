//! The skeleton graph: symbol RELATIONSHIPS only.
//!
//! `/query?detail=skeleton` answers with the graph's structure — which
//! symbols are in the result and how they relate — and nothing else. A
//! node's instances and an edge's call sites are display detail that a
//! client fetches per node (`/v1/symbol/{id}/instances`) or per edge
//! (`/v1/edge/{from}/{to}/occurrences`) when it opens them. Nothing is
//! inlined "representatively": one hand-picked instance is low value and
//! easy to pick wrong.
//!
//! The multi-parent SPLIT lives here rather than in the client. A symbol
//! that appears under several containers renders as one node per
//! containment context, which needs instance-level containment data — the
//! exact data the skeleton does not ship. The server has it in memory
//! already, so it splits and the client renders what it is given. Split
//! node ids are `{symbol}\0{context}` (NUL never occurs in an id).
//!
//! This is a pure transform over the assembled [`Graph`]; the in-memory
//! graph itself is untouched, so the markdown renderer and MCP's one-shot
//! `detail=full` keep the inlined shape.

use index::symbols::SymbolType;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

use super::types::{ErrorResponse, Graph, QueryStatement};

/// A node of the skeleton: a symbol (in one containment context) and the
/// counts a client needs to decide what to fetch.
#[derive(Debug, Serialize)]
pub struct SkeletonNode {
    /// Display id: the symbol id, or `{symbol}\0{context}` when split.
    pub id: String,
    /// The raw symbol(s) this node covers — the key the detail endpoints
    /// take. One entry today; a client that merges same-name nodes unions
    /// them.
    pub symbol_ids: Vec<String>,
    pub label: String,
    pub symbol_type: Option<SymbolType>,
    /// True for symbols that ARE containers (module/directory/file), read
    /// off the symbol type rather than off instance types, so group-node
    /// rendering needs no instances.
    pub is_directory: bool,
    /// Instances of this symbol in this context, as counted in the result —
    /// what the node badge shows. The symbol's exact total comes from the
    /// instances endpoint when the node is opened; the skeleton does not
    /// guess it (a result-derived "total" was the misleading 800-vs-931).
    pub instance_count: usize,
    /// The containment context this node was split into, if any: the parent
    /// SYMBOL id, or `null` for "not contained by anything in the result".
    /// A client pages this node's instances with `?parent=<this>`.
    pub parent_symbol: Option<String>,
    pub query_statements: Vec<QueryStatement>,
}

#[derive(Debug, Serialize)]
pub struct SkeletonEdge {
    /// Display endpoints (split node ids).
    pub from: String,
    pub to: String,
    /// The raw symbol pair — what `/v1/edge/{from}/{to}/occurrences` takes.
    pub from_symbol: String,
    pub to_symbol: String,
    pub occurrence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct SkeletonHasEdge {
    pub parent: String,
    pub child: String,
}

/// Note the absence of an `objects` map. Nothing in the skeleton references
/// a file: nodes carry no instances and edges no call sites. Every detail
/// row carries its own `path`/`project_id`, so a client learns file identity
/// as it loads detail — which removed 90 KB of 140 KB on a 101-node graph,
/// where 801 object entries described files nothing pointed at.
#[derive(Debug, Serialize)]
pub struct SkeletonGraph {
    pub nodes: Vec<SkeletonNode>,
    pub edges: Vec<SkeletonEdge>,
    pub has_edges: Vec<SkeletonHasEdge>,
    pub warnings: Vec<ErrorResponse>,
    pub truncated: bool,
    pub total_symbols: usize,
    pub edges_truncated: Vec<String>,
}

/// Containment context of one instance: the parent SYMBOL that contains it,
/// or `None` when nothing in the result does.
type Context = Option<String>;

fn context_key(ctx: &Context) -> String {
    match ctx {
        Some(parent) => format!("contained-by:{parent}"),
        None => "root".to_string(),
    }
}

/// Symbol types that render as containers.  Derived from the SYMBOL, not
/// from instance types, so a skeleton node needs no instances to be drawn
/// as a group.
fn is_container_type(symbol_type: Option<SymbolType>) -> bool {
    matches!(
        symbol_type,
        Some(SymbolType::Module | SymbolType::Directory | SymbolType::File)
    )
}

/// Derive the skeleton from an assembled graph, splitting symbols that
/// appear in several containment contexts.
///
/// Mirrors the client transform it replaces (`splitMultiParentNodes`), step
/// for step, so the two can be compared: containment map, context per
/// instance, split nodes, has-edges between splits, then ref edges remapped
/// by call-site file with a containment fallback.
pub fn to_skeleton(graph: Graph) -> SkeletonGraph {
    // 1. Instance-level containment: which parent instance holds each child.
    let mut child_inst_to_parent: HashMap<&str, (String, &str)> = HashMap::new();
    for he in &graph.has_edges {
        child_inst_to_parent.insert(
            he.child_instance(),
            (he.parent().to_string(), he.parent_instance()),
        );
    }

    // 2. Context per instance, and the set of contexts per symbol.
    let mut instance_context: HashMap<&str, Context> = HashMap::new();
    let mut symbol_contexts: HashMap<String, Vec<Context>> = HashMap::new();
    for node in &graph.nodes {
        let mut contexts: Vec<Context> = Vec::new();
        for inst in node.instances() {
            let ctx: Context = child_inst_to_parent
                .get(inst.id.as_str())
                .map(|(parent, _)| parent.clone());
            if !contexts.contains(&ctx) {
                contexts.push(ctx.clone());
            }
            instance_context.insert(inst.id.as_str(), ctx);
        }
        symbol_contexts.insert(node.id().to_string(), contexts);
    }

    // 3. Build skeleton nodes, splitting where a symbol has several
    //    contexts. `instance_to_node` carries the remapping the edges need.
    let mut nodes: Vec<SkeletonNode> = Vec::new();
    let mut instance_to_node: HashMap<&str, String> = HashMap::new();
    let mut node_objects: HashMap<String, HashSet<&str>> = HashMap::new();
    let mut split_lookup: HashMap<String, Vec<String>> = HashMap::new();
    let mut node_parent: HashMap<String, String> = HashMap::new();

    for node in &graph.nodes {
        let symbol = node.id().to_string();
        let contexts = symbol_contexts.get(&symbol).cloned().unwrap_or_default();
        let split = contexts.len() > 1;
        let symbol_type = node.symbol_type();

        for ctx in &contexts {
            let id = if split {
                format!("{symbol}\u{0}{}", context_key(ctx))
            } else {
                symbol.clone()
            };
            let instances: Vec<&super::types::NodeSymbolInstance> = node
                .instances()
                .iter()
                .filter(|i| instance_context.get(i.id.as_str()) == Some(ctx))
                .collect();

            for inst in &instances {
                instance_to_node.insert(inst.id.as_str(), id.clone());
                node_objects
                    .entry(id.clone())
                    .or_default()
                    .insert(inst.object_id.as_str());
            }
            if let Some(parent) = ctx {
                node_parent.insert(id.clone(), parent.clone());
            }
            if split {
                split_lookup
                    .entry(symbol.clone())
                    .or_default()
                    .push(id.clone());
            }

            // A split node's counts describe its own context; only an
            // unsplit node can claim the symbol's total.
            let instance_count = instances.len();
            nodes.push(SkeletonNode {
                id,
                symbol_ids: vec![symbol.clone()],
                label: node.label().to_string(),
                symbol_type,
                is_directory: is_container_type(symbol_type),
                instance_count,
                parent_symbol: ctx.clone(),
                query_statements: node.query_statements().to_vec(),
            });
        }
    }

    // 4. Has-edges between (possibly split) nodes, deduped by pair.
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let mut has_edges: Vec<SkeletonHasEdge> = Vec::new();
    for he in &graph.has_edges {
        let (Some(parent), Some(child)) = (
            instance_to_node.get(he.parent_instance()),
            instance_to_node.get(he.child_instance()),
        ) else {
            continue;
        };
        if seen_pairs.insert((parent.clone(), child.clone())) {
            has_edges.push(SkeletonHasEdge {
                parent: parent.clone(),
                child: child.clone(),
            });
        }
    }

    // 5. Ref edges: route each endpoint to the right split.  A `from` goes
    //    to the split whose files hold the call sites; a `to` goes to the
    //    split sharing a containment ancestor with the source.
    let mut child_to_parent: HashMap<&str, &str> = HashMap::new();
    for he in &has_edges {
        child_to_parent.insert(he.child.as_str(), he.parent.as_str());
    }
    let ancestors = |node: &str| -> HashSet<String> {
        let mut seen = HashSet::new();
        let mut current = child_to_parent.get(node).copied();
        while let Some(c) = current {
            if !seen.insert(c.to_string()) {
                break;
            }
            current = child_to_parent.get(c).copied();
        }
        seen
    };

    let mut edges: Vec<SkeletonEdge> = Vec::new();
    for edge in &graph.edges {
        let from_symbol = edge.from().to_string();
        let to_symbol = edge.to().to_string();
        let from_splits = split_lookup.get(&from_symbol);
        let to_splits = split_lookup.get(&to_symbol);

        let make = |from: String, to: String| SkeletonEdge {
            from,
            to,
            from_symbol: from_symbol.clone(),
            to_symbol: to_symbol.clone(),
            occurrence_count: edge.occurrence_count(),
        };

        match (from_splits, to_splits) {
            (None, None) => edges.push(make(from_symbol.clone(), to_symbol.clone())),
            (Some(froms), None) if from_symbol == to_symbol => {
                // Self-loop on a split symbol: one per split.
                for split in froms {
                    edges.push(make(split.clone(), split.clone()));
                }
            }
            (Some(froms), None) => {
                let matched = froms
                    .iter()
                    .find(|split| {
                        node_objects.get(*split).is_some_and(|objs| {
                            edge.from_objects()
                                .iter()
                                .any(|o| objs.contains(o.as_str()))
                        })
                    })
                    .or_else(|| froms.first());
                if let Some(split) = matched {
                    edges.push(make(split.clone(), to_symbol.clone()));
                }
            }
            (None, Some(tos)) => {
                let mut from_scope = ancestors(&from_symbol);
                from_scope.insert(from_symbol.clone());
                match tos.iter().find(|split| {
                    node_parent
                        .get(*split)
                        .is_some_and(|p| from_scope.contains(p))
                }) {
                    Some(split) => edges.push(make(from_symbol.clone(), split.clone())),
                    // No shared container: the edge reaches every context.
                    None => {
                        for split in tos {
                            edges.push(make(from_symbol.clone(), split.clone()));
                        }
                    }
                }
            }
            (Some(froms), Some(tos)) => {
                for from_split in froms {
                    let from_parent = node_parent.get(from_split);
                    let to_split = from_parent
                        .and_then(|fp| {
                            tos.iter()
                                .find(|t| node_parent.get(*t).is_some_and(|tp| tp == fp))
                        })
                        .or_else(|| tos.first());
                    let Some(to_split) = to_split else { continue };
                    // Keep only the split that actually holds the call sites.
                    if !edge.from_objects().is_empty() {
                        let holds = node_objects.get(from_split).is_some_and(|objs| {
                            edge.from_objects()
                                .iter()
                                .any(|o| objs.contains(o.as_str()))
                        });
                        if !holds {
                            continue;
                        }
                    }
                    edges.push(make(from_split.clone(), to_split.clone()));
                }
            }
        }
    }

    SkeletonGraph {
        nodes,
        edges,
        has_edges,
        warnings: graph.warnings,
        truncated: graph.truncated,
        total_symbols: graph.total_symbols,
        edges_truncated: graph.edges_truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Edge, HasEdge, Node, NodeSymbolInstance};
    use index::symbols::{InstanceType, SymbolId, SymbolInstanceId, SymbolType};

    fn inst(id: &str, symbol: i64, object_id: &str, ty: SymbolType) -> NodeSymbolInstance {
        NodeSymbolInstance {
            id: id.into(),
            symbol: symbol.to_string(),
            object_id: object_id.into(),
            project_id: "p".into(),
            symbol_type: ty,
            instance_type: InstanceType::Definition,
            start_offset: 0,
            end_offset: 1,
        }
    }

    fn node(id: i64, label: &str, insts: Vec<NodeSymbolInstance>) -> Node {
        let total = insts.len();
        Node::new(SymbolId(id), label.into(), insts, Vec::new(), false, total)
    }

    fn has(parent: i64, child: i64, parent_inst: i64, child_inst: i64) -> HasEdge {
        HasEdge::new(
            SymbolId(parent),
            SymbolId(child),
            SymbolInstanceId::new(parent_inst),
            SymbolInstanceId::new(child_inst),
        )
    }

    fn graph_of(nodes: Vec<Node>, has_edges: Vec<HasEdge>, edges: Vec<Edge>) -> Graph {
        let mut g = Graph::new();
        g.nodes = nodes;
        g.has_edges = has_edges;
        g.edges = edges;
        g
    }

    #[test]
    fn a_symbol_in_one_context_is_not_split() {
        // Both instances of 2 sit under the same parent: one node, plain id.
        let g = graph_of(
            vec![
                node(1, "p", vec![inst("10", 1, "o1", SymbolType::Module)]),
                node(
                    2,
                    "c",
                    vec![
                        inst("20", 2, "o1", SymbolType::Function),
                        inst("21", 2, "o1", SymbolType::Function),
                    ],
                ),
            ],
            vec![has(1, 2, 10, 20), has(1, 2, 10, 21)],
            vec![],
        );
        let sk = to_skeleton(g);
        let child = sk.nodes.iter().find(|n| n.label == "c").unwrap();
        assert_eq!(child.id, "2", "no split, so the id stays the symbol id");
        assert_eq!(child.instance_count, 2);
        assert_eq!(child.parent_symbol.as_deref(), Some("1"));
        assert_eq!(sk.has_edges.len(), 1, "one pair, deduped");
    }

    #[test]
    fn a_symbol_under_two_parents_splits_per_context() {
        // Symbol 3 appears under 1 and under 2 → two nodes, each scoped.
        let g = graph_of(
            vec![
                node(1, "p1", vec![inst("10", 1, "o1", SymbolType::Module)]),
                node(2, "p2", vec![inst("20", 2, "o2", SymbolType::Module)]),
                node(
                    3,
                    "c",
                    vec![
                        inst("30", 3, "o1", SymbolType::Function),
                        inst("31", 3, "o2", SymbolType::Function),
                    ],
                ),
            ],
            vec![has(1, 3, 10, 30), has(2, 3, 20, 31)],
            vec![],
        );
        let sk = to_skeleton(g);
        let splits: Vec<&SkeletonNode> = sk.nodes.iter().filter(|n| n.label == "c").collect();
        assert_eq!(splits.len(), 2, "one node per containment context");
        for s in &splits {
            assert!(s.id.contains('\u{0}'), "split ids are suffixed: {}", s.id);
            assert_eq!(s.symbol_ids, vec!["3".to_string()], "raw symbol preserved");
            assert_eq!(s.instance_count, 1, "each split counts its own context");
        }
        let parents: HashSet<Option<String>> =
            splits.iter().map(|s| s.parent_symbol.clone()).collect();
        assert_eq!(
            parents,
            HashSet::from([Some("1".to_string()), Some("2".to_string())]),
            "each split names the parent it pages instances under"
        );
        // Containment now points at the splits, not the bare symbol.
        assert_eq!(sk.has_edges.len(), 2);
        assert!(sk.has_edges.iter().all(|h| h.child.contains('\u{0}')));
    }

    #[test]
    fn an_edge_follows_its_call_sites_to_the_right_split() {
        // 3 is split across o1 (under 1) and o2 (under 2); an edge whose call
        // sites are in o2 must attach to the o2 split, not the first one.
        let g = graph_of(
            vec![
                node(1, "p1", vec![inst("10", 1, "o1", SymbolType::Module)]),
                node(2, "p2", vec![inst("20", 2, "o2", SymbolType::Module)]),
                node(
                    3,
                    "c",
                    vec![
                        inst("30", 3, "o1", SymbolType::Function),
                        inst("31", 3, "o2", SymbolType::Function),
                    ],
                ),
                node(4, "t", vec![inst("40", 4, "o3", SymbolType::Function)]),
            ],
            vec![has(1, 3, 10, 30), has(2, 3, 20, 31)],
            vec![Edge::new(
                SymbolId(3),
                SymbolId(4),
                2,
                vec!["o2".to_string()],
            )],
        );
        let sk = to_skeleton(g);
        assert_eq!(sk.edges.len(), 1);
        let edge = &sk.edges[0];
        assert_eq!(
            edge.from, "3\u{0}contained-by:2",
            "routed by call-site file"
        );
        assert_eq!(edge.to, "4");
        assert_eq!(
            (edge.from_symbol.as_str(), edge.to_symbol.as_str()),
            ("3", "4"),
            "the raw pair rides along for the occurrences endpoint"
        );
        assert_eq!(edge.occurrence_count, 2);
    }

    #[test]
    fn a_container_symbol_is_flagged_without_instance_types() {
        let g = graph_of(
            vec![node(
                1,
                "mod",
                vec![inst("10", 1, "o1", SymbolType::Module)],
            )],
            vec![],
            vec![],
        );
        let sk = to_skeleton(g);
        assert!(
            sk.nodes[0].is_directory,
            "group-node rendering reads the SYMBOL type, so no instances are needed"
        );
        assert_eq!(sk.nodes[0].parent_symbol, None, "nothing contains it");
    }

    #[test]
    fn the_skeleton_ships_no_instances() {
        // The point of the shape: structure only.
        let g = graph_of(
            vec![node(
                1,
                "f",
                vec![inst("10", 1, "o1", SymbolType::Function)],
            )],
            vec![],
            vec![],
        );
        let json = serde_json::to_string(&to_skeleton(g)).unwrap();
        assert!(
            !json.contains("symbol_instances") && !json.contains("start_offset"),
            "skeleton must carry no per-instance data: {json}"
        );
        assert!(json.contains("instance_count"), "counts stay: {json}");
    }
}
