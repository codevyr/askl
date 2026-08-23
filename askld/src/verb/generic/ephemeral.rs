use crate::cfg::ControlFlowGraph;
use crate::parser::Value;
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::verb::Args;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    EphContext, EphInstanceRow, EphRefRow, EphSymbolRow, LayerBatch, INSTANCE_TYPE_DEFINITION,
    INSTANCE_TYPE_DOCUMENTATION, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FUNCTION,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};

use super::super::{AnchorKind, DeriveMethod, Selector, Verb, VerbClass};

/// Resolved selections for labels referenced by ephemeral verbs.  An
/// ephemeral verb that takes `symbol="@foo"` looks up the symbol IDs of
/// the statement labelled `@foo` here at hash/materialise time.
///
/// Populated by `compute_roots` immediately before pushing a layer-creating
/// statement's compute future: the prior labelled statement's selection is
/// guaranteed-applied by then (via the User dependency edge installed at
/// parse time).  Empty for statements with no `@label` references.
///
/// Keys are `Rc<str>` — the same shape used by
/// `DependencyRole::PreSeedLabel` upstream, so inserts move the label
/// in without re-allocating.
#[derive(Debug, Clone, Default)]
pub struct LabelResolutions {
    /// label name → symbol IDs from the labelled statement's selection.
    map: HashMap<std::rc::Rc<str>, Vec<i64>>,
}

impl LabelResolutions {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, label: std::rc::Rc<str>, symbol_ids: Vec<i64>) {
        self.map.insert(label, symbol_ids);
    }

    /// Look up the resolved symbol IDs for `label`.  Returns an empty
    /// slice if the label is missing — an ephemeral op should treat
    /// that as "no rows to emit" rather than panicking.
    pub fn get(&self, label: &str) -> &[i64] {
        self.map.get(label).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Trait for ephemeral operations that can be batched into a layer block.
///
/// Ephemeral verbs are only available inside `layer { }` blocks — this trait
/// is the only interface through which they execute.
pub(crate) trait EphemeralOp: std::fmt::Debug + Send + Sync {
    /// Contribute this operation's parameters to a combined hash.  Ops that
    /// reference `@label` arguments hash the *resolved* IDs (not the label
    /// string) so the cache key reflects the actual rows the op will emit.
    fn hash_params(&self, h: &mut Sha256, resolved: &LabelResolutions);
    /// Collect this operation's rows into the batch for bulk insert.
    fn collect_rows(&self, batch: &mut LayerBatch, resolved: &LabelResolutions);
    /// Labels referenced by this op's arguments, if any.  Used by
    /// `build_dependency_graph` to add User edges so the labelled statement
    /// runs before this op's enclosing layer materialises.  Default empty.
    fn label_refs(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Whether any row in the batch references an eph (negative) id.  This is
/// the root/selection shard classifier for `layer { … }` blocks: it inspects
/// the
/// rows an op actually emits, so classification is by resolved *value* (a
/// literal negative id counts the same as a label that resolved to eph
/// rows) and lives in exactly one place — an op cannot opt out by omission.
/// When a new id-bearing field is added to a row type, extend this check.
fn batch_references_eph(batch: &LayerBatch) -> bool {
    batch.instances.iter().any(|r| r.symbol_id < 0) || batch.refs.iter().any(|r| r.to_symbol < 0)
}

/// Append `src`'s rows onto `dst`, preserving order within each row kind.
fn merge_batch(dst: &mut LayerBatch, src: LayerBatch) {
    dst.symbols.extend(src.symbols);
    dst.instances.extend(src.instances);
    dst.refs.extend(src.refs);
}

/// Shared, mutable collection of ephemeral operations for a `layer { … }`
/// block.
///
/// # Why `Arc<Mutex<...>>` rather than `Arc<RefCell<...>>`?
///
/// The [`Verb`] trait is bound by `Send + Sync` (see
/// `askld/src/verb/mod.rs`), so every `dyn Verb` — including
/// [`LayerVerb`] which holds an `EphemeralOps` — must be `Sync`.
/// `RefCell` is `Send` but **not** `Sync`, so a `RefCell` here would
/// stop `LayerVerb` from being usable as a `Verb`.  The `Sync` bound
/// itself is load-bearing for actix-web's multi-worker dispatch of
/// `ControlFlowGraph` (which carries the parsed verbs).
///
/// In practice the lock is never contended:
///
/// 1. Writes happen single-threaded during parsing
///    (`build_generic_verb` at `verb/generic/mod.rs:142` pushes
///    ephemeral ops into the vec).
/// 2. Reads happen single-threaded during execution
///    ([`LayerVerb::layer_spec`] hashes the ops and collects the
///    batch).
/// 3. The two phases never overlap — execution begins only after the
///    whole AST is built — and each `LayerVerb` instance is touched by
///    exactly one execution future.
///
/// So each `.lock().unwrap()` is essentially an uncontested atomic.
/// The lock is here for the trait bound, not for mutual exclusion.
pub(crate) type EphemeralOps = Arc<Mutex<Vec<Arc<dyn EphemeralOp>>>>;

/// EphemeralSymbolVerb - creates an ephemeral symbol row in the DB.
///
/// Only available inside `layer { }` blocks.
/// Usage: ephemeral_symbol(name="sym", project_id=1, symbol_type=1)
#[derive(Debug)]
pub(in crate::verb) struct EphemeralSymbolVerb {
    name: String,
    project_id: i32,
    symbol_type: i32,
    scope: Option<i32>,
}

impl EphemeralSymbolVerb {
    pub(in crate::verb) const NAME: &'static str = "ephemeral_symbol";

    fn create(args: &Args) -> Result<Self> {
        args.allow(&["name", "project_id", "symbol_type", "scope"])?;
        let name = args.req_str("name")?.to_string();
        let project_id = args.req_i32("project_id")?;
        let symbol_type = args.req_i32("symbol_type")?;
        if !(SYMBOL_TYPE_FUNCTION..=SYMBOL_TYPE_FIELD).contains(&symbol_type) {
            bail!(
                "symbol_type must be between {} and {} (got {})",
                SYMBOL_TYPE_FUNCTION,
                SYMBOL_TYPE_FIELD,
                symbol_type
            );
        }
        let scope = args.named_i32("scope")?;

        Ok(Self {
            name,
            project_id,
            symbol_type,
            scope,
        })
    }

    pub(crate) fn new_op(_span: Span, args: &Args) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(args)?))
    }
}

