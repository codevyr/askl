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
//! intent (caller vs. callee) from a flat graph.  The report states the
//! convention itself, in one line under `# Results`, for the marks it uses.
//!
//! A PAIR IS DRAWN ONCE, WITH EVERY RELATIONSHIP IT HAS: one mark for each,
//! `▸` contains, `→` refers to, `↦` declares, `⇢` holds, as in `▸,→ member`.
//! The report does not choose which of them matters.  What the pair's
//! references DO is said in words after the target's type, where the index
//! states it: `→ handler  (func, takes)`.  Marks and words are the engine's
//! (`Edge::kinds`, `Edge::refers`, the containment edges), not worked out
//! again here.

use askld::line_index::LineIndex;
use index::roles::RefKind;
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

/// A relationship a pair of symbols can have, and the mark it is drawn with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The source contains the target: `▸`.
    Contains,
    /// Some token of the source names the target: `→`.
    Refers,
    /// The source's invocation declared the target: `↦`.
    Declares,
    /// The source is a dispatch point and the target is stored in it: `⇢`.
    Holds,
}

impl Mark {
    /// Every mark, in the ONE order they are written: on a line and in the
    /// legend.
    const ALL: [Mark; 4] = [Mark::Contains, Mark::Refers, Mark::Declares, Mark::Holds];

    fn arrow(self) -> char {
        match self {
            Mark::Contains => '▸',
            Mark::Refers => '→',
            Mark::Declares => '↦',
            Mark::Holds => '⇢',
        }
    }

    fn legend(self) -> &'static str {
        match self {
            Mark::Contains => "`▸` contains",
            Mark::Refers => "`→` refers to",
            Mark::Declares => "`↦` declares",
            Mark::Holds => "`⇢` holds",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Mark::Contains => 1,
            Mark::Refers => 2,
            Mark::Declares => 4,
            Mark::Holds => 8,
        }
    }

    /// Where a line with this mark goes among a symbol's child lines: what
    /// it refers to, what it declares, what it holds, what it contains.
    fn rank(self) -> u8 {
        match self {
            Mark::Refers => 0,
            Mark::Declares => 1,
            Mark::Holds => 2,
            Mark::Contains => 3,
        }
    }
}

/// The relationships of one pair of symbols.
///
/// EVERY ONE OF THEM IS DRAWN.  A file contains a function and refers to it
/// wherever another of its functions calls it; a slot can hold a function a
/// token of it also names.  Each is a fact about the pair, and which of them
/// the reader is after is the reader's business: a rule that picked one (as
/// "a pair that contains is drawn `▸`" did) hid the others.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Marks(u8);

impl Marks {
    fn add(&mut self, mark: Mark) {
        self.0 |= mark.bit();
    }

    fn has(self, mark: Mark) -> bool {
        self.0 & mark.bit() != 0
    }

    fn iter(self) -> impl Iterator<Item = Mark> {
        Mark::ALL.into_iter().filter(move |mark| self.has(*mark))
    }

    /// The relationships an edge of the answer stands for.  A relation is
    /// what its kind says and nothing else; an edge refers when the engine
    /// says a token names the target, and an edge that states no relation at
    /// all is a reference whatever it says.
    fn of_edge(edge: &super::types::Edge) -> Marks {
        let mut marks = Marks::default();
        if edge.kinds().contains(&RefKind::Declares) {
            marks.add(Mark::Declares);
        }
        if edge.kinds().contains(&RefKind::Holds) {
            marks.add(Mark::Holds);
        }
        if edge.refers() || marks == Marks::default() {
            marks.add(Mark::Refers);
        }
        marks
    }

    /// As drawn at the head of a line: `▸,→`.
    fn drawn(self) -> String {
        let arrows: Vec<String> = self.iter().map(|mark| mark.arrow().to_string()).collect();
        arrows.join(",")
    }

