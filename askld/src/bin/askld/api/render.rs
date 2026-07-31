//! Render a query result [`Graph`] as compact markdown for an AI agent.
//!
//! The JSON graph is token-heavy and hard for a model to reason over; this
//! renders the same information as `file:line`-anchored markdown. The query
//! decides *what* is in scope; the [`Projection`] decides *how much* source
//! rides along.
//!
//! Direction is uniform: a reference edge is printed `from → to` meaning "from
//! references/calls to", and a containment edge `parent ▸ child`. A single
//! well-understood convention avoids the ambiguity of guessing a query's
//! intent (caller vs. callee) from a flat graph.

use askld::line_index::LineIndex;
use index::symbols::{InstanceType, SymbolId, SymbolType};
use std::collections::{HashMap, HashSet};

use super::types::{ErrorResponse, Graph, LineColLocation, Node, NodeSymbolInstance};

/// How much source text to render alongside each focused symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Projection {
    /// `label (type) file:line` only — cheapest, for exploring wide.
    Names,
    /// Adds the first line of the symbol (its signature). The default.
    #[default]
    Signature,
    /// Adds the full source body, fenced.
    Body,
}

impl Projection {
    /// Parse the agent-facing projection name; `None` if unrecognised.
    pub fn from_name(name: &str) -> Option<Projection> {
        match name {
            "names" => Some(Projection::Names),
            "signature" => Some(Projection::Signature),
            "body" => Some(Projection::Body),
            _ => None,
        }
    }
}

/// Raw file content for the objects referenced by a graph, keyed by the
/// stringified `object_id` (matching [`NodeSymbolInstance::object_id`]).
pub type SourceMap = HashMap<String, Vec<u8>>;

#[derive(Clone, Copy)]
enum EdgeKind {
    /// A reference/call: rendered `→`.
    Ref,
    /// A containment relationship: rendered `▸`.
    Has,
}

impl EdgeKind {
    fn arrow(self) -> char {
        match self {
            EdgeKind::Ref => '→',
            EdgeKind::Has => '▸',
        }
    }
}

/// Render `graph` (produced by the query `query`) as markdown.
///
/// `sources` supplies file content for line-number / signature / body
/// resolution; objects absent from it degrade to a path without a line number
/// rather than failing.
pub fn render_markdown(
    query: &str,
    graph: &Graph,
    sources: &SourceMap,
    projection: Projection,
) -> String {
    let ctx = RenderCtx::new(graph, sources);

    let mut out = String::new();

    // Query section — fenced so a multi-line query renders verbatim instead of
    // bleeding past the heading.
    out.push_str("# Query\n```askl\n");
    out.push_str(query.trim());
    out.push_str("\n```\n\n");

    // Stats section.
    out.push_str("# Stats\n");
    out.push_str(&ctx.counts_line(graph));
    out.push('\n');

    // Warnings section — truncation notice + any engine warnings, before
    // results so caveats are seen first. Omitted when there are none.
    if graph.truncated || !graph.warnings.is_empty() {
        out.push_str("# Warnings\n");
        if graph.truncated {
            out.push_str(&format!(
                "- Result truncated to {} of {} symbols — narrow with a type filter \
                 (`{{ func }}`), `ignore(...)`, `project(...)`, or fewer hops.\n",
                graph.nodes.len(),
                graph.total_symbols,
            ));
        }
        for w in &graph.warnings {
            out.push_str(&format!("- {}\n", warning_bullet(w)));
        }
        out.push('\n');
    }

    // Results section.
    out.push_str("# Results\n");
    if graph.nodes.is_empty() {
        out.push_str("_no results_\n");
        return out;
    }

    // Adjacency: source symbol -> ordered, de-duplicated (target, kind).
    let mut children: HashMap<SymbolId, Vec<(SymbolId, EdgeKind)>> = HashMap::new();
    let mut seen_pair: HashSet<(SymbolId, SymbolId)> = HashSet::new();
    let mut is_source: HashSet<SymbolId> = HashSet::new();
    let mut is_target: HashSet<SymbolId> = HashSet::new();
    let mut record = |from: SymbolId, to: SymbolId, kind: EdgeKind| {
        is_source.insert(from);
        is_target.insert(to);
        if seen_pair.insert((from, to)) {
            children.entry(from).or_default().push((to, kind));
        }
    };
    for edge in &graph.edges {
        record(edge.from(), edge.to(), EdgeKind::Ref);
    }
    for he in &graph.has_edges {
        record(he.parent(), he.child(), EdgeKind::Has);
    }

    // Top-level nodes: any source of an edge, plus any node that is not a
    // target of one (isolated selections, definitions, search hits). A node
    // that is only a target is shown under its parent(s). Sorted by file:line
    // so output is deterministic and same-file symbols group together.
    let mut top_level: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| {
            let id = n.id();
            is_source.contains(&id) || !is_target.contains(&id)
        })
        .collect();
    top_level.sort_by(|a, b| ctx.sort_key(a).cmp(&ctx.sort_key(b)));

    for node in top_level {
        let id = node.id();
        if ctx.is_content(node) {
            ctx.render_content_node(node, projection, &mut out);
            continue;
        }

        out.push_str(&ctx.node_line(node, projection));
        out.push('\n');

        if let Some(kids) = children.get(&id) {
            let mut kids = kids.clone();
            kids.sort_by(|(a, ka), (b, kb)| {
                kind_rank(*ka)
                    .cmp(&kind_rank(*kb))
                    .then_with(|| ctx.target_sort_key(*a).cmp(&ctx.target_sort_key(*b)))
            });
            for (target, kind) in kids {
                out.push_str(&ctx.child_line(target, kind));
                out.push('\n');
            }
        }
    }

    out
}