impl EphemeralOp for EphemeralSymbolVerb {
    fn hash_params(&self, h: &mut Sha256, _resolved: &LabelResolutions) {
        h.update(b"ephemeral_symbol");
        h.update((self.name.len() as u64).to_le_bytes());
        h.update(self.name.as_bytes());
        h.update(self.project_id.to_le_bytes());
        h.update(self.symbol_type.to_le_bytes());
        match self.scope {
            Some(s) => {
                h.update([1u8]);
                h.update(s.to_le_bytes());
            }
            None => {
                h.update([0u8]);
            }
        }
    }

    fn collect_rows(&self, batch: &mut LayerBatch, _resolved: &LabelResolutions) {
        let (path, leaf_name) = symbol_path_and_leaf(&self.name, self.symbol_type);
        batch.symbols.push(EphSymbolRow {
            name: self.name.clone(),
            path,
            project_id: self.project_id,
            symbol_type: self.symbol_type,
            scope: self.scope,
            leaf_name,
        });
    }
}

/// Symbol-ID input for an ephemeral verb.  Resolves to a *set* of
/// symbol IDs at materialise time; the verb emits one row per
/// resolved symbol.
///
/// **Multi-row semantics by default.**  An ephemeral verb takes a
/// `SymbolRef` and emits N rows, where N is the resolved set's size:
///   - `symbol_id=1` → `Literal(1)` → resolves to one symbol → one row.
///   - `symbol_id="@foo"` → `Label("foo")` → resolves to whatever the
///     `@foo` statement selected → N rows.
///   - If a label resolves to zero symbols (statement matched nothing),
///     zero rows are emitted and a `tracing::warn!` is logged from
///     `compute_roots` so the operator can tell the layer was a no-op.
///
/// Authors writing queries should think in N-row terms: a literal is
/// just the degenerate `N=1` case.
#[derive(Debug)]
enum SymbolRef {
    Literal(i64),
    Label(String),
}

