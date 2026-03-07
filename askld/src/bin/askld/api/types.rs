use askld::cfg::ControlFlowGraph;
use askld::parser::Rule;
use index::symbols::{FileId, SymbolId, SymbolType};
use serde::{Deserialize, Serialize, Serializer};

pub struct AsklData {
    pub cfg: ControlFlowGraph,
}

fn symbolid_as_string<S>(x: &SymbolId, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeDeclaration {
    pub id: String,
    pub symbol: String,
    pub file_id: String,
    pub project_id: String,
    pub symbol_type: SymbolType,
    pub start_offset: i32,
    pub end_offset: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_end_line: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Node {
    #[serde(serialize_with = "symbolid_as_string")]
    pub id: SymbolId,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    pub declarations: Vec<NodeDeclaration>,
}

impl Node {
    pub fn new(id: SymbolId, label: String, declarations: Vec<NodeDeclaration>) -> Self {
        Self {
            id,
            label,
            name: None,
            qualified_name: None,
            declarations,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    #[serde(serialize_with = "symbolid_as_string")]
    pub from: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    pub to: SymbolId,
    pub from_file: Option<FileId>,
    pub from_project_id: Option<String>,
    pub from_offset_start: Option<i32>,
    pub from_offset_end: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_qualified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee_qualified: Option<String>,
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
            from_file: occurrence.map(|o| o.file),
            from_project_id,
            from_offset_start: range.map(|r| r.0),
            from_offset_end: range.map(|r| r.1),
            caller: None,
            caller_qualified: None,
            callee: None,
            callee_qualified: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GraphFileEntry {
    pub file_id: String,
    pub path: String,
    pub project_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub files: Vec<GraphFileEntry>,
    pub warnings: Vec<ErrorResponse>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            files: vec![],
            warnings: vec![],
        }
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    pub fn add_warnings(&mut self, warnings: Vec<pest::error::Error<Rule>>) {
        for warning in warnings {
            let error_response = ErrorResponse {
                message: warning.to_string(),
                location: warning.location.clone().into(),
                line_col: warning.line_col.clone().into(),
                path: warning.path().map(|p| p.to_string()),
                line: warning.line().to_string(),
            };
            self.warnings.push(error_response);
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

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexUploadResponse {
    pub project_id: i32,
}

#[derive(Debug, Serialize)]
pub struct IndexDeleteResponse {
    pub project_id: i32,
    pub deleted: bool,
}

/// Slice byte content by offset range. Used by both REST and MCP endpoints.
pub fn slice_content(
    content: Vec<u8>,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
) -> Result<Vec<u8>, String> {
    let len = content.len();
    let start = start_offset.unwrap_or(0);
    let end = end_offset.unwrap_or(len as i64);
    if start < 0 || end < 0 {
        return Err("Offsets must be non-negative".to_string());
    }
    let start = start as usize;
    let end = end as usize;
    if start > end || end > len {
        return Err("Invalid offset range".to_string());
    }
    Ok(content[start..end].to_vec())
}