struct RenderCtx<'a> {
    /// object_id -> file path.
    paths: HashMap<&'a str, &'a str>,
    /// object_id -> precomputed line table (only for objects with content).
    lines: HashMap<&'a str, LineIndex>,
    /// object_id -> raw content.
    sources: &'a SourceMap,
    nodes_by_id: HashMap<SymbolId, &'a Node>,
}

impl<'a> RenderCtx<'a> {
    fn new(graph: &'a Graph, sources: &'a SourceMap) -> Self {
        let paths = graph
            .objects
            .iter()
            .map(|o| (o.object_id.as_str(), o.path.as_str()))
            .collect();
        let lines = sources
            .iter()
            .map(|(oid, content)| (oid.as_str(), LineIndex::new(content)))
            .collect();
        let nodes_by_id = graph.nodes.iter().map(|n| (n.id(), n)).collect();
        RenderCtx {
            paths,
            lines,
            sources,
            nodes_by_id,
        }
    }

    fn counts_line(&self, graph: &Graph) -> String {
        // `search()`/`loc()` produce content nodes each holding many match
        // instances, so those count as "matches"; everything else is a symbol.
        let mut symbols = 0usize;
        let mut matches = 0usize;
        for node in &graph.nodes {
            if self.is_content(node) {
                matches += node.instances().len();
            } else {
                symbols += 1;
            }
        }
        let mut parts = Vec::new();
        if symbols > 0 || matches == 0 {
            if graph.truncated {
                parts.push(format!("{symbols} of {} symbols", graph.total_symbols));
            } else {
                parts.push(format!("{symbols} symbols"));
            }
        }
        if matches > 0 {
            parts.push(format!("{matches} matches"));
        }
        if !graph.edges.is_empty() {
            parts.push(format!("{} refs", graph.edges.len()));
        }
        if !graph.has_edges.is_empty() {
            parts.push(format!("{} contains", graph.has_edges.len()));
        }
        format!("{}\n", parts.join(" · "))
    }

    /// Deterministic ordering key for a node: (path, line, label).
    fn sort_key(&self, node: &Node) -> (String, usize, String) {
        match self.primary(node) {
            Some(inst) => {
                let oid = inst.object_id.as_str();
                let path = self.paths.get(oid).copied().unwrap_or("").to_string();
                let line = self
                    .lines
                    .get(oid)
                    .map(|li| li.line_of(inst.start_offset as usize))
                    .unwrap_or(0);
                (path, line, node.label().to_string())
            }
            None => (String::new(), 0, node.label().to_string()),
        }
    }

    /// Owned ordering key for an edge target (looked up by id).
    fn target_sort_key(&self, target: SymbolId) -> (String, usize, String) {
        match self.nodes_by_id.get(&target) {
            Some(node) => self.sort_key(node),
            None => (String::new(), 0, format!("#{}", target.0)),
        }
    }

    fn is_content(&self, node: &Node) -> bool {
        node.instances()
            .first()
            .map(|i| i.symbol_type == SymbolType::Content)
            .unwrap_or(false)
    }