    /// The order of a symbol's child lines: by what the pair has, mark by
    /// mark in the order of [`Mark::rank`].  So what a symbol only refers to
    /// comes first, what it only contains last, and what it does both to in
    /// between.
    fn rank(self) -> Vec<u8> {
        let mut ranks: Vec<u8> = self.iter().map(Mark::rank).collect();
        ranks.sort_unstable();
        ranks
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
                total_symbols_text(graph),
            ));
        }
        // Two statements can earn the same words; said once is enough.
        let mut said: Vec<String> = Vec::new();
        for w in &graph.warnings {
            let bullet = warning_bullet(w);
            if !said.contains(&bullet) {
                out.push_str(&format!("- {}\n", bullet));
                said.push(bullet);
            }
        }
        out.push('\n');
    }

    // Results section.
    out.push_str("# Results\n");
    if graph.nodes.is_empty() {
        out.push_str("_no results_\n");
        return out;
    }

    // Adjacency: source symbol -> its targets, each once, and beside it what
    // every pair has.  A pair arrives once per relationship (as a containment
    // edge, as a reference edge) and is one line.
    let mut children: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
    let mut pair_marks: HashMap<(SymbolId, SymbolId), Marks> = HashMap::new();
    let mut is_source: HashSet<SymbolId> = HashSet::new();
    let mut is_target: HashSet<SymbolId> = HashSet::new();
    let mut record = |from: SymbolId, to: SymbolId, marks: Marks| {
        is_source.insert(from);
        is_target.insert(to);
        let pair = pair_marks.entry((from, to)).or_insert_with(|| {
            children.entry(from).or_default().push(to);
            Marks::default()
        });
        pair.0 |= marks.0;
    };
    for he in &graph.has_edges {
        let mut contains = Marks::default();
        contains.add(Mark::Contains);
        record(he.parent(), he.child(), contains);
    }
    for edge in &graph.edges {
        record(edge.from(), edge.to(), Marks::of_edge(edge));
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

    // THE REPORT SAYS WHAT ITS MARKS MEAN: the ones it uses, once, before the
    // first of them.  Two marks hang under a symbol, and nothing in the
    // answer told a call from a member.
    let drawn = |mark: Mark| pair_marks.values().any(|marks| marks.has(mark));
    let legend: Vec<&str> = Mark::ALL
        .into_iter()
        .filter(|mark| drawn(*mark))
        .map(Mark::legend)
        .collect();
    if !legend.is_empty() {
        out.push_str(&legend.join(", "));
        out.push('\n');
    }

    for node in top_level {
        let id = node.id();
        if ctx.is_content(node) {
            ctx.render_content_node(node, projection, &mut out);
            continue;
        }

        out.push_str(&ctx.node_line(node, projection));
        out.push('\n');

        if let Some(kids) = children.get(&id) {
            let marks_of = |target: SymbolId| pair_marks[&(id, target)];
            let mut kids = kids.clone();
            kids.sort_by(|a, b| {
                marks_of(*a).rank().cmp(&marks_of(*b).rank()).then_with(|| {
                    ctx.target_sort_key(id, *a)
                        .cmp(&ctx.target_sort_key(id, *b))
                })
            });
            for target in kids {
                out.push_str(&ctx.child_line(id, target, marks_of(target)));
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
    /// (parent, child) -> how the parent led to the child.
    pair_lead: HashMap<(SymbolId, SymbolId), Lead<'a>>,
    /// (parent, child) -> what the pair's edge states, in the engine's words.
    pair_kinds: HashMap<(SymbolId, SymbolId), &'a [RefKind]>,
    /// Symbol -> how the result led to it, from everywhere.
    own_lead: HashMap<SymbolId, Lead<'a>>,
}

/// How the result reached a symbol: what says which of its instances is meant.
#[derive(Default)]
struct Lead<'a> {
    /// Instances a containment edge names.  Exact.
    instances: Vec<&'a str>,
    /// Files whose text refers to the symbol.
    objects: Vec<&'a str>,
}

/// The instance a symbol is shown at, and what to say about the others.
struct Shown<'n> {
    inst: &'n NodeSymbolInstance,
    /// "(1 of 47 definitions)" when the instance was a choice among several.
    others: Option<String>,
}

/// How many leading directories two paths share.
fn shared_dirs(a: &str, b: &str) -> usize {
    fn dirs(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
    dirs(a)
        .split('/')
        .zip(dirs(b).split('/'))
        .take_while(|(x, y)| x == y)
        .count()
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

        let mut pair_lead: HashMap<(SymbolId, SymbolId), Lead<'a>> = HashMap::new();
        let pair_kinds = graph
            .edges
            .iter()
            .map(|edge| ((edge.from(), edge.to()), edge.kinds()))
            .collect();
        let mut own_lead: HashMap<SymbolId, Lead<'a>> = HashMap::new();
        // Where a symbol's own references are written: what is left to go
        // by for one that nothing in the result refers to.
        let mut written_in: HashMap<SymbolId, Vec<&'a str>> = HashMap::new();
        for edge in &graph.edges {
            let files = edge.from_objects().iter().map(String::as_str);
            pair_lead
                .entry((edge.from(), edge.to()))
                .or_default()
                .objects
                .extend(files.clone());
            // A symbol's reference to itself says where it is, not who led
            // to it.
            if edge.from() != edge.to() {
                own_lead
                    .entry(edge.to())
                    .or_default()
                    .objects
                    .extend(files.clone());
            }
            written_in.entry(edge.from()).or_default().extend(files);
        }
        for he in &graph.has_edges {
            pair_lead
                .entry((he.parent(), he.child()))
                .or_default()
                .instances
                .push(he.child_instance());
            own_lead
                .entry(he.child())
                .or_default()
                .instances
                .push(he.child_instance());
            own_lead
                .entry(he.parent())
                .or_default()
                .instances
                .push(he.parent_instance());
        }
        for (symbol, files) in written_in {
            let lead = own_lead.entry(symbol).or_default();
            if lead.objects.is_empty() {
                lead.objects = files;
            }
        }

        RenderCtx {
            paths,
            lines,
            sources,
            nodes_by_id,
            pair_lead,
            pair_kinds,
            own_lead,
        }
    }

    /// What follows the target's type on a child line: what the pair's
    /// references DO, in the engine's words and order.  A relation is not
    /// among them: it is a relationship of the pair, and a relationship is
    /// said by its mark, once.  Empty when the index states nothing, which
    /// leaves the line as it always was.
    fn said(&self, parent: SymbolId, target: SymbolId) -> String {
        self.pair_kinds
            .get(&(parent, target))
            .into_iter()
            .flat_map(|kinds| kinds.iter())
            .filter(|kind| !matches!(kind, RefKind::Declares | RefKind::Holds))
            .map(|kind| format!(", {}", kind.word()))
            .collect()
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
                parts.push(format!(
                    "{symbols} of {} symbols",
                    total_symbols_text(graph)
                ));
            } else if graph.incomplete {
                parts.push(format!("{symbols} symbols (incomplete)"));
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
        self.sort_key_at(node, self.shown(node, self.own_lead.get(&node.id())))
    }

    fn sort_key_at(&self, node: &Node, shown: Option<Shown<'_>>) -> (String, usize, String) {
        match shown.map(|s| s.inst) {
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

    /// Owned ordering key for an edge target (looked up by id), as it is
    /// shown under `parent`.
    fn target_sort_key(&self, parent: SymbolId, target: SymbolId) -> (String, usize, String) {
        match self.nodes_by_id.get(&target) {
            Some(node) => self.sort_key_at(
                node,
                self.shown(node, self.pair_lead.get(&(parent, target))),
            ),
            None => (String::new(), 0, format!("#{}", target.0)),
        }
    }

    fn is_content(&self, node: &Node) -> bool {
        node.instances()
            .first()
            .map(|i| i.symbol_type == SymbolType::Content)
            .unwrap_or(false)
    }

    /// A SYMBOL IS SHOWN AT THE DEFINITION NEAREST TO WHOEVER LED TO IT, AND
    /// SAYS HOW MANY THERE ARE.
    ///
    /// One name is one symbol, however many files define it: the kernel has
    /// 47 `DEVICE_NAME`s and 9,714 `init_module`s.  A report line has room
    /// for one place.  It used to be the first definition in the list, so a
    /// driver "referred to" another driver's macro, at a different place on
    /// the next run.
    ///
    /// What led here says which one is meant:
    ///   - a containment edge names the instance it holds, exactly;
    ///   - a reference is written in some files, and the definition in one of
    ///     them is the one in sight there; failing that, the definition that
    ///     shares the most directories with them (a driver's own header
    ///     before a namesake across the tree).
    ///
    /// Nearest is a guess where no file matches -- which definition a
    /// reference resolves to is not in the index -- so a choice among several
    /// is always labelled as one.  With nothing to go by, the first by path:
    /// the same every time.
    ///
    /// A definition is preferred to a declaration and a declaration to
    /// anything else, as before.
    fn shown<'n>(&self, node: &'n Node, lead: Option<&Lead<'_>>) -> Option<Shown<'n>> {
        let insts = node.instances();
        let path =
            |i: &NodeSymbolInstance| self.paths.get(i.object_id.as_str()).copied().unwrap_or("");
        let order = |i: &&'n NodeSymbolInstance| (path(i), i.start_offset, i.id.as_str());

        if let Some(lead) = lead {
            let held = insts
                .iter()
                .filter(|i| lead.instances.contains(&i.id.as_str()))
                .min_by_key(order);
            if let Some(inst) = held {
                return Some(Shown { inst, others: None });
            }
        }

        let class = [InstanceType::Definition, InstanceType::Declaration]
            .into_iter()
            .find(|class| insts.iter().any(|i| i.instance_type == *class));
        let candidates: Vec<&'n NodeSymbolInstance> = insts
            .iter()
            .filter(|i| class.is_none_or(|class| i.instance_type == class))
            .collect();

        let files: &[&str] = lead.map(|l| l.objects.as_slice()).unwrap_or(&[]);
        let file_paths: Vec<&str> = files
            .iter()
            .filter_map(|object| self.paths.get(object).copied())
            .collect();
        let distance = |i: &NodeSymbolInstance| {
            if files.contains(&i.object_id.as_str()) {
                return 0;
            }
            let shared = file_paths
                .iter()
                .map(|file| shared_dirs(path(i), file))
                .max()
                .unwrap_or(0);
            usize::MAX - shared
        };
        let inst = candidates
            .iter()
            .copied()
            .min_by_key(|i| (distance(i), order(i)))?;

        let others = match class {
            Some(class) if candidates.len() > 1 => {
                let what = match class {
                    InstanceType::Definition => "definitions",
                    _ => "declarations",
                };
                // A list the instance cap cut is not a count of them.
                let at_least = if node.instances_truncated() {
                    "at least "
                } else {
                    ""
                };
                Some(format!("(1 of {at_least}{} {what})", candidates.len()))
            }
            _ => None,
        };
        Some(Shown { inst, others })
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
        let Some(Shown { inst, others }) = self.shown(node, self.own_lead.get(&node.id())) else {
            return node.label().to_string();
        };
        let mut line = format!(
            "{}  ({})  {}",
            node.label(),
            type_abbr(inst.symbol_type),
            self.location(inst),
        );
        if let Some(others) = others {
            line.push_str(&format!("  {others}"));
        }
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
            parts = self
                .shown(node, self.own_lead.get(&node.id()))
                .map(|shown| shown.inst)
                .into_iter()
                .collect();
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

    fn child_line(&self, parent: SymbolId, target: SymbolId, marks: Marks) -> String {
        match self.nodes_by_id.get(&target) {
            Some(node) => {
                let (ty, loc) = match self.shown(node, self.pair_lead.get(&(parent, target))) {
                    Some(Shown { inst, others }) => {
                        let mut loc = self.location(inst);
                        if let Some(others) = others {
                            loc.push_str(&format!("  {others}"));
                        }
                        (type_abbr(inst.symbol_type), loc)
                    }
                    None => ("?", String::new()),
                };
                let said = self.said(parent, target);
                format!(
                    "  {} {}  ({}{})  {}",
                    marks.drawn(),
                    node.label(),
                    ty,
                    said,
                    loc
                )
                .trim_end()
                .to_string()
            }
            None => format!("  {} #{}", marks.drawn(), target.0),
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

/// How many symbols the query found, as far as anyone knows.  A count that a
/// limit cut is a lower bound, and is never printed as a total: "101 of 930"
/// read as "930 exist" when 930 was where the reading stopped.
fn total_symbols_text(graph: &Graph) -> String {
    if graph.incomplete {
        format!("at least {}", graph.total_symbols)
    } else {
        graph.total_symbols.to_string()
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
            ". The name exists but wasn't matched here — check `project(...)` and the \
             scope it is asked in.",
        );
    }
    out
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
    use index::roles::RefKind;
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
            line: None,
        }
    }

    fn node(id: i64, label: &str, insts: Vec<NodeSymbolInstance>) -> Node {
        let total = insts.len();
        Node::new(
            SymbolId(id),
            label.into(),
            insts,
            Vec::<QueryStatement>::new(),
            false,
            total,
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
            1,
            Vec::new(),
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

    /// THE REPORT SAYS WHAT ITS MARKS MEAN -- the ones it uses, once, before
    /// the first of them.  A reader met `→` and `▸` under one symbol with
    /// nothing to tell a call from a member.
    #[test]
    fn the_report_explains_the_marks_it_uses() {
        let (src, start, end) = body_source();
        let function = |id: i64, label: &str| {
            node(
                id,
                label,
                vec![inst(
                    "10",
                    start,
                    end,
                    SymbolType::Function,
                    InstanceType::Definition,
                )],
            )
        };

        // Nothing drawn, nothing to explain.
        let mut alone = Graph::new();
        obj(&mut alone, "10", "/a.c");
        alone.add_node(function(1, "f"));
        let md = render_markdown("\"f\"", &alone, &src, Projection::Names);
        assert!(
            !md.contains("refers to") && !md.contains("contains"),
            "{md}"
        );

        // A reference only: `▸` is not in the report, so not in its legend.
        let mut refs = Graph::new();
        obj(&mut refs, "10", "/a.c");
        refs.add_node(function(1, "f"));
        refs.add_node(function(2, "g"));
        refs.add_edge(crate::api::types::Edge::new(
            SymbolId(1),
            SymbolId(2),
            1,
            Vec::new(),
        ));
        let md = render_markdown("\"f\" { }", &refs, &src, Projection::Names);
        assert!(md.contains("# Results\n`→` refers to\n"), "{md}");

        // Both kinds, each from its own pair.
        refs.add_node(function(3, "h"));
        refs.add_has_edge(crate::api::types::HasEdge::new(
            SymbolId(1),
            SymbolId(3),
            index::symbols::SymbolInstanceId::new(1),
            index::symbols::SymbolInstanceId::new(3),
        ));
        let md = render_markdown("\"f\" { }", &refs, &src, Projection::Names);
        assert!(
            md.contains("# Results\n`▸` contains, `→` refers to\n"),
            "{md}"
        );
    }

    /// A PAIR IS DRAWN ONCE, WITH EVERY RELATIONSHIP IT HAS.
    ///
    /// A file contains its functions, and it holds the references to them
    /// too -- every call between two of its functions is text of the file.
    /// So the pair (file, function) arrives twice, as containment and as a
    /// reference.  `file("bt-bmc.c") has { func }` said "27 contains" and
    /// printed 25 of them as `→`; then containment won and the 25 were drawn
    /// `▸`, which hid that the file refers to them.  Both are true of the
    /// pair, and the report does not choose: `▸,→`.
    #[test]
    fn a_pair_that_contains_and_refers_is_drawn_with_both_marks() {
        let (src, start, end) = body_source();
        let function = |id: i64, label: &str| {
            node(
                id,
                label,
                vec![inst(
                    "10",
                    start,
                    end,
                    SymbolType::Function,
                    InstanceType::Definition,
                )],
            )
        };
        let mut g = Graph::new();
        obj(&mut g, "10", "/a.c");
        g.add_node(function(1, "outer"));
        g.add_node(function(2, "member"));
        g.add_edge(crate::api::types::Edge::new(
            SymbolId(1),
            SymbolId(2),
            1,
            Vec::new(),
        ));
        g.add_has_edge(crate::api::types::HasEdge::new(
            SymbolId(1),
            SymbolId(2),
            index::symbols::SymbolInstanceId::new(1),
            index::symbols::SymbolInstanceId::new(2),
        ));
        let md = render_markdown("\"outer\" { }", &g, &src, Projection::Names);
        assert!(md.contains("  ▸,→ member"), "{md}");
        // Drawn once, and the legend names both marks the line carries.
        assert_eq!(md.matches("member").count(), 1, "{md}");
        assert!(
            md.contains("# Results\n`▸` contains, `→` refers to\n"),
            "{md}"
        );
    }

    /// A graph of one object, built for the tests of what an edge says.
    struct Said {
        graph: Graph,
        source: SourceMap,
    }

    impl Said {
        fn new() -> Self {
            let mut graph = Graph::new();
            obj(&mut graph, "10", "/a.c");
            let mut source = SourceMap::new();
            source.insert("10".into(), b"x\n".to_vec());
            Said { graph, source }
        }

        fn symbol(mut self, id: i64, label: &str, ty: SymbolType) -> Self {
            self.graph.add_node(node(
                id,
                label,
                vec![inst("10", 0, 1, ty, InstanceType::Definition)],
            ));
            self
        }

        fn edge(mut self, from: i64, to: i64, kinds: Vec<RefKind>) -> Self {
            self.graph.add_edge(
                crate::api::types::Edge::new(SymbolId(from), SymbolId(to), 1, Vec::new())
                    .with_kinds(kinds),
            );
            self
        }

        /// An edge whose every row is a relation: nothing refers by it.
        fn relation(mut self, from: i64, to: i64, kinds: Vec<RefKind>) -> Self {
            self.graph.add_edge(
                crate::api::types::Edge::new(SymbolId(from), SymbolId(to), 1, Vec::new())
                    .with_kinds(kinds)
                    .with_refers(false),
            );
            self
        }

        fn holding(mut self, parent: i64, child: i64) -> Self {
            self.graph.add_has_edge(crate::api::types::HasEdge::new(
                SymbolId(parent),
                SymbolId(child),
                index::symbols::SymbolInstanceId::new(parent),
                index::symbols::SymbolInstanceId::new(child),
            ));
            self
        }

        fn report(&self) -> String {
            render_markdown("q", &self.graph, &self.source, Projection::Names)
        }
    }

    /// AN ARROW SAYS WHAT THE EDGE DOES, after the type, in the engine's own
    /// words and order.  `bt_bmc_config_irq → bt_bmc_irq` was drawn like a
    /// call; the source passes it as an interrupt handler.
    #[test]
    fn an_arrow_says_what_the_edge_does() {
        use RefKind::*;
        let said = Said::new()
            .symbol(1, "caller", SymbolType::Function)
            .symbol(2, "callee", SymbolType::Function)
            .symbol(3, "handler", SymbolType::Function)
            .symbol(4, "counter", SymbolType::Data)
            .symbol(5, "ops.read", SymbolType::Field)
            .symbol(6, "plain", SymbolType::Function)
            .edge(1, 2, vec![Calls])
            .edge(1, 3, vec![Takes])
            .edge(1, 4, vec![Writes, Reads])
            .edge(1, 5, vec![Calls, Reads])
            .edge(1, 6, vec![]);
        let md = said.report();
        assert!(md.contains("  → callee  (func, calls)  /a.c:1"), "{md}");
        assert!(md.contains("  → handler  (func, takes)  /a.c:1"), "{md}");
        assert!(
            md.contains("  → counter  (data, writes, reads)  /a.c:1"),
            "{md}"
        );
        assert!(
            md.contains("  → ops.read  (field, calls, reads)  /a.c:1"),
            "{md}"
        );
        // Nothing stated, nothing added: the line an older index prints.
        assert!(md.contains("  → plain  (func)  /a.c:1"), "{md}");
        // Words do not change the mark, so the legend is the one it was.
        assert!(md.contains("# Results\n`→` refers to\n"), "{md}");
    }

    /// A RELATION HAS ITS OWN MARK.  "The macro declares it" and "the slot
    /// holds it" are not things a token says where the arrow starts, so a
    /// pair that has only that is not drawn as a reference: `↦` declares, `⇢`
    /// holds.  The mark is how a relationship is said; the words after the
    /// type are what the references do.
    #[test]
    fn a_relation_has_its_own_mark() {
        use RefKind::*;
        let declares = Said::new()
            .symbol(1, "DEFINE_MUTEX", SymbolType::Macro)
            .symbol(2, "lock", SymbolType::Data)
            .relation(1, 2, vec![Declares]);
        let md = declares.report();
        assert!(md.contains("  ↦ lock  (data)  /a.c:1"), "{md}");
        assert!(md.contains("# Results\n`↦` declares\n"), "{md}");
        assert!(!md.contains("→") && !md.contains("⇢"), "{md}");

        let holds = Said::new()
            .symbol(1, "ops.read", SymbolType::Field)
            .symbol(2, "my_read", SymbolType::Function)
            .relation(1, 2, vec![Holds]);
        let md = holds.report();
        assert!(md.contains("  ⇢ my_read  (func)  /a.c:1"), "{md}");
        assert!(md.contains("# Results\n`⇢` holds\n"), "{md}");
        assert!(!md.contains("→") && !md.contains("↦"), "{md}");
    }

    /// The legend names the marks drawn, each once, in the order they are
    /// written on a line.
    #[test]
    fn the_legend_names_every_mark_drawn_and_no_other() {
        use RefKind::*;
        let all = Said::new()
            .symbol(1, "a", SymbolType::Function)
            .symbol(2, "b", SymbolType::Function)
            .symbol(3, "c", SymbolType::Function)
            .symbol(4, "d", SymbolType::Function)
            .symbol(5, "e", SymbolType::Function)
            .edge(1, 2, vec![Calls])
            .holding(1, 3)
            .relation(1, 4, vec![Declares])
            .relation(1, 5, vec![Holds]);
        let md = all.report();
        assert!(
            md.contains("# Results\n`▸` contains, `→` refers to, `↦` declares, `⇢` holds\n"),
            "{md}"
        );
        // Under one symbol: what it refers to, declares, holds, contains.
        let marks: Vec<&str> = md
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(marks, vec!["→", "↦", "⇢", "▸"], "{md}");

        // A mark that only ever appears beside another is in the legend too.
        let md = Said::new()
            .symbol(1, "file", SymbolType::File)
            .symbol(2, "member", SymbolType::Function)
            .edge(1, 2, vec![])
            .holding(1, 2)
            .report();
        assert!(
            md.contains("# Results\n`▸` contains, `→` refers to\n"),
            "{md}"
        );
    }

    /// EVERY RELATIONSHIP OF A PAIR IS DRAWN, as one mark each, in one order:
    /// contains, refers to, declares, holds.  The report does not pick the
    /// one that matters.  The words after the type are what the pair's
    /// references do, and a mark never takes one away.
    #[test]
    fn a_pair_is_drawn_with_every_relationship_it_has() {
        use RefKind::*;
        let md = Said::new()
            .symbol(1, "file", SymbolType::File)
            .symbol(2, "held_only", SymbolType::Function)
            .symbol(3, "used_only", SymbolType::Function)
            .symbol(4, "both", SymbolType::Function)
            .holding(1, 2)
            .edge(1, 3, vec![])
            .edge(1, 4, vec![Calls])
            .holding(1, 4)
            .report();
        assert!(md.contains("  ▸ held_only  (func)  /a.c:1"), "{md}");
        assert!(md.contains("  → used_only  (func)  /a.c:1"), "{md}");
        assert!(md.contains("  ▸,→ both  (func, calls)  /a.c:1"), "{md}");
        // What it refers to first, then what it also contains, then what it
        // only contains.
        let lines: Vec<&str> = md
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().nth(1))
            .collect();
        assert_eq!(lines, vec!["used_only", "both", "held_only"], "{md}");

        // A relation beside an ordinary reference: both marks, and the
        // reference's words.  The relation is said by its mark, once.
        let md = Said::new()
            .symbol(1, "ops.read", SymbolType::Field)
            .symbol(2, "my_read", SymbolType::Function)
            .edge(1, 2, vec![Takes, Holds])
            .report();
        assert!(md.contains("  →,⇢ my_read  (func, takes)  /a.c:1"), "{md}");
        assert!(md.contains("# Results\n`→` refers to, `⇢` holds\n"), "{md}");
        assert!(!md.contains("holds)"), "the mark says it:\n{md}");

        // Both relations and nothing else.
        let md = Said::new()
            .symbol(1, "m", SymbolType::Macro)
            .symbol(2, "f", SymbolType::Function)
            .relation(1, 2, vec![Declares, Holds])
            .report();
        assert!(md.contains("  ↦,⇢ f  (func)  /a.c:1"), "{md}");
        assert!(md.contains("# Results\n`↦` declares, `⇢` holds\n"), "{md}");

        // All four.
        let md = Said::new()
            .symbol(1, "outer", SymbolType::Macro)
            .symbol(2, "inner", SymbolType::Data)
            .edge(1, 2, vec![Writes, Reads, Declares, Holds])
            .holding(1, 2)
            .report();
        assert!(
            md.contains("  ▸,→,↦,⇢ inner  (data, writes, reads)  /a.c:1"),
            "{md}"
        );
        assert!(
            md.contains("# Results\n`▸` contains, `→` refers to, `↦` declares, `⇢` holds\n"),
            "{md}"
        );

        // An edge that says nothing at all is still a reference.
        let md = Said::new()
            .symbol(1, "a", SymbolType::Function)
            .symbol(2, "b", SymbolType::Function)
            .relation(1, 2, vec![])
            .report();
        assert!(md.contains("  → b  (func)  /a.c:1"), "{md}");
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

        // The same answer when a limit cut the reading as well: 42 is how
        // far it got, not how many there are.
        g.incomplete = true;
        let md = render_markdown("\"f\"", &g, &src, Projection::Names);
        assert!(md.contains("1 of at least 42 symbols"), "{md}");
        assert!(md.contains("truncated to 1 of at least 42 symbols"), "{md}");

        // Cut while reading, and everything read is shown.
        g.truncated = false;
        let md = render_markdown("\"f\"", &g, &src, Projection::Names);
        assert!(md.contains("1 symbols (incomplete)"), "{md}");
    }

    #[test]
    fn a_warning_said_twice_is_printed_once() {
        let mut g = Graph::new();
        for _ in 0..2 {
            g.warnings
                .push(warning("too many results in `{ }`", 1, None, &[]));
        }
        g.warnings
            .push(warning("too many results in `func`", 1, None, &[]));
        let md = render_markdown("{ }", &g, &SourceMap::new(), Projection::Names);
        assert_eq!(md.matches("too many results in `{ }`").count(), 1, "{md}");
        assert_eq!(md.matches("too many results in `func`").count(), 1, "{md}");
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
        assert!(bullet.contains("project(...)"), "{bullet}");
        assert!(!bullet.contains("Did you mean"), "{bullet}");
    }

    #[test]
    fn warning_bullet_offers_the_spelling_that_matches() {
        // The name exists as data: the suggestion is the selector to type.
        let mut w = warning(
            "`func(\"module_mutex\")` matched no symbols",
            1,
            Some("module_mutex"),
            &["data(\"module_mutex\")"],
        );
        w.name_exists = true;
        let bullet = warning_bullet(&w);
        assert!(
            bullet.ends_with("Did you mean: `data(\"module_mutex\")`?"),
            "{bullet}"
        );
    }

    /// The child line printed under `parent` for `child`.
    fn child_line_of<'m>(md: &'m str, parent: &str, child: &str) -> &'m str {
        let mut lines = md.lines().skip_while(|l| !l.starts_with(parent));
        lines
            .next()
            .unwrap_or_else(|| panic!("no `{parent}` in:\n{md}"));
        lines
            .take_while(|l| l.starts_with("  "))
            .find(|l| l.contains(child))
            .unwrap_or_else(|| panic!("no `{child}` under `{parent}` in:\n{md}"))
    }

    /// A SYMBOL IS SHOWN AT THE DEFINITION NEAREST TO WHOEVER LED TO IT, AND
    /// SAYS HOW MANY THERE ARE.
    ///
    /// C has one `DEVICE_NAME` per driver and the index has one symbol for
    /// the name, with 47 definitions.  The report printed whichever came
    /// first, so `bt_bmc_probe` in `bt-bmc.c` "referred to" the
    /// `DEVICE_NAME` of a USB sound card, at another place on the next run,
    /// and nothing said there was more than one.
    ///
    /// The edge knows which files its references are written in.  The
    /// definition in one of those files is the one meant; failing that, the
    /// one that shares the most directories with them.
    #[test]
    fn a_symbol_is_shown_at_the_definition_nearest_to_whoever_led_to_it() {
        use crate::api::types::Edge;
        let src = SourceMap::new();
        let def = |object: &str, ty| inst(object, 0, 1, ty, InstanceType::Definition);
        let (func, mac) = (SymbolType::Function, SymbolType::Macro);

        let mut g = Graph::new();
        obj(&mut g, "10", "/k/drivers/char/ipmi/bt-bmc.c");
        obj(&mut g, "11", "/k/drivers/hid/hid-udraw-ps3.c");
        obj(&mut g, "12", "/k/sound/usb/card.c");
        obj(&mut g, "13", "/k/drivers/char/ipmi/ipmi_si.h");
        obj(&mut g, "14", "/k/drivers/char/ipmi/kcs_bmc.c");
        g.add_node(node(1, "bt_bmc_probe", vec![def("10", func)]));
        g.add_node(node(
            2,
            "DEVICE_NAME",
            vec![def("12", mac), def("11", mac), def("10", mac)],
        ));
        g.add_node(node(3, "snd_probe", vec![def("12", func)]));
        g.add_node(node(4, "kcs_probe", vec![def("14", func)]));
        g.add_node(node(5, "SI_NAME", vec![def("12", mac), def("13", mac)]));
        g.add_edge(Edge::new(SymbolId(1), SymbolId(2), 1, vec!["10".into()]));
        g.add_edge(Edge::new(SymbolId(3), SymbolId(2), 1, vec!["12".into()]));
        g.add_edge(Edge::new(SymbolId(4), SymbolId(5), 1, vec!["14".into()]));
        let md = render_markdown("q", &g, &src, Projection::Names);

        // In the file that refers to it -- a different one under each parent.
        assert_eq!(
            child_line_of(&md, "bt_bmc_probe", "DEVICE_NAME"),
            "  → DEVICE_NAME  (macro)  /k/drivers/char/ipmi/bt-bmc.c  (1 of 3 definitions)",
            "{md}"
        );
        assert_eq!(
            child_line_of(&md, "snd_probe", "DEVICE_NAME"),
            "  → DEVICE_NAME  (macro)  /k/sound/usb/card.c  (1 of 3 definitions)",
            "{md}"
        );
        // In no such file: the header beside it, not the one across the tree.
        assert_eq!(
            child_line_of(&md, "kcs_probe", "SI_NAME"),
            "  → SI_NAME  (macro)  /k/drivers/char/ipmi/ipmi_si.h  (1 of 2 definitions)",
            "{md}"
        );
        // One definition is the definition: nothing to add.
        assert!(
            md.contains("bt_bmc_probe  (func)  /k/drivers/char/ipmi/bt-bmc.c\n"),
            "{md}"
        );
    }

    /// Nobody led to a symbol that was asked for by name.  Its definitions
    /// are all the answer; the first by path is shown, the same one every
    /// time, and the count says there are others.
    #[test]
    fn a_symbol_nobody_led_to_is_shown_at_its_first_definition() {
        let src = SourceMap::new();
        let def = |object: &str| inst(object, 0, 1, SymbolType::Macro, InstanceType::Definition);
        let mut g = Graph::new();
        obj(&mut g, "10", "/k/drivers/char/ipmi/bt-bmc.c");
        obj(&mut g, "11", "/k/drivers/hid/hid-udraw-ps3.c");
        obj(&mut g, "12", "/k/sound/usb/card.c");
        g.add_node(node(
            2,
            "DEVICE_NAME",
            vec![def("12"), def("11"), def("10")],
        ));
        let md = render_markdown("q", &g, &src, Projection::Names);
        assert!(
            md.contains(
                "DEVICE_NAME  (macro)  /k/drivers/char/ipmi/bt-bmc.c  (1 of 3 definitions)\n"
            ),
            "{md}"
        );

        // A list the instance cap cut is not a count of the definitions.
        let mut cut = Graph::new();
        obj(&mut cut, "10", "/k/drivers/char/ipmi/bt-bmc.c");
        obj(&mut cut, "11", "/k/drivers/hid/hid-udraw-ps3.c");
        cut.add_node(Node::new(
            SymbolId(2),
            "pr_fmt".into(),
            vec![def("11"), def("10")],
            Vec::<QueryStatement>::new(),
            true,
            1877,
        ));
        let md = render_markdown("q", &cut, &src, Projection::Names);
        assert!(md.contains("(1 of at least 2 definitions)"), "{md}");
    }

    /// Containment names the instance it holds, so there is nothing to
    /// choose: every module's `init_module` is one symbol, and the one a file
    /// contains is the one in that file.
    #[test]
    fn a_contained_symbol_is_shown_at_the_instance_that_is_contained() {
        use crate::api::types::HasEdge;
        use index::symbols::SymbolInstanceId;
        let src = SourceMap::new();
        let decl = |id: &str, object: &str| NodeSymbolInstance {
            id: id.into(),
            ..inst(
                object,
                0,
                1,
                SymbolType::Function,
                InstanceType::Declaration,
            )
        };
        let mut g = Graph::new();
        obj(&mut g, "10", "/k/drivers/char/ipmi/bt-bmc.c");
        obj(&mut g, "11", "/k/drivers/clk/clk-a.c");
        g.add_node(node(
            1,
            "/k/drivers/char/ipmi/bt-bmc.c",
            vec![NodeSymbolInstance {
                id: "100".into(),
                ..inst("10", 0, 1, SymbolType::File, InstanceType::Definition)
            }],
        ));
        g.add_node(node(
            2,
            "init_module",
            vec![decl("201", "11"), decl("202", "10")],
        ));
        g.add_has_edge(HasEdge::new(
            SymbolId(1),
            SymbolId(2),
            SymbolInstanceId::new(100),
            SymbolInstanceId::new(202),
        ));
        let md = render_markdown("q", &g, &src, Projection::Names);
        assert_eq!(
            child_line_of(&md, "/k/drivers/char/ipmi/bt-bmc.c", "init_module"),
            "  ▸ init_module  (func)  /k/drivers/char/ipmi/bt-bmc.c",
            "{md}"
        );
    }

    #[test]
    fn warning_bullet_without_suggestions_omits_did_you_mean() {
        let w = warning("something advisory", 3, None, &[]);
        let bullet = warning_bullet(&w);
        assert_eq!(bullet, "something advisory (line 3)");
    }
}