impl SymbolRef {
    /// `123` → `Literal(123)`, `"@foo"` → `Label("foo")`.  The argument's
    /// TYPE does the discriminating: an integer is an id, a string is a label
    /// reference.  Before arguments were typed both arrived as strings and
    /// the leading `@` had to be sniffed off.
    fn from_value(value: &Value, key: &str) -> Result<Self> {
        match value {
            Value::Int(id) => Ok(SymbolRef::Literal(*id)),
            Value::Str { text, .. } => match text.strip_prefix('@') {
                Some("") => bail!("'{}' label reference must not be empty", key),
                Some(label) => Ok(SymbolRef::Label(label.to_string())),
                None => bail!(
                    "'{}' expects a symbol id or a label reference, found the \
                     string {:?} — write an id unquoted ({}={}), or reference a \
                     label as \"@name\"",
                    key,
                    text,
                    key,
                    text,
                ),
            },
            other => bail!(
                "'{}' expects a symbol id or a label reference, found {}",
                key,
                other.describe()
            ),
        }
    }

    /// Resolve to the set of symbol IDs the verb should materialise.
    /// Always returns 0..N elements; `Literal` is the `N=1` case.
    fn resolve_vec(&self, resolved: &LabelResolutions) -> Vec<i64> {
        match self {
            SymbolRef::Literal(id) => vec![*id],
            SymbolRef::Label(label) => resolved.get(label).to_vec(),
        }
    }

    fn label(&self) -> Option<&str> {
        match self {
            SymbolRef::Literal(_) => None,
            SymbolRef::Label(l) => Some(l.as_str()),
        }
    }
}

/// EphemeralInstanceVerb - creates ephemeral instance rows.
///
/// Only available inside `layer { }` blocks.  Emits one row per
/// resolved symbol — see [`SymbolRef`] for the multi-row semantic.
///
/// Usage: `ephemeral_instance(symbol_id=<id>, object_id=1,
///        start=0, end=10, instance_type=1)` for a literal
///        (single-row) input, or `symbol_id="@label"` for a
///        label-resolved (`N`-row) input.
#[derive(Debug)]
pub(in crate::verb) struct EphemeralInstanceVerb {
    symbol: SymbolRef,
    object_id: i32,
    start: i64,
    end: i64,
    instance_type: i32,
}

impl EphemeralInstanceVerb {
    pub(in crate::verb) const NAME: &'static str = "ephemeral_instance";

    fn create(args: &Args) -> Result<Self> {
        args.allow(&["symbol_id", "object_id", "start", "end", "instance_type"])?;
        let symbol = SymbolRef::from_value(args.required_named("symbol_id")?, "symbol_id")?;
        let object_id = args.req_i32("object_id")?;
        let start = args.req_i64("start")?;
        let end = args.req_i64("end")?;
        let instance_type = args.req_i32("instance_type")?;
        if !(INSTANCE_TYPE_DEFINITION..=INSTANCE_TYPE_DOCUMENTATION).contains(&instance_type) {
            bail!(
                "instance_type must be between {} and {} (got {})",
                INSTANCE_TYPE_DEFINITION,
                INSTANCE_TYPE_DOCUMENTATION,
                instance_type
            );
        }

        Ok(Self {
            symbol,
            object_id,
            start,
            end,
            instance_type,
        })
    }

    pub(crate) fn new_op(_span: Span, args: &Args) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(args)?))
    }
}

impl EphemeralOp for EphemeralInstanceVerb {
    fn hash_params(&self, h: &mut Sha256, resolved: &LabelResolutions) {
        h.update(b"ephemeral_instance");
        // Hash the *resolved* symbol IDs (not the label string), so the
        // cache key reflects the actual rows we'll emit.  A literal
        // `symbol_id=42` hashes the same as before; `symbol="@x"` where
        // @x resolves to [42] hashes identically (cache shared if the
        // resolved set matches).
        let ids = self.symbol.resolve_vec(resolved);
        h.update((ids.len() as u64).to_le_bytes());
        for id in &ids {
            h.update(id.to_le_bytes());
        }
        h.update(self.object_id.to_le_bytes());
        h.update(self.start.to_le_bytes());
        h.update(self.end.to_le_bytes());
        h.update(self.instance_type.to_le_bytes());
    }

