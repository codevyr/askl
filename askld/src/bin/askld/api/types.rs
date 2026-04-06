use askld::cfg::ControlFlowGraph;
use askld::parser::Rule;
use index::symbols::{FileId, InstanceType, SymbolId, SymbolInstanceId, SymbolType};
use serde::{Deserialize, Serialize, Serializer};

pub struct AsklData {
    pub cfg: ControlFlowGraph,
    pub query_timeout: std::time::Duration,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Node {
    #[serde(serialize_with = "symbolid_as_string")]
    id: SymbolId,
    label: String,
    symbol_instances: Vec<NodeSymbolInstance>,
}

impl Node {
    pub fn new(id: SymbolId, label: String, symbol_instances: Vec<NodeSymbolInstance>) -> Self {
        Self {
            id,
            label,
            symbol_instances,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Edge {
    id: String,
    #[serde(serialize_with = "symbolid_as_string")]
    from: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    to: SymbolId,
    from_object: Option<FileId>,
    from_project_id: Option<String>,
    from_offset_start: Option<i32>,
    from_offset_end: Option<i32>,
}

impl Edge {
    pub fn new(
        from: SymbolId,
        to: SymbolId,
        occurrence: Option<index::symbols::Occurrence>,
        from_project_id: Option<String>,
    ) -> Self {
        let range = occurrence.as_ref().map(|o| o.offset_range.clone());
        Self {
            id: format!("{}-{}", from, to),
            from: from,
            to: to,
            from_object: occurrence.map(|o| o.file),
            from_project_id,
            from_offset_start: range.map(|r| r.0),
            from_offset_end: range.map(|r| r.1),
        }
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
            id: format!("has-{}-{}-{}-{}", parent, child, parent_instance, child_instance),
            parent,
            child,
            parent_instance: parent_instance.to_string(),
            child_instance: child_instance.to_string(),
        }
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
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            has_edges: vec![],
            objects: vec![],
            warnings: vec![],
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

    pub fn add_warnings(&mut self, warnings: Vec<pest::error::Error<Rule>>) {
        for warning in &warnings {
            self.warnings.push(ErrorResponse::from_pest(warning));
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
}

impl ErrorResponse {
    pub fn from_pest(err: &pest::error::Error<Rule>) -> Self {
        Self {
            message: err.to_string(),
            location: err.location.clone().into(),
            line_col: err.line_col.clone().into(),
            path: err.path().map(|p| p.to_string()),
            line: err.line().to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexUploadResponse {
    pub project_id: i32,
}

#[derive(Debug, Serialize)]
pub struct IndexDeleteResponse {
    pub project_id: i32,
    pub deleted: bool,
}
