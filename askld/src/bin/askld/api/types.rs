use askld::cfg::ControlFlowGraph;
use askld::diagnostic::Diagnostic;
use askld::parser::Rule;
use index::roles::RefKind;
use index::symbols::{InstanceType, SymbolId, SymbolInstanceId, SymbolType};
use serde::{Deserialize, Serialize, Serializer};

pub struct AsklData {
    pub cfg: ControlFlowGraph,
    pub query_timeout: std::time::Duration,
    /// Default cap on distinct symbols per result (0 = unlimited).
    pub max_result_symbols: usize,
    /// Cardinality-probe cap (see `ExecutionContext::probe_cap`).
    pub probe_cap: usize,
    /// Default per-node listing cap: instances per node and distinct
    /// out-edges per node (0 = disabled). Overridable per
    /// request (`?listing=`) and per command (`limits(listing=N)`).
    pub listing_cap: usize,
}

fn symbolid_as_string<S>(x: &SymbolId, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeSymbolInstance {
    pub id: String,
    pub symbol: String,
    pub object_id: String,
    pub project_id: String,
    pub symbol_type: SymbolType,
    pub instance_type: InstanceType,
    pub start_offset: i32,
    pub end_offset: i32,
    /// The line `start_offset` is on, counted from 1: what the markdown report
    /// prints after the path.  Set in the full JSON answer; absent where the
    /// file's content is not stored, and in the paged listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryStatement {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Node {
    #[serde(serialize_with = "symbolid_as_string")]
    id: SymbolId,
    label: String,
    /// What the symbol is.  Every instance says it too, but a node can ship
    /// none, and a reader should not have to open one to learn it.
    #[serde(default)]
    symbol_type: Option<SymbolType>,
    /// Not listed by default: nobody wrote this symbol down in normal code
    /// (a macro generated it for itself).  It is in the answer because the
    /// query named it or asked for `unnest`.
    #[serde(default)]
    folded: bool,
    symbol_instances: Vec<NodeSymbolInstance>,
    query_statements: Vec<QueryStatement>,
    /// True when `symbol_instances` was capped by `instance_cap` — assembly
    /// capping: the node is complete, its instance LISTING is paged
    /// (`/v1/symbol/{id}/instances` serves the rest).
    #[serde(default)]
    pub instances_truncated: bool,
    /// Exact instance count for this symbol.  For truncated nodes this is
    /// the symbol's REAL total from an index count (the result's own
    /// instance list is leaf-budget-bounded and would understate it);
    /// untruncated nodes report their shipped length.  Exact is fine here:
    /// a per-symbol count is bounded by the symbol's own size.
    #[serde(default)]
    pub total_instances: usize,
}

impl Node {
    pub fn new(
        id: SymbolId,
        label: String,
        symbol_instances: Vec<NodeSymbolInstance>,
        query_statements: Vec<QueryStatement>,
        instances_truncated: bool,
        total_instances: usize,
    ) -> Self {
        Self {
            id,
            label,
            symbol_type: symbol_instances.first().map(|i| i.symbol_type),
            folded: false,
            symbol_instances,
            query_statements,
            instances_truncated,
            total_instances,
        }
    }

    /// What the symbol's own row says, where there is one: its type (known
    /// even when no instance is shipped) and whether it is folded.
    pub fn of_symbol(mut self, symbol_type: SymbolType, folded: bool) -> Self {
        self.symbol_type = Some(symbol_type);
        self.folded = folded;
        self
    }

    pub fn id(&self) -> SymbolId {
        self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn instances(&self) -> &[NodeSymbolInstance] {
        &self.symbol_instances
    }

    pub fn instances_mut(&mut self) -> &mut [NodeSymbolInstance] {
        &mut self.symbol_instances
    }

    pub fn folded(&self) -> bool {
        self.folded
    }

    pub fn query_statements(&self) -> &[QueryStatement] {
        &self.query_statements
    }

    pub fn total_instances(&self) -> usize {
        self.total_instances
    }

    pub fn instances_truncated(&self) -> bool {
        self.instances_truncated
    }

    /// The node's symbol type: the symbol row's, or any instance's (all
    /// instances of a symbol share it).  `None` when neither is known.
    pub fn symbol_type(&self) -> Option<SymbolType> {
        self.symbol_type
    }
}

/// One edge of the result graph: a (from, to) SYMBOL pair plus how many call
/// sites it has.  Individual call sites are display detail, not structure —
/// a client pages them from `/v1/edge/{from}/{to}/occurrences` when it opens
/// the edge.  The `id` is recomputable as `from-to` and is not sent.
#[derive(Debug, Serialize, Deserialize)]
pub struct Edge {
    #[serde(serialize_with = "symbolid_as_string")]
    from: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    to: SymbolId,
    /// Call sites of this pair in the result (bounded by the discovery cap;
    /// a symbol whose out-edges were capped is listed in `edges_truncated`).
    occurrence_count: usize,
    /// Distinct files this pair's call sites live in — STRUCTURAL, not display
    /// detail: the client's multi-parent split routes an edge to the split
    /// whose files contain its call sites.  A handful of ints per pair, versus
    /// the per-occurrence rows this replaced.  Goes away when the split moves
    /// server-side (which has the occurrences in memory already).
    from_objects: Vec<String>,
    /// What the pair's references state about the target, taken together:
    /// `calls`, `writes`, `takes` / `reads`, `declares`, `holds`, in that
    /// order.  Computed here once (`index::roles::Stated::kinds`), so no
    /// reader guesses a call from a callback out of the two symbol types.
    /// Absent when nothing is stated, which is every edge of an index
    /// written before references carried roles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kinds: Vec<RefKind>,
}

impl Edge {
    pub fn new(
        from: SymbolId,
        to: SymbolId,
        occurrence_count: usize,
        from_objects: Vec<String>,
    ) -> Self {
        Self {
            from,
            to,
            occurrence_count,
            from_objects,
            kinds: Vec::new(),
        }
    }

    /// The edge, saying what its references state.
    pub fn with_kinds(mut self, kinds: Vec<RefKind>) -> Self {
        self.kinds = kinds;
        self
    }

    pub fn kinds(&self) -> &[RefKind] {
        &self.kinds
    }

    pub fn from(&self) -> SymbolId {
        self.from
    }

    pub fn to(&self) -> SymbolId {
        self.to
    }

    pub fn occurrence_count(&self) -> usize {
        self.occurrence_count
    }

    pub fn from_objects(&self) -> &[String] {
        &self.from_objects
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HasEdge {
    id: String,
    #[serde(serialize_with = "symbolid_as_string")]
    parent: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    child: SymbolId,
    parent_instance: String,
    child_instance: String,
}

impl HasEdge {
    pub fn new(
        parent: SymbolId,
        child: SymbolId,
        parent_instance: SymbolInstanceId,
        child_instance: SymbolInstanceId,
    ) -> Self {
        Self {
            id: format!(
                "has-{}-{}-{}-{}",
                parent, child, parent_instance, child_instance
            ),
            parent,
            child,
            parent_instance: parent_instance.to_string(),
            child_instance: child_instance.to_string(),
        }
    }

    pub fn parent(&self) -> SymbolId {
        self.parent
    }

    pub fn child(&self) -> SymbolId {
        self.child
    }

    pub fn parent_instance(&self) -> &str {
        &self.parent_instance
    }

    pub fn child_instance(&self) -> &str {
        &self.child_instance
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GraphObjectEntry {
    pub object_id: String,
    pub path: String,
    pub project_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub has_edges: Vec<HasEdge>,
    pub objects: Vec<GraphObjectEntry>,
    pub warnings: Vec<ErrorResponse>,
    /// True when ANY statement's symbols were capped to `max_result_symbols`
    /// (the cap is per statement — a selector never competes with another
    /// statement's fan-out).
    pub truncated: bool,
    /// Distinct symbol count across the whole result before capping.  When
    /// `incomplete` is set this counts what a limit let through, so it is a
    /// lower bound and not a total.
    pub total_symbols: usize,
    /// True when a limit cut the answer while it was being computed (the
    /// response carries the warning that says where).  Not the same thing
    /// as `truncated`: that one means the symbols were all found and the
    /// response shows the first of them, which a larger `limits(symbols=)`
    /// fixes; this one means the rest were never read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
    /// Node ids (symbol ids, same encoding as `Node.id`) whose implicit
    /// out-edge list was capped by `edge_cap` — assembly capping: each is an
    /// invitation to fetch that node's remaining references, never a sign the
    /// symbol set is incomplete.
    pub edges_truncated: Vec<String>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            has_edges: vec![],
            objects: vec![],
            warnings: vec![],
            truncated: false,
            total_symbols: 0,
            incomplete: false,
            edges_truncated: vec![],
        }
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    pub fn add_has_edge(&mut self, edge: HasEdge) {
        self.has_edges.push(edge);
    }

    pub fn add_warnings(&mut self, warnings: Vec<Diagnostic>) {
        // Every truncation notice says some limit cut the answer short.
        self.incomplete |= warnings
            .iter()
            .any(|w| w.kind == askld::diagnostic::DiagnosticKind::Truncation);
        for warning in &warnings {
            self.warnings.push(ErrorResponse::from_diagnostic(warning));
        }
    }
}

/// Where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum InputLocation {
    /// `Error` was created by `Error::new_from_pos`
    Pos(usize),
    /// `Error` was created by `Error::new_from_span`
    Span((usize, usize)),
}

impl From<pest::error::InputLocation> for InputLocation {
    fn from(loc: pest::error::InputLocation) -> Self {
        match loc {
            pest::error::InputLocation::Pos(pos) => InputLocation::Pos(pos),
            pest::error::InputLocation::Span(span) => InputLocation::Span(span),
        }
    }
}

/// Line/column where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum LineColLocation {
    /// Line/column pair if `Error` was created by `Error::new_from_pos`
    Pos((usize, usize)),
    /// Line/column pairs if `Error` was created by `Error::new_from_span`
    Span((usize, usize), (usize, usize)),
}

impl From<pest::error::LineColLocation> for LineColLocation {
    fn from(loc: pest::error::LineColLocation) -> Self {
        match loc {
            pest::error::LineColLocation::Pos(pos) => LineColLocation::Pos(pos),
            pest::error::LineColLocation::Span(start, end) => LineColLocation::Span(start, end),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub message: String,
    pub location: InputLocation,
    pub line_col: LineColLocation,
    pub path: Option<String>,
    pub line: String,
    /// The typed token this diagnostic is about (e.g. `vfs_rea`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Selectors that would match, for a no-match diagnostic: query text to
    /// type in place of the one that missed (`data("x")`, `file("/a/b.c")`, or
    /// the nearest existing names for a typo).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    /// True when the typed name exists but was excluded by a filter/scope
    /// (so it is not a typo).  `suggestions`, when present, are the spellings
    /// that find it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_exists: bool,
}

impl ErrorResponse {
    /// For a fatal parse/exec error: keep the pest rendering (with its `^---`
    /// caret) as the message — the caret is the right presentation there.
    pub fn from_pest(err: &pest::error::Error<Rule>) -> Self {
        Self {
            message: err.to_string(),
            location: err.location.clone().into(),
            line_col: err.line_col.clone().into(),
            path: err.path().map(|p| p.to_string()),
            line: err.line().to_string(),
            token: None,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    /// For a runtime warning: a clean semantic message (no caret art) plus the
    /// structured token/suggestions, derived from the diagnostic's [`Span`].
    pub fn from_diagnostic(d: &Diagnostic) -> Self {
        let pest_span = d.span.as_pest_span();
        let start = pest_span.start_pos().line_col();
        let end = pest_span.end_pos().line_col();
        Self {
            message: d.message.clone(),
            location: InputLocation::Span((d.span.start(), d.span.end())),
            line_col: LineColLocation::Span(start, end),
            path: None,
            line: d.span.line_text().to_string(),
            token: d.token.clone(),
            suggestions: d.suggestions.clone(),
            name_exists: d.name_exists,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexUploadResponse {
    pub project_id: i32,
    pub resumed: bool,
}

#[derive(Debug, Serialize)]
pub struct IndexDeleteResponse {
    pub project_id: i32,
    pub deleted: bool,
}