    fn collect_rows(&self, batch: &mut LayerBatch, resolved: &LabelResolutions) {
        for symbol_id in self.symbol.resolve_vec(resolved) {
            batch.instances.push(EphInstanceRow {
                symbol_id,
                object_id: self.object_id,
                start: self.start,
                end: self.end,
                instance_type: self.instance_type,
            });
        }
    }

    fn label_refs(&self) -> Vec<String> {
        self.symbol
            .label()
            .map(|l| vec![l.to_string()])
            .unwrap_or_default()
    }
}

/// EphemeralRefVerb - creates ephemeral ref rows.
///
/// Only available inside `layer { }` blocks.  Emits one row per
/// resolved to-symbol — see [`SymbolRef`] for the multi-row semantic.
///
/// Usage: `ephemeral_ref(to_symbol=<id>, from_object=1,
///        start=0, end=10)` for a literal (single-row) input,
///        or `to_symbol="@label"` for a label-resolved (`N`-row)
///        input.
#[derive(Debug)]
pub(in crate::verb) struct EphemeralRefVerb {
    to_symbol: SymbolRef,
    from_object: i32,
    start: i64,
    end: i64,
}

impl EphemeralRefVerb {
    pub(in crate::verb) const NAME: &'static str = "ephemeral_ref";

    fn create(args: &Args) -> Result<Self> {
        args.allow(&["to_symbol", "from_object", "start", "end"])?;
        let to_symbol = SymbolRef::from_value(args.required_named("to_symbol")?, "to_symbol")?;
        let from_object = args.req_i32("from_object")?;
        let start = args.req_i64("start")?;
        let end = args.req_i64("end")?;

        Ok(Self {
            to_symbol,
            from_object,
            start,
            end,
        })
    }

    pub(crate) fn new_op(_span: Span, args: &Args) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(args)?))
    }
}

impl EphemeralOp for EphemeralRefVerb {
    fn hash_params(&self, h: &mut Sha256, resolved: &LabelResolutions) {
        h.update(b"ephemeral_ref");
        let ids = self.to_symbol.resolve_vec(resolved);
        h.update((ids.len() as u64).to_le_bytes());
        for id in &ids {
            h.update(id.to_le_bytes());
        }
        h.update(self.from_object.to_le_bytes());
        h.update(self.start.to_le_bytes());
        h.update(self.end.to_le_bytes());
    }

    fn collect_rows(&self, batch: &mut LayerBatch, resolved: &LabelResolutions) {
        for to_symbol in self.to_symbol.resolve_vec(resolved) {
            batch.refs.push(EphRefRow {
                to_symbol,
                from_object: self.from_object,
                start: self.start,
                end: self.end,
            });
        }
    }

    fn label_refs(&self) -> Vec<String> {
        self.to_symbol
            .label()
            .map(|l| vec![l.to_string()])
            .unwrap_or_default()
    }
}

// --- LayerVerb ---

/// Groups multiple ephemeral operations into a single content-addressed layer.
///
/// `layer` is a normal verb registered in `build_generic_verb`. During parsing,
/// `update_context()` sets the shared ops vec on the ParserContext. Ephemeral
/// verbs in the scope inherit the context and push their ops into the shared vec.
///
/// Usage:
/// ```askl
/// layer {
///     ephemeral_symbol(name="foo", project_id=1, symbol_type=1);
///     ephemeral_instance(symbol_id=1, object_id=1,
///         start=100, end=200, instance_type=1);
/// }
/// ```
#[derive(Debug)]
pub(crate) struct LayerVerb {
    span: Span,
    ops: EphemeralOps,
}

impl LayerVerb {
    pub(in crate::verb) const NAME: &'static str = "layer";

    pub(in crate::verb) fn new(span: Span, _args: &Args) -> Result<Arc<dyn Verb>> {
        Ok(Arc::new(Self {
            span,
            ops: Arc::new(Mutex::new(Vec::new())),
        }))
    }
}

