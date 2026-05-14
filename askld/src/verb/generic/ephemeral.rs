use crate::cfg::ControlFlowGraph;
use crate::span::Span;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, EphemeralOverlay, ScopeContext, Selection, SymbolInstanceIdMixin,
};
use index::symbols::SymbolInstanceId;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Selector, Verb};

// ============================================================================
// EphemeralSymbolVerb — adds a single symbol to the overlay; no selection.
// ============================================================================

/// Adds an ephemeral symbol to the per-query overlay.
///
/// Named args: symbol_id (i64), name, path (ltree text), project_id (i32),
///             symbol_type (i32), leaf_name; optional: scope (i32).
///
/// Returns empty selection — symbol has no instance and thus won't appear
/// in find_symbol results.  Use together with EphemeralInstanceVerb.
#[derive(Debug)]
pub(in crate::verb) struct EphemeralSymbolVerb {
    span: Span,
    symbol_id: i64,
    name: String,
    path: String,
    project_id: i32,
    symbol_type: i32,
    scope: Option<i32>,
    leaf_name: String,
}

impl EphemeralSymbolVerb {
    pub const NAME: &'static str = "ephemeral_symbol";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let symbol_id = named
            .get("symbol_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_symbol requires symbol_id"))?
            .parse::<i64>()?;
        let name = named
            .get("name")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_symbol requires name"))?
            .clone();
        let path = named
            .get("path")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_symbol requires path"))?
            .clone();
        let project_id = named
            .get("project_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_symbol requires project_id"))?
            .parse::<i32>()?;
        let symbol_type = named
            .get("symbol_type")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_symbol requires symbol_type"))?
            .parse::<i32>()?;
        let leaf_name = named.get("leaf_name").cloned().unwrap_or_default();
        let scope = named.get("scope").map(|s| s.parse::<i32>()).transpose()?;

        if !index::db_diesel::overlay::is_ephemeral_symbol_id(symbol_id) {
            bail!("symbol_id {} is not in the ephemeral range", symbol_id);
        }

        Ok(Arc::new(Self {
            span,
            symbol_id,
            name,
            path,
            project_id,
            symbol_type,
            scope,
            leaf_name,
        }))
    }
}

impl Verb for EphemeralSymbolVerb {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }
    fn as_selector<'a>(&'a self) -> anyhow::Result<&'a dyn super::super::Selector> {
        Ok(self)
    }
}

impl Display for EphemeralSymbolVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EphemeralSymbolVerb({})", self.name)
    }
}

#[async_trait(?Send)]
impl Selector for EphemeralSymbolVerb {
    async fn select_from_all_impl(
        &self,
        _cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<(Option<Selection>, EphemeralOverlay)> {
        let mut overlay = EphemeralOverlay::empty();
        overlay.symbol_ids.push(self.symbol_id);
        overlay.symbol_names.push(self.name.clone());
        overlay.symbol_paths.push(self.path.clone());
        overlay.symbol_project_ids.push(self.project_id);
        overlay.symbol_types.push(self.symbol_type);
        overlay.symbol_scopes.push(self.scope);
        overlay.symbol_leaf_names.push(self.leaf_name.clone());
        // No instance — selection is empty; symbol contributes to overlay only.
        Ok((None, overlay))
    }
}

// ============================================================================
// EphemeralInstanceVerb — adds an instance to the overlay; returns the
// instance as a selection node via find_symbol.
// ============================================================================

/// Adds an ephemeral instance to the per-query overlay.
///
/// The instance references an existing symbol identified by `symbol_id`.
/// That symbol may be persistent (already in the index) or ephemeral (added
/// by a preceding `ephemeral_symbol` verb).  Note: because selectors run in
/// parallel during `initialize_roots`, a same-query `ephemeral_symbol` verb's
/// symbol will not be visible inside this selector's `find_symbol` call; the
/// instance will still be present in `ctx.overlay` for graph queries but the
/// selection returned by this verb will be empty unless `symbol_id` refers to
/// a persistent symbol.
///
/// Named args: symbol_id (i64), instance_id (i32), object_id (i32),
///             start (i32), end (i32), instance_type (i32).
#[derive(Debug)]
pub(in crate::verb) struct EphemeralInstanceVerb {
    span: Span,
    symbol_id: i64,
    instance_id: i32,
    object_id: i32,
    start: i32,
    end: i32,
    instance_type: i32,
}

impl EphemeralInstanceVerb {
    pub const NAME: &'static str = "ephemeral_instance";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let symbol_id = named
            .get("symbol_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires symbol_id"))?
            .parse::<i64>()?;
        let instance_id = named
            .get("instance_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires instance_id"))?
            .parse::<i32>()?;
        let object_id = named
            .get("object_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires object_id"))?
            .parse::<i32>()?;
        let start = named
            .get("start")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires start"))?
            .parse::<i32>()?;
        let end = named
            .get("end")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires end"))?
            .parse::<i32>()?;
        let instance_type = named
            .get("instance_type")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_instance requires instance_type"))?
            .parse::<i32>()?;

        if !index::db_diesel::overlay::is_ephemeral_instance_id(instance_id) {
            bail!("instance_id {} is not in the ephemeral range", instance_id);
        }

        Ok(Arc::new(Self {
            span,
            symbol_id,
            instance_id,
            object_id,
            start,
            end,
            instance_type,
        }))
    }
}

