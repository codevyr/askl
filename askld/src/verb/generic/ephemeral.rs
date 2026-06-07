use crate::cfg::ControlFlowGraph;
use crate::parser_context::ParserContext;
use crate::span::Span;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    EphContext, EphInstanceRow, EphLayerKind, EphRefRow, EphSymbolRow, LayerBatch,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FIELD,
    INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DOCUMENTATION,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};

use super::super::{DeriveMethod, Selector, Verb};

/// Resolved selections for labels referenced by ephemeral verbs.  An
/// ephemeral verb that takes `symbol="@foo"` looks up the symbol IDs of
/// the statement labelled `@foo` here at hash/materialise time.
///
/// Populated by `compute_roots` immediately before pushing a layer-creating
/// statement's compute future: the prior labelled statement's selection is
/// guaranteed-applied by then (via the User dependency edge installed at
/// parse time).  Empty for statements with no `@label` references.
#[derive(Debug, Clone, Default)]
pub struct LabelResolutions {
    /// label name → symbol IDs from the labelled statement's selection.
    map: HashMap<String, Vec<i64>>,
}

impl LabelResolutions {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, label: String, symbol_ids: Vec<i64>) {
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
    fn label_refs(&self) -> Vec<String> { Vec::new() }
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

macro_rules! parse_required {
    ($named:expr, $key:expr, $t:ty) => {
        $named.get($key)
            .ok_or_else(|| anyhow::anyhow!("requires '{}' parameter", $key))?
            .parse::<$t>()
            .map_err(|_| anyhow::anyhow!("'{}' must be a valid {}", $key, stringify!($t)))?
    };
}

/// EphemeralSymbolVerb - creates an ephemeral symbol row in the DB.
///
/// Only available inside `layer { }` blocks.
/// Usage: ephemeral_symbol(name="sym", project_id="1", symbol_type="1")
#[derive(Debug)]
pub(in crate::verb) struct EphemeralSymbolVerb {
    name: String,
    project_id: i32,
    symbol_type: i32,
    scope: Option<i32>,
}

impl EphemeralSymbolVerb {
    pub(in crate::verb) const NAME: &'static str = "ephemeral_symbol";

    fn create(
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Self> {
        let name: String = parse_required!(named, "name", String);
        let project_id: i32 = parse_required!(named, "project_id", i32);
        let symbol_type: i32 = parse_required!(named, "symbol_type", i32);
        if !(SYMBOL_TYPE_FUNCTION..=SYMBOL_TYPE_FIELD).contains(&symbol_type) {
            bail!("symbol_type must be between {} and {} (got {})", SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FIELD, symbol_type);
        }
        let scope: Option<i32> = named.get("scope")
            .map(|s| s.parse())
            .transpose()
            .map_err(|_| anyhow::anyhow!("'scope' must be a valid i32"))?;

        Ok(Self {
            name,
            project_id,
            symbol_type,
            scope,
        })
    }

    pub(crate) fn new_op(
        _span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(positional, named)?))
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
            Some(s) => { h.update([1u8]); h.update(s.to_le_bytes()); }
            None    => { h.update([0u8]); }
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

/// Symbol-ID input for [`EphemeralInstanceVerb`]: either a literal i64 or
/// a `@label` reference resolved at materialise time.  When `Label`,
/// each resolved symbol gets its own instance row (so e.g.
/// `symbol="@func"` against a selection of three symbols emits three
/// rows, one per symbol).
#[derive(Debug)]
enum SymbolRef {
    Literal(i64),
    Label(String),
}

impl SymbolRef {
    /// Parse `"123"` → `Literal(123)` or `"@foo"` → `Label("foo".into())`.
    fn parse(raw: &str, key: &str) -> Result<Self> {
        if let Some(label) = raw.strip_prefix('@') {
            if label.is_empty() {
                bail!("'{}' label reference must not be empty", key);
            }
            Ok(SymbolRef::Label(label.to_string()))
        } else {
            let id: i64 = raw.parse().map_err(|_| {
                anyhow::anyhow!("'{}' must be a valid i64 or a @label reference", key)
            })?;
            Ok(SymbolRef::Literal(id))
        }
    }

    /// Resolve to the actual symbol IDs to materialise.  Literal → one
    /// element; Label → the labelled statement's selected symbol IDs.
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

/// EphemeralInstanceVerb - creates an ephemeral instance row.
///
/// Only available inside `layer { }` blocks.
/// Usage: `ephemeral_instance(symbol_id="<id>", object_id="1",
///        start="0", end="10", instance_type="1")`
///        or `symbol_id="@label"` to emit one row per symbol selected by
///        the labelled statement.
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

