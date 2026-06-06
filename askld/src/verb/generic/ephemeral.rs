use crate::cfg::ControlFlowGraph;
use crate::parser_context::ParserContext;
use crate::span::Span;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    EphContext, EphInstanceRow, EphRefRow, EphSymbolRow, LayerBatch,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FIELD,
    INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DOCUMENTATION,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};

use super::super::{DeriveMethod, Selector, Verb};

/// Trait for ephemeral operations that can be batched into a layer block.
///
/// Ephemeral verbs are only available inside `layer { }` blocks — this trait
/// is the only interface through which they execute.
pub(crate) trait EphemeralOp: std::fmt::Debug + Send + Sync {
    /// Contribute this operation's parameters to a combined hash.
    fn hash_params(&self, h: &mut Sha256);
    /// Collect this operation's rows into the batch for bulk insert.
    fn collect_rows(&self, batch: &mut LayerBatch);
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
    fn hash_params(&self, h: &mut Sha256) {
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

    fn collect_rows(&self, batch: &mut LayerBatch) {
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

/// EphemeralInstanceVerb - creates an ephemeral instance row.
///
/// Only available inside `layer { }` blocks.
/// Usage: ephemeral_instance(symbol_id="<id>", object_id="1",
///        start="0", end="10", instance_type="1")
#[derive(Debug)]
pub(in crate::verb) struct EphemeralInstanceVerb {
    symbol_id: i64,
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
        let symbol_id: i64 = parse_required!(named, "symbol_id", i64);
        let object_id: i32 = parse_required!(named, "object_id", i32);
        let start: i64 = parse_required!(named, "start", i64);
        let end: i64 = parse_required!(named, "end", i64);
        let instance_type: i32 = parse_required!(named, "instance_type", i32);
        if !(INSTANCE_TYPE_DEFINITION..=INSTANCE_TYPE_DOCUMENTATION).contains(&instance_type) {
            bail!("instance_type must be between {} and {} (got {})", INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DOCUMENTATION, instance_type);
        }

        Ok(Self {
            symbol_id,
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
    fn hash_params(&self, h: &mut Sha256) {
        h.update(b"ephemeral_instance");
        h.update(self.symbol_id.to_le_bytes());
        h.update(self.object_id.to_le_bytes());
        h.update(self.start.to_le_bytes());
        h.update(self.end.to_le_bytes());
        h.update(self.instance_type.to_le_bytes());
    }

    fn collect_rows(&self, batch: &mut LayerBatch) {
        batch.instances.push(EphInstanceRow {
            symbol_id: self.symbol_id,
            object_id: self.object_id,
            start: self.start,
            end: self.end,
            instance_type: self.instance_type,
        });
    }
}

/// EphemeralRefVerb - creates an ephemeral ref row.
///
/// Only available inside `layer { }` blocks.
/// Usage: ephemeral_ref(to_symbol="<id>", from_object="1",
///        start="0", end="10")
#[derive(Debug)]
pub(in crate::verb) struct EphemeralRefVerb {
    to_symbol: i64,
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
        let to_symbol: i64 = parse_required!(named, "to_symbol", i64);
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
    fn hash_params(&self, h: &mut Sha256) {
        h.update(b"ephemeral_ref");
        h.update(self.to_symbol.to_le_bytes());
        h.update(self.from_object.to_le_bytes());
        h.update(self.start.to_le_bytes());
        h.update(self.end.to_le_bytes());
    }

    fn collect_rows(&self, batch: &mut LayerBatch) {
        batch.refs.push(EphRefRow {
            to_symbol: self.to_symbol,
            from_object: self.from_object,
            start: self.start,
            end: self.end,
        });
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
}

#[async_trait(?Send)]
impl Selector for LayerVerb {
    fn has_layer_spec(&self) -> bool { true }

    async fn layer_spec(
        &self,
        _cfg: &ControlFlowGraph,
        eph: &EphContext,
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
            h.update(b"layer");
            h.update(parent_id.unwrap_or(0i64).to_le_bytes());
            for op in ops.iter() {
                // Op insertion order is significant for the cache key by design;
                // two layers with the same ops in different order will not share
                // cache state.
                op.hash_params(&mut h);
            }
            let hash_vec = h.finalize().to_vec();

            let mut batch = LayerBatch::new();
            for op in ops.iter() {
                op.collect_rows(&mut batch);
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
            kind: "layer",
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