impl Verb for LayerVerb {
    fn name(&self) -> &str {
        "layer"
    }
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }
    /// Layer rows carry the types the layer literal declares; a scope's
    /// inherited default type filter must not second-guess them (see
    /// SearchSelector).  Explicit type filters still apply.
    fn suppresses_default_type_filter(&self) -> bool {
        true
    }
    fn anchor_kind(&self) -> Option<AnchorKind> {
        Some(AnchorKind::LayerLiteral)
    }
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }
    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    /// Layer-materialising: the emission is a layer read, not a single
    /// predicate query, so it cannot be an expression atom.
    fn compound_admission(&self) -> Result<(), String> {
        Err(
            "`layer` materialises a layer; its emission is not a single \
             predicate query, so it cannot appear in a filter expression \
             (write it beside the expression instead)"
                .to_string(),
        )
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        if ctx.get_eph_ops().is_some() {
            bail!("nested layer blocks are not allowed");
        }
        ctx.set_eph_ops(self.ops.clone());
        Ok(false) // stays in command
    }
}

#[async_trait(?Send)]
impl Selector for LayerVerb {
    fn has_layer_spec(&self) -> bool {
        true
    }

    fn layer_label_refs(&self) -> Vec<String> {
        // Uncontended — see EphemeralOps rustdoc.
        let ops = self.ops.lock().unwrap();
        ops.iter().flat_map(|op| op.label_refs()).collect()
    }