    fn create(
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Self> {
        let symbol_raw = named.get("symbol_id")
            .ok_or_else(|| anyhow::anyhow!("requires 'symbol_id' parameter"))?;
        let symbol = SymbolRef::parse(symbol_raw, "symbol_id")?;
        let object_id: i32 = parse_required!(named, "object_id", i32);
        let start: i64 = parse_required!(named, "start", i64);
        let end: i64 = parse_required!(named, "end", i64);
        let instance_type: i32 = parse_required!(named, "instance_type", i32);
        if !(INSTANCE_TYPE_DEFINITION..=INSTANCE_TYPE_DOCUMENTATION).contains(&instance_type) {
            bail!("instance_type must be between {} and {} (got {})", INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DOCUMENTATION, instance_type);
        }

        Ok(Self {
            symbol,
            object_id,
            start,
            end,
            instance_type,
        })
    }

    pub(crate) fn new_op(
        _span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(positional, named)?))
    }
}

impl EphemeralOp for EphemeralInstanceVerb {
    fn hash_params(&self, h: &mut Sha256, resolved: &LabelResolutions) {
        h.update(b"ephemeral_instance");
        // Hash the *resolved* symbol IDs (not the label string), so the
        // cache key reflects the actual rows we'll emit.  A literal
        // `symbol_id="42"` hashes the same as before; `symbol="@x"` where
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
        self.symbol.label().map(|l| vec![l.to_string()]).unwrap_or_default()
    }
}

/// EphemeralRefVerb - creates an ephemeral ref row.
///
/// Only available inside `layer { }` blocks.
/// Usage: `ephemeral_ref(to_symbol="<id>", from_object="1",
///        start="0", end="10")`
///        or `to_symbol="@label"` to emit one row per symbol selected by
///        the labelled statement.
#[derive(Debug)]
pub(in crate::verb) struct EphemeralRefVerb {
    to_symbol: SymbolRef,
    from_object: i32,
    start: i64,
    end: i64,
}

impl EphemeralRefVerb {
    pub(in crate::verb) const NAME: &'static str = "ephemeral_ref";

    fn create(
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Self> {
        let to_symbol_raw = named.get("to_symbol")
            .ok_or_else(|| anyhow::anyhow!("requires 'to_symbol' parameter"))?;
        let to_symbol = SymbolRef::parse(to_symbol_raw, "to_symbol")?;
        let from_object: i32 = parse_required!(named, "from_object", i32);
        let start: i64 = parse_required!(named, "start", i64);
        let end: i64 = parse_required!(named, "end", i64);

        Ok(Self {
            to_symbol,
            from_object,
            start,
            end,
        })
    }

    pub(crate) fn new_op(
        _span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn EphemeralOp>> {
        Ok(Arc::new(Self::create(positional, named)?))
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
        self.to_symbol.label().map(|l| vec![l.to_string()]).unwrap_or_default()
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
///     ephemeral_symbol(name="foo", project_id="1", symbol_type="1");
///     ephemeral_instance(symbol_id="1", object_id="1",
///         start="100", end="200", instance_type="1");
/// }
/// ```
#[derive(Debug)]
pub(crate) struct LayerVerb {
    span: Span,
    ops: EphemeralOps,
}

impl LayerVerb {
    pub(in crate::verb) const NAME: &'static str = "layer";

    pub(in crate::verb) fn new(
        span: Span,
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        Ok(Arc::new(Self {
            span,
            ops: Arc::new(Mutex::new(Vec::new())),
        }))
    }
}

impl Verb for LayerVerb {
    fn name(&self) -> &str { "layer" }
    fn span(&self) -> pest::Span<'_> { self.span.as_pest_span() }
    fn derive_method(&self) -> DeriveMethod { DeriveMethod::Skip }
    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> { Ok(self) }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        if ctx.get_eph_ops().is_some() {
            bail!("nested layer blocks are not allowed");
        }
        ctx.set_eph_ops(self.ops.clone());
        Ok(false) // stays in command
    }

    fn layer_label_refs(&self) -> Vec<String> {
        // Uncontended — see EphemeralOps rustdoc.
        let ops = self.ops.lock().unwrap();
        ops.iter().flat_map(|op| op.label_refs()).collect()
    }
}

#[async_trait(?Send)]
impl Selector for LayerVerb {
    fn has_layer_spec(&self) -> bool { true }

    async fn layer_spec(
        &self,
        _cfg: &ControlFlowGraph,
        eph: &EphContext,
        resolved: &LabelResolutions,
    ) -> Result<Option<crate::verb::LayerSpec>> {
        // Compute hash and collect batch synchronously, then release the lock
        // before any .await points.  The `Mutex` is uncontended at this point
        // — see `EphemeralOps` rustdoc for why the lock exists at all.
        let (hash, batch) = {
            let ops = self.ops.lock().unwrap();
            if ops.is_empty() {
                bail!("layer block must contain at least one ephemeral verb");
            }

            let parent_id = eph.last();
            let mut h = Sha256::new();
            h.update(EphLayerKind::Layer.as_str().as_bytes());
            h.update(parent_id.unwrap_or(0i64).to_le_bytes());
            for op in ops.iter() {
                // Op insertion order is significant for the cache key by design;
                // two layers with the same ops in different order will not share
                // cache state.  Resolved labels (if any) feed into the hash via
                // the op itself, so the cache key reflects the actual rows.
                op.hash_params(&mut h, resolved);
            }
            let hash_vec = h.finalize().to_vec();

            let mut batch = LayerBatch::new();
            for op in ops.iter() {
                op.collect_rows(&mut batch, resolved);
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&hash_vec);
            (hash, batch)
        }; // lock released here

        let populate: crate::verb::LayerPopulate = Box::new(move |txn| Box::pin(async move {
            txn.insert_batch(&batch).await?;
            Ok(())
        }));

        Ok(Some(crate::verb::LayerSpec {
            hash,
            kind: EphLayerKind::Layer,
            parent_id: eph.last(),
            populate,
        }))
    }
}

impl Display for LayerVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lock is uncontended — see `EphemeralOps` rustdoc.
        write!(f, "LayerVerb({} ops)", self.ops.lock().unwrap().len())
    }
}