    /// Prefer a definition, then a declaration, then any instance.
    fn primary<'n>(&self, node: &'n Node) -> Option<&'n NodeSymbolInstance> {
        let insts = node.instances();
        insts
            .iter()
            .find(|i| i.instance_type == InstanceType::Definition)
            .or_else(|| {
                insts
                    .iter()
                    .find(|i| i.instance_type == InstanceType::Declaration)
            })
            .or_else(|| insts.first())
    }

    fn location(&self, inst: &NodeSymbolInstance) -> String {
        let oid = inst.object_id.as_str();
        let path = self.paths.get(oid).copied().unwrap_or("?");
        match self.lines.get(oid) {
            Some(li) => format!("{}:{}", path, li.line_of(inst.start_offset as usize)),
            None => path.to_string(),
        }
    }

    fn slice(&self, inst: &NodeSymbolInstance) -> Option<&'a [u8]> {
        let content = self.sources.get(&inst.object_id)?;
        let start = inst.start_offset.max(0) as usize;
        let end = (inst.end_offset.max(0) as usize).min(content.len());
        content.get(start..end.max(start))
    }

    fn signature(&self, inst: &NodeSymbolInstance) -> Option<String> {
        let bytes = self.slice(inst)?;
        let first = bytes.split(|b| *b == b'\n').next().unwrap_or(bytes);
        Some(String::from_utf8_lossy(first).trim_end().to_string())
    }

    /// The full source line containing the match, grep-style. A `search()` match
    /// instance spans only the matched token, so its first line is just the
    /// query text; agents want the surrounding line for context.
    fn match_line(&self, inst: &NodeSymbolInstance) -> Option<String> {
        let content = self.sources.get(&inst.object_id)?;
        let off = (inst.start_offset.max(0) as usize).min(content.len());
        let start = content[..off]
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = content[off..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| off + i)
            .unwrap_or(content.len());
        Some(
            String::from_utf8_lossy(&content[start..end])
                .trim()
                .to_string(),
        )
    }

    fn node_line(&self, node: &Node, projection: Projection) -> String {
        let Some(inst) = self.primary(node) else {
            return node.label().to_string();
        };
        let mut line = format!(
            "{}  ({})  {}",
            node.label(),
            type_abbr(inst.symbol_type),
            self.location(inst),
        );
        if matches!(projection, Projection::Signature | Projection::Body) {
            if let Some(sig) = self.signature(inst) {
                line.push_str(&format!("\n    {sig}"));
            }
        }
        if projection == Projection::Body {
            if let Some((lang, body)) = self.body_text(node) {
                line.push_str(&format!("\n```{lang}\n{body}\n```"));
            }
        }
        line
    }

    /// The fenced body: `Documentation` + `Definition` instances stitched in
    /// file order (so a doc comment precedes its code). Macro `Expansion`
    /// instances are deliberately excluded — a widely-used macro has hundreds
    /// of them. Falls back to the primary instance when neither is present.
    fn body_text(&self, node: &Node) -> Option<(&'static str, String)> {
        let mut parts: Vec<&NodeSymbolInstance> = node
            .instances()
            .iter()
            .filter(|i| {
                matches!(
                    i.instance_type,
                    InstanceType::Definition | InstanceType::Documentation
                )
            })
            .collect();
        if parts.is_empty() {
            parts = self.primary(node).into_iter().collect();
        }
        parts.sort_by_key(|i| {
            (
                self.paths.get(i.object_id.as_str()).copied().unwrap_or(""),
                i.start_offset,
            )
        });

        let mut lang = "";
        let mut body = String::new();
        for inst in parts {
            let Some(bytes) = self.slice(inst) else {
                continue;
            };
            if lang.is_empty() {
                lang = fence_lang(self.paths.get(inst.object_id.as_str()).copied());
            }
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&String::from_utf8_lossy(bytes));
        }
        (!body.is_empty()).then_some((lang, body))
    }

    fn child_line(&self, target: SymbolId, kind: EdgeKind) -> String {
        match self.nodes_by_id.get(&target) {
            Some(node) => {
                let (ty, loc) = match self.primary(node) {
                    Some(inst) => (type_abbr(inst.symbol_type), self.location(inst)),
                    None => ("?", String::new()),
                };
                format!("  {} {}  ({})  {}", kind.arrow(), node.label(), ty, loc)
                    .trim_end()
                    .to_string()
            }
            None => format!("  {} #{}", kind.arrow(), target.0),
        }
    }

    /// A `search()`/`loc()` result is one content symbol holding many byte-range
    /// instances; render one line per match rather than collapsing to one.
    fn render_content_node(&self, node: &Node, projection: Projection, out: &mut String) {
        let mut insts: Vec<&NodeSymbolInstance> = node.instances().iter().collect();
        insts.sort_by_key(|i| {
            (
                self.paths.get(i.object_id.as_str()).copied().unwrap_or(""),
                i.start_offset,
            )
        });
        for inst in insts {
            let loc = self.location(inst);
            match projection {
                Projection::Names => out.push_str(&format!("{loc}\n")),
                Projection::Signature | Projection::Body => {
                    let matched = self.match_line(inst).unwrap_or_default();
                    out.push_str(&format!("{loc}   {matched}\n"));
                }
            }
        }
    }
}