    async fn layer_spec(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        _composite_filter: &index::db_diesel::CompositeFilter,
        resolved: &LabelResolutions,
        _parent_scope: &index::db_diesel::ScopeContext,
    ) -> Result<Option<crate::verb::LayerSpec>> {
        // Classify ops, compute hashes, and collect both batches
        // synchronously, then release the lock before any .await points.
        // The `Mutex` is uncontended at this point — see `EphemeralOps`
        // rustdoc for why the lock exists at all.
        let (input_hash, selection_extra, root_batch, sel_batch) = {
            let ops = self.ops.lock().unwrap();
            if ops.is_empty() {
                bail!("layer block must contain at least one ephemeral verb");
            }

            // Classify each op by the ROWS it will actually emit, not by a
            // per-op trait override: collect its rows into a probe batch and
            // check every id-bearing field for eph (negative) ids.  Ops
            // whose rows reference only persistent ids (or nothing) form
            // the root-shard half — keyed by their resolved content, so the
            // root shard is shared across any upstream chains under which the
            // labels resolve identically.  Ops emitting rows that reference
            // eph ids form the selection-shard half.  Deriving this from the
            // rows in one place means a future op (or a new id-bearing row
            // field, once added to `batch_references_eph`) cannot be silently
            // mis-classified into the chain-independent shared root shard by a
            // forgotten override.  (`EphSymbolRow` carries no id
            // references — `symbol_scope` is a Local/Global discriminator,
            // not a symbol id.)
            let mut root_ops = Vec::new();
            let mut sel_ops = Vec::new();
            let mut root_batch = LayerBatch::new();
            let mut sel_batch = LayerBatch::new();
            for op in ops.iter() {
                let mut probe = LayerBatch::new();
                op.collect_rows(&mut probe, resolved);
                if batch_references_eph(&probe) {
                    sel_ops.push(op);
                    merge_batch(&mut sel_batch, probe);
                } else {
                    root_ops.push(op);
                    merge_batch(&mut root_batch, probe);
                }
            }

            if !sel_ops.is_empty() && !eph.has_chain() {
                // Only reachable via a literal negative id: labels cannot
                // resolve to eph rows without chain layers to supply them.
                // Without a chain the executor creates no selection shard, so
                // these rows would be silently dropped — error instead.
                bail!(
                    "layer block references ephemeral (negative) ids but no \
                     ephemeral context exists before this statement"
                );
            }

            let mut h = Sha256::new();
            // Explicit per-verb domain tag (byte-identical to the former
            // `EphLayerKind::Layer.as_str()`) so `layer{}` hashes stay disjoint
            // even though the layer kind is now the coarse `Ephemeral`.
            h.update(b"layer");
            for op in &root_ops {
                // Op insertion order is significant for the cache key by
                // design (within each half); two layers with the same ops in
                // different order will not share cache state.  Resolved
                // labels (if any) feed into the hash via the op itself, so
                // the cache key reflects the actual rows.
                op.hash_params(&mut h, resolved);
            }
            let input_hash: [u8; 32] = h.finalize().into();

            // The selection-shard key must fold the selection ops' resolved
            // content: (parent, input_hash) alone cannot distinguish two
            // blocks that share root-shard ops but differ in eph-referencing
            // ops.
            let selection_extra: Vec<u8> = if sel_ops.is_empty() {
                Vec::new()
            } else {
                let mut he = Sha256::new();
                he.update(b"selection-shard-ops-v1");
                for op in &sel_ops {
                    op.hash_params(&mut he, resolved);
                }
                he.finalize().to_vec()
            };

            (input_hash, selection_extra, root_batch, sel_batch)
        }; // lock released here

        // Validate op references against the visible root set BEFORE any
        // layer is created.  The scoped inserts partition rows per root in
        // SQL and silently drop rows matching no visible project — without
        // this check a typo'd project_id/object_id would commit hollow,
        // input-keyed (i.e. sticky) cache layers with no diagnostic, where
        // the pre-scoping inserts raised a loud FK error.
        let visible_projects: std::collections::HashSet<i32> =
            eph.roots().iter().map(|r| r.project_id).collect();
        for row in root_batch.symbols.iter().chain(sel_batch.symbols.iter()) {
            if !visible_projects.contains(&row.project_id) {
                bail!(
                    "ephemeral_symbol: project_id {} does not match any visible project",
                    row.project_id
                );
            }
        }
        let mut object_ids: Vec<i32> = root_batch
            .instances
            .iter()
            .chain(sel_batch.instances.iter())
            .map(|r| r.object_id)
            .chain(
                root_batch
                    .refs
                    .iter()
                    .chain(sel_batch.refs.iter())
                    .map(|r| r.from_object),
            )
            .collect();
        object_ids.sort_unstable();
        object_ids.dedup();
        if !object_ids.is_empty() {
            let known = cfg.index.load_object_projects(&object_ids).await?;
            for id in &object_ids {
                match known.get(id) {
                    None => bail!("layer block references unknown object_id {}", id),
                    Some(p) if !visible_projects.contains(p) => bail!(
                        "layer block references object_id {} in project {}, \
                         which is not a visible project",
                        id,
                        p
                    ),
                    Some(_) => {}
                }
            }
        }

        // Populates run once per root (`Fn`): the batches — which may mix
        // rows of several projects — are shared via `Arc`, and the scoped
        // insert's SQL partitions each root's share (symbols by their
        // explicit project_id, instances/refs by their object's project).
        let root_batch = std::sync::Arc::new(root_batch);
        let sel_batch = std::sync::Arc::new(sel_batch);
        let root_shard_populate: crate::verb::RootShardPopulate = Box::new(move |txn, root| {
            let batch = std::sync::Arc::clone(&root_batch);
            Box::pin(async move {
                txn.insert_batch(&batch, root.project_id).await?;
                // `layer { … }` blocks never truncate; truncated = false.
                Ok(false)
            })
        });
        let selection_shard_populate: crate::verb::SelectionShardPopulate =
            Box::new(move |txn, root, _root_shard_ref| {
                let batch = std::sync::Arc::clone(&sel_batch);
                Box::pin(async move {
                    txn.insert_batch(&batch, root.project_id).await?;
                    Ok(false)
                })
            });

        Ok(Some(crate::verb::LayerSpec {
            input_hash,
            root_shard_populate,
            selection_shard_populate,
            // `layer { … }` is chain-dependent ops, not a layer-shard content
            // scan, so it exposes no N-way layer shard: its delta stays the
            // selection shard keyed on `tip`.
            layer_shard_populate: None,
            selection_extra,
        }))
    }
}

impl Display for LayerVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lock is uncontended — see `EphemeralOps` rustdoc.
        write!(f, "LayerVerb({} ops)", self.ops.lock().unwrap().len())
    }
}