impl Verb for EphemeralInstanceVerb {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }
    fn as_selector<'a>(&'a self) -> anyhow::Result<&'a dyn super::super::Selector> {
        Ok(self)
    }
}

impl Display for EphemeralInstanceVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EphemeralInstanceVerb({})", self.instance_id)
    }
}

#[async_trait(?Send)]
impl Selector for EphemeralInstanceVerb {
    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<(Option<Selection>, EphemeralOverlay)> {
        let mut overlay = EphemeralOverlay::empty();

        overlay.instance_ids.push(self.instance_id);
        overlay.instance_symbols.push(self.symbol_id);
        overlay.instance_object_ids.push(self.object_id);
        overlay.instance_offset_starts.push(self.start);
        overlay.instance_offset_ends.push(self.end);
        overlay.instance_types.push(self.instance_type);

        // Query via find_symbol to get the full SelectionNode with object/project data.
        // symbol_id must refer to a symbol already visible in the overlay (if ephemeral)
        // or in the persistent index.
        let instances = vec![SymbolInstanceId::new(self.instance_id)];
        let filter = CompositeFilter::leaf(SymbolInstanceIdMixin::new(&instances));
        let selection = cfg.index.find_symbol(&filter, parent_scope, children_scope, &overlay).await?;

        Ok((Some(selection), overlay))
    }
}

// ============================================================================
// EphemeralRefVerb — adds a ref to the overlay; returns empty selection.
// ============================================================================

/// Adds an ephemeral reference to the per-query overlay.
///
/// Named args: ref_id (i32), to_symbol (i64), from_object (i32),
///             start (i32), end (i32).
///
/// Returns empty selection — the ref participates as graph data only.
#[derive(Debug)]
pub(in crate::verb) struct EphemeralRefVerb {
    span: Span,
    ref_id: i32,
    to_symbol: i64,
    from_object: i32,
    start: i32,
    end: i32,
}

impl EphemeralRefVerb {
    pub const NAME: &'static str = "ephemeral_ref";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let ref_id = named
            .get("ref_id")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_ref requires ref_id"))?
            .parse::<i32>()?;
        let to_symbol = named
            .get("to_symbol")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_ref requires to_symbol"))?
            .parse::<i64>()?;
        let from_object = named
            .get("from_object")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_ref requires from_object"))?
            .parse::<i32>()?;
        let start = named
            .get("start")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_ref requires start"))?
            .parse::<i32>()?;
        let end = named
            .get("end")
            .ok_or_else(|| anyhow::anyhow!("ephemeral_ref requires end"))?
            .parse::<i32>()?;

        if !index::db_diesel::overlay::is_ephemeral_ref_id(ref_id) {
            bail!("ref_id {} is not in the ephemeral range", ref_id);
        }

        Ok(Arc::new(Self {
            span,
            ref_id,
            to_symbol,
            from_object,
            start,
            end,
        }))
    }
}

impl Verb for EphemeralRefVerb {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }
    fn as_selector<'a>(&'a self) -> anyhow::Result<&'a dyn super::super::Selector> {
        Ok(self)
    }
}

impl Display for EphemeralRefVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EphemeralRefVerb({})", self.ref_id)
    }
}

#[async_trait(?Send)]
impl Selector for EphemeralRefVerb {
    async fn select_from_all_impl(
        &self,
        _cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<(Option<Selection>, EphemeralOverlay)> {
        let mut overlay = EphemeralOverlay::empty();

        overlay.ref_ids.push(self.ref_id);
        overlay.ref_to_symbols.push(self.to_symbol);
        overlay.ref_from_objects.push(self.from_object);
        overlay.ref_from_offset_starts.push(self.start);
        overlay.ref_from_offset_ends.push(self.end);

        // Ref participates as graph data only; no selection returned.
        Ok((None, overlay))
    }
}

// ============================================================================
// DependencyRole impl — all three verbs use the default DependencyKind::Sufficient
// (dependency-free root selectors; no override needed).
// ============================================================================