/// One clean markdown bullet for a runtime warning: the semantic message, its
/// query line, and any name suggestions inline — no pest caret art.
fn warning_bullet(w: &ErrorResponse) -> String {
    let mut out = w.message.trim().to_string();
    if let LineColLocation::Span((line, _), _) = &w.line_col {
        out.push_str(&format!(" (line {line})"));
    }
    // Presentation of the no-match outcome lives here, not in the engine.
    if !w.suggestions.is_empty() {
        let names: Vec<String> = w.suggestions.iter().map(|n| format!("`{n}`")).collect();
        out.push_str(&format!(". Did you mean: {}?", names.join(", ")));
    } else if w.name_exists {
        out.push_str(
            ". The name exists but wasn't matched here — check the type, `project(...)`, \
             or relationship (caller/callee) filters.",
        );
    }
    out
}

fn kind_rank(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::Ref => 0,
        EdgeKind::Has => 1,
    }
}

fn type_abbr(t: SymbolType) -> &'static str {
    match t {
        SymbolType::Function => "func",
        SymbolType::File => "file",
        SymbolType::Module => "mod",
        SymbolType::Directory => "dir",
        SymbolType::Type => "type",
        SymbolType::Data => "data",
        SymbolType::Macro => "macro",
        SymbolType::Field => "field",
        SymbolType::Content => "match",
    }
}

fn fence_lang(path: Option<&str>) -> &'static str {
    let ext = path
        .and_then(|p| p.rsplit('.').next())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("c" | "h") => "c",
        Some("cc" | "cpp" | "cxx" | "hpp") => "cpp",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("py") => "python",
        Some("js" | "jsx") => "javascript",
        Some("ts" | "tsx") => "typescript",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Graph, Node, NodeSymbolInstance, QueryStatement};
    use index::symbols::{InstanceType, SymbolId, SymbolType};

    fn inst(
        object_id: &str,
        start: i32,
        end: i32,
        ty: SymbolType,
        it: InstanceType,
    ) -> NodeSymbolInstance {
        NodeSymbolInstance {
            id: "1".into(),
            symbol: "1".into(),
            object_id: object_id.into(),
            project_id: "3".into(),
            symbol_type: ty,
            instance_type: it,
            start_offset: start,
            end_offset: end,
        }
    }

    fn node(id: i64, label: &str, insts: Vec<NodeSymbolInstance>) -> Node {
        Node::new(
            SymbolId(id),
            label.into(),
            insts,
            Vec::<QueryStatement>::new(),
        )
    }

    fn obj(graph: &mut Graph, object_id: &str, path: &str) {
        graph.objects.push(crate::api::types::GraphObjectEntry {
            object_id: object_id.into(),
            path: path.into(),
            project_id: "3".into(),
        });
    }

    // "hello\nssize_t f(int x)\n{\n\treturn x;\n}\n"
    fn body_source() -> (SourceMap, i32, i32) {
        let text = b"hello\nssize_t f(int x)\n{\n\treturn x;\n}\n".to_vec();
        let start = 6; // start of line 2
        let end = text.len() as i32; // through end
        let mut m = SourceMap::new();
        m.insert("10".into(), text);
        (m, start, end)
    }

    #[test]
    fn signature_projection_renders_file_line_and_first_line() {
        let (src, start, end) = body_source();
        let mut g = Graph::new();
        obj(&mut g, "10", "/linux/fs/read_write.c");
        g.add_node(node(
            1,
            "f",
            vec![inst(
                "10",
                start,
                end,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));

        let md = render_markdown("\"f\"", &g, &src, Projection::Signature);
        // sectioned document with the query fenced verbatim
        assert!(md.contains("# Query"), "{md}");
        assert!(md.contains("```askl\n\"f\"\n```"), "{md}");
        assert!(md.contains("# Stats"), "{md}");
        assert!(md.contains("1 symbols"), "{md}");
        assert!(md.contains("# Results"), "{md}");
        // no warnings section when there are none
        assert!(!md.contains("# Warnings"), "{md}");
        // definition starts on line 2 of the file
        assert!(md.contains("f  (func)  /linux/fs/read_write.c:2"), "{md}");
        assert!(md.contains("    ssize_t f(int x)"), "{md}");
        // signature projection does not fence the *body* (the query fence is ```askl)
        assert!(!md.contains("```c"), "{md}");
    }

    #[test]
    fn names_projection_omits_signature() {
        let (src, start, end) = body_source();
        let mut g = Graph::new();
        obj(&mut g, "10", "/linux/fs/read_write.c");
        g.add_node(node(
            1,
            "f",
            vec![inst(
                "10",
                start,
                end,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));

        let md = render_markdown("\"f\"", &g, &src, Projection::Names);
        assert!(md.contains("f  (func)  /linux/fs/read_write.c:2"), "{md}");
        assert!(!md.contains("ssize_t f(int x)"), "{md}");
    }

    #[test]
    fn body_projection_fences_full_source() {
        let (src, start, end) = body_source();
        let mut g = Graph::new();
        obj(&mut g, "10", "/linux/fs/read_write.c");
        g.add_node(node(
            1,
            "f",
            vec![inst(
                "10",
                start,
                end,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));

        let md = render_markdown("\"f\"", &g, &src, Projection::Body);
        assert!(md.contains("```c"), "{md}");
        assert!(md.contains("return x;"), "{md}");
    }

    #[test]
    fn callee_edge_renders_arrow_child() {
        // root f (line 2) → callee g (line 1 of another file)
        let (mut src, start, end) = body_source();
        src.insert("20".into(), b"int g(void) { return 0; }\n".to_vec());

        let mut g = Graph::new();
        obj(&mut g, "10", "/a.c");
        obj(&mut g, "20", "/b.c");
        g.add_node(node(
            1,
            "f",
            vec![inst(
                "10",
                start,
                end,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));
        g.add_node(node(
            2,
            "g",
            vec![inst(
                "20",
                0,
                25,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));
        g.add_edge(crate::api::types::Edge::new(
            SymbolId(1),
            SymbolId(2),
            None,
            None,
        ));

        let md = render_markdown("\"f\" { func }", &g, &src, Projection::Names);
        assert!(md.contains("1 refs"), "{md}");
        // f is a source (top-level); g appears as an arrow child, not top-level
        assert!(md.contains("f  (func)  /a.c:2"), "{md}");
        assert!(md.contains("  → g  (func)  /b.c:1"), "{md}");
        // g should not also appear as its own top-level line
        let top_level_g = md.lines().any(|l| l.starts_with("g  (func)"));
        assert!(!top_level_g, "g should only appear as a child:\n{md}");
    }

    #[test]
    fn truncation_surfaces_in_stats_and_warnings() {
        let (src, start, end) = body_source();
        let mut g = Graph::new();
        obj(&mut g, "10", "/x.c");
        g.add_node(node(
            1,
            "f",
            vec![inst(
                "10",
                start,
                end,
                SymbolType::Function,
                InstanceType::Definition,
            )],
        ));
        g.truncated = true;
        g.total_symbols = 42;

        let md = render_markdown("\"f\"", &g, &src, Projection::Names);
        assert!(md.contains("1 of 42 symbols"), "{md}");
        assert!(md.contains("# Warnings"), "{md}");
        assert!(md.contains("truncated to 1 of 42 symbols"), "{md}");
    }

    #[test]
    fn body_stitches_documentation_before_definition_and_skips_expansions() {
        //          0                16 17                          42 43
        let text = b"/** doc for f */\nint f(void) { return 1; }\nf();\n".to_vec();
        let mut src = SourceMap::new();
        src.insert("10".into(), text);
        let mut g = Graph::new();
        obj(&mut g, "10", "/x.c");
        g.add_node(node(
            1,
            "f",
            vec![
                inst("10", 17, 42, SymbolType::Function, InstanceType::Definition),
                inst(
                    "10",
                    0,
                    16,
                    SymbolType::Function,
                    InstanceType::Documentation,
                ),
                inst("10", 43, 47, SymbolType::Function, InstanceType::Expansion),
            ],
        ));

        let md = render_markdown("\"f\"", &g, &src, Projection::Body);
        let body = md.split("```c").nth(1).expect("fenced body");
        let doc_pos = body.find("doc for f").expect("doc present");
        let def_pos = body.find("int f(void)").expect("def present");
        assert!(doc_pos < def_pos, "doc must precede def:\n{md}");
        // the macro expansion site must not be stitched in
        assert!(!body.contains("f();"), "expansion must be excluded:\n{md}");
    }

    #[test]
    fn search_content_node_renders_one_line_per_instance() {
        let mut src = SourceMap::new();
        // two matches of "foo" on lines 1 and 3
        src.insert("30".into(), b"foo bar\nbaz\nqux foo end\n".to_vec());
        let mut g = Graph::new();
        obj(&mut g, "30", "/c.c");
        g.add_node(node(
            9,
            "foo",
            vec![
                inst("30", 0, 3, SymbolType::Content, InstanceType::Definition),
                inst("30", 16, 19, SymbolType::Content, InstanceType::Definition),
            ],
        ));

        let md = render_markdown("search(\"foo\")", &g, &src, Projection::Signature);
        assert!(md.contains("/c.c:1"), "{md}");
        assert!(md.contains("/c.c:3"), "{md}");
        // each match is its own line (per-instance, not collapsed)
        assert_eq!(md.matches("/c.c:").count(), 2, "{md}");
        // the match renders its full source line (grep-style), not just the token
        assert!(md.contains("/c.c:1   foo bar"), "{md}");
        assert!(md.contains("/c.c:3   qux foo end"), "{md}");
    }

    #[test]
    fn projection_from_name_parses() {
        assert_eq!(Projection::from_name("names"), Some(Projection::Names));
        assert_eq!(
            Projection::from_name("signature"),
            Some(Projection::Signature)
        );
        assert_eq!(Projection::from_name("body"), Some(Projection::Body));
        assert_eq!(Projection::from_name("bogus"), None);
        assert_eq!(Projection::default(), Projection::Signature);
    }

    #[test]
    fn empty_graph_says_no_results() {
        let g = Graph::new();
        let src = SourceMap::new();
        let md = render_markdown("\"nope\"", &g, &src, Projection::Signature);
        assert!(md.contains("_no results_"), "{md}");
    }

    fn warning(
        message: &str,
        line: usize,
        token: Option<&str>,
        suggestions: &[&str],
    ) -> ErrorResponse {
        use super::super::types::InputLocation;
        ErrorResponse {
            message: message.into(),
            location: InputLocation::Span((0, 1)),
            line_col: LineColLocation::Span((line, 1), (line, 2)),
            path: None,
            line: String::new(),
            token: token.map(str::to_string),
            suggestions: suggestions.iter().map(|s| s.to_string()).collect(),
            name_exists: false,
        }
    }

    #[test]
    fn warning_bullet_is_clean_with_line_and_suggestions() {
        let w = warning(
            "`\"vfs_rea\"` matched no symbols",
            10,
            Some("vfs_rea"),
            &["vfs_read", "vfs_readv"],
        );
        let bullet = warning_bullet(&w);
        // no pest caret art
        assert!(!bullet.contains("^"), "{bullet}");
        assert!(!bullet.contains("-->"), "{bullet}");
        assert_eq!(
            bullet,
            "`\"vfs_rea\"` matched no symbols (line 10). Did you mean: `vfs_read`, `vfs_readv`?"
        );
    }

    #[test]
    fn warning_bullet_name_exists_phrases_not_a_typo() {
        let mut w = warning(
            "`\"vfs_read\"` matched no symbols",
            5,
            Some("vfs_read"),
            &[],
        );
        w.name_exists = true;
        let bullet = warning_bullet(&w);
        assert!(
            bullet.contains("name exists but wasn't matched here"),
            "{bullet}"
        );
        assert!(bullet.contains("type"), "{bullet}");
        assert!(!bullet.contains("Did you mean"), "{bullet}");
    }

    #[test]
    fn warning_bullet_without_suggestions_omits_did_you_mean() {
        let w = warning("something advisory", 3, None, &[]);
        let bullet = warning_bullet(&w);
        assert_eq!(bullet, "something advisory (line 3)");
    }
}
