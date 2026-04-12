use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::DependencyRole;
use crate::parser_context::{
    ParserContext, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompoundNameMixin, CompositeFilter, ExactNameMixin, Index,
    LeafNameMixin, ParentReference, ScopeContext, Selection, SymbolTypeMixin,
};
use index::models_diesel::SymbolRef;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, OnceLock};

use super::name_filter;
use super::super::{DeriveMethod, Filter, Selector, Verb, VerbTag};

#[derive(Debug)]
pub struct NameSelector {
    span: Span,
    pub name: String,
}

impl NameSelector {
    pub(in crate::verb) const NAME: &'static str = "_name_select";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                name: name.clone(),
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for NameSelector {
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn name(&self) -> &str {
        NameSelector::NAME
    }
}

#[async_trait(?Send)]
impl Selector for NameSelector {
    fn build_composite_filter(&self, command: &crate::command::Command) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> =
            command.filters().filter_map(|f| f.get_composite_filter()).collect();
        parts.push(name_filter(&self.name));
        Some(CompositeFilter::and(parts))
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let combined = CompositeFilter::and(vec![filter, name_filter(&self.name)]);
        let selection = cfg.index.find_symbol(&combined, parent_scope, children_scope).await?;
        Ok(Some(selection))
    }
}

#[derive(Debug)]
pub(in crate::verb) struct ForcedVerb {
    span: Span,
    name: String,
    selection: Arc<OnceLock<Selection>>,
}

impl ForcedVerb {
    pub(in crate::verb) const NAME: &'static str = "forced";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                name: name.clone(),
                selection: Arc::new(OnceLock::new()),
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for ForcedVerb {
    fn name(&self) -> &str {
        ForcedVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for ForcedVerb {
    fn build_composite_filter(&self, command: &crate::command::Command) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> =
            command.filters().filter_map(|f| f.get_composite_filter()).collect();
        parts.push(name_filter(&self.name));
        Some(CompositeFilter::and(parts))
    }

    fn dependency_ready(&self, dependency_role: DependencyRole) -> bool {
        if dependency_role == DependencyRole::Parent {
            false
        } else {
            true
        }
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let combined = CompositeFilter::and(vec![filter, name_filter(&self.name)]);
        let selection = cfg.index.find_symbol(&combined, parent_scope, children_scope).await?;

        // Cache the forced selection so derivations can fabricate the
        // correct parent <-> child relationship later on.
        let _ = self.selection.set(selection);

        // Forced matches shouldn't directly contribute nodes; they are only
        // materialised when another statement (e.g. a parent) references
        // them. Returning an empty selection keeps the execution state unset
        // so derivations can populate it when needed.
        Ok(None)
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        parent: &Statement,
        _notif_ctx: super::super::NotificationContext,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let parent_selection = match parent.get_selection(ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };

        let cached_selection = self.selection.get().cloned();

        let mut normal_selection = match cached_selection {
            Some(selection) => selection,
            None => {
                return Ok(Some(Selection::new()));
            }
        };

        let mut fake_parent_references = Vec::<ParentReference>::new();
        for parent_node in parent_selection.nodes.iter() {
            for child_node in normal_selection.nodes.iter() {
                let reference = ParentReference {
                    to_symbol: child_node.symbol.clone(),
                    to_instance: child_node.symbol_instance.clone(),
                    from_instance: parent_node.symbol_instance.clone(),
                    symbol_ref: SymbolRef {
                        id: 0,
                        to_symbol: child_node.symbol.id,
                        from_object: parent_node.object.id,
                        from_offset_range: parent_node.symbol_instance.offset_range.clone(),
                    },
                };
                fake_parent_references.push(reference);
            }
        }

        normal_selection.parents = fake_parent_references;

        Ok(Some(normal_selection))
    }
}

impl Display for ForcedVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForcedVerb(name={})", self.name)
    }
}

#[derive(Debug)]
pub struct UnitVerb {
    span: Span,
}

impl UnitVerb {
    pub fn new(span: Span) -> Arc<dyn Verb> {
        Arc::new(Self { span })
    }
}

impl Verb for UnitVerb {
    fn name(&self) -> &str {
        "unit"
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn is_unit(&self) -> bool {
        true
    }
}

#[async_trait(?Send)]
impl Selector for UnitVerb {
    async fn select_from_all_impl(
        &self,
        _cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        Ok(None)
    }
}

impl Display for UnitVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnitVerb")
    }
}

/// TypeSelector - selects symbols by type (func, file, mod, dir)
///
/// # Behavior Modes
///
/// - **Filter mode** (`filter_only=true`): Returns `None` from `select_from_all`,
///   forcing derivation from parent. Much more efficient when used inside `has { }`.
///   At root level (no parent), returns empty results.
///
/// - **Selector mode** (`filter_only=false`): Queries all symbols of this type
///   from the database. Works at any level including root.
///
/// # Default Behavior
///
/// The default is optimized for performance - querying all symbols of a type
/// can be expensive, so filter mode is preferred when possible:
///
/// - `func` (no args) -> filter mode
/// - `func("foo")` (with name) -> selector mode
/// - `func(filter="true")` -> explicitly filter mode
/// - `func(filter="false")` -> explicitly selector mode (select all)
/// - `func("foo", filter="true")` -> filter mode even with name
///
/// # Examples
///
/// ```text
/// file("main.go") has { func }             // filter: derives functions from file
/// func("main")                              // selector: queries for "main" function
/// func(filter="false")                      // selector: queries ALL functions
/// func                                      // filter: empty at root, derives in has
/// ```
#[derive(Debug)]
pub(in crate::verb) struct TypeSelector {
    span: Span,
    symbol_type_id: i32,
    name_pattern: Option<String>,
    /// If true, don't select from all - only act as a filter when deriving from parent.
    /// This is much more efficient for queries like `file has { func }`.
    filter_only: bool,
    /// If true, this filter is inherited (cloned) into derived child scopes.
    /// Used for namespace filters like `mod("test", filter="true", inherit="true")`.
    inherit: bool,
    /// If true, the last query token is anchored to the last path component.
    /// Default for dir and file; can be overridden with `match="contains"`.
    leaf_anchored: bool,
}

impl TypeSelector {
    pub(in crate::verb) const NAME_FUNCTION: &'static str = "func";
    pub(in crate::verb) const NAME_FILE: &'static str = "file";
    pub(in crate::verb) const NAME_MODULE: &'static str = "mod";
    pub(in crate::verb) const NAME_DIRECTORY: &'static str = "dir";
    pub(in crate::verb) const NAME_TYPE: &'static str = "type";
    pub(in crate::verb) const NAME_DATA: &'static str = "data";
    pub(in crate::verb) const NAME_MACRO: &'static str = "macro";
    pub(in crate::verb) const NAME_FIELD: &'static str = "field";
    pub(in crate::verb) const NAME_METHOD: &'static str = "method";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
        symbol_type_id: i32,
    ) -> Result<Arc<dyn Verb>> {
        let name_pattern = positional.first().cloned();

        // Check for explicit filter argument (true or false)
        let explicit_filter = named.get("filter").map(|v| v.eq_ignore_ascii_case("true"));

        // Default: filter mode if no name pattern, selector mode if name provided
        // Can be overridden with explicit filter="true" or filter="false"
        let filter_only = match explicit_filter {
            Some(true) => true,             // filter="true" forces filter mode
            Some(false) => false,           // filter="false" forces selector mode
            None => name_pattern.is_none(), // default based on name presence
        };

        // Bare type selectors (no name) inherit by default so they propagate
        // the type filter into child scopes. Named type selectors don't inherit.
        let inherit = named
            .get("inherit")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(name_pattern.is_none());

        let leaf_anchored = match named.get("match").map(|v| v.as_str()) {
            Some("contains") => false,
            _ => matches!(symbol_type_id, SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE),
        };

        Ok(Arc::new(Self {
            span,
            symbol_type_id,
            name_pattern,
            filter_only,
            inherit,
            leaf_anchored,
        }))
    }

    /// Returns the appropriate name filter for the given name and symbol type.
    fn name_filter_leaf(
        name: &str,
        symbol_type_id: i32,
        leaf_anchored: bool,
    ) -> CompositeFilter {
        match symbol_type_id {
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE if name.starts_with('/') => {
                CompositeFilter::leaf(ExactNameMixin::new(name))
            }
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE => {
                let is_compound = name.contains('/') || name.contains(':');
                if is_compound {
                    CompositeFilter::leaf(CompoundNameMixin::with_options(name, leaf_anchored, false))
                } else {
                    CompositeFilter::leaf(LeafNameMixin::new(name, false))
                }
            }
            _ => {
                let is_compound = name.contains('.') || name.contains('/') || name.contains(':');
                if is_compound {
                    if leaf_anchored {
                        CompositeFilter::leaf(CompoundNameMixin::new_leaf_anchored(name))
                    } else {
                        CompositeFilter::leaf(CompoundNameMixin::new(name))
                    }
                } else {
                    CompositeFilter::leaf(LeafNameMixin::new(name, true))
                }
            }
        }
    }

    /// Build composite filter parts for this type selector.
    fn build_filter_parts(&self) -> Vec<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> =
            vec![CompositeFilter::leaf(SymbolTypeMixin::new(self.symbol_type_id))];
        if let Some(ref name) = self.name_pattern {
            parts.push(Self::name_filter_leaf(
                name,
                self.symbol_type_id,
                self.leaf_anchored,
            ));
        }
        parts
    }
}

impl Verb for TypeSelector {
    fn name(&self) -> &str {
        match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => TypeSelector::NAME_FUNCTION,
            SYMBOL_TYPE_FILE => TypeSelector::NAME_FILE,
            SYMBOL_TYPE_MODULE => TypeSelector::NAME_MODULE,
            SYMBOL_TYPE_DIRECTORY => TypeSelector::NAME_DIRECTORY,
            SYMBOL_TYPE_TYPE => TypeSelector::NAME_TYPE,
            SYMBOL_TYPE_DATA => TypeSelector::NAME_DATA,
            SYMBOL_TYPE_MACRO => TypeSelector::NAME_MACRO,
            SYMBOL_TYPE_FIELD => TypeSelector::NAME_FIELD,
            _ => "type_selector",
        }
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        if self.filter_only && self.name_pattern.is_none() && self.inherit {
            Err(anyhow!(
                "Filter-only inherited TypeSelector is not a selector"
            ))
        } else {
            Ok(self)
        }
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        if self.inherit {
            DeriveMethod::Clone
        } else {
            DeriveMethod::Skip
        }
    }

    fn derive_new_instance(&self) -> Option<Arc<dyn Verb>> {
        // Create a fresh TypeSelector instance so derived child scopes
        // get their own registry entry, avoiding shared state issues.
        Some(Arc::new(TypeSelector {
            span: self.span.clone(),
            symbol_type_id: self.symbol_type_id,
            name_pattern: self.name_pattern.clone(),
            filter_only: self.filter_only,
            inherit: self.inherit,
            leaf_anchored: self.leaf_anchored,
        }))
    }

    fn get_tag(&self) -> Option<VerbTag> {
        if self.filter_only && self.name_pattern.is_some() {
            Some(VerbTag::TypeFilter)
        } else {
            None
        }
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        // Any type selector replaces inherited type selectors.
        // e.g. `data` or `data("foo")` inside a `func` scope replaces the
        // inherited func filter rather than AND'ing with it.
        existing_verbs
            .into_iter()
            .filter(|v| !v.suppresses_default_type_filter())
            .collect()
    }

    /// Set default symbol types and relationship type for child scopes.
    /// Container types (dir, file, mod) implicitly set refs+has with inherit.
    /// func explicitly sets REFS to override any inherited refs+has.
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        let default_types = match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => vec![SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_TYPE => vec![SYMBOL_TYPE_TYPE],
            SYMBOL_TYPE_FIELD => vec![SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_MACRO => vec![SYMBOL_TYPE_MACRO, SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_MODULE => vec![SYMBOL_TYPE_MODULE, SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_FILE => vec![SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MODULE],
            SYMBOL_TYPE_DIRECTORY => vec![SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FILE],
            _ => vec![SYMBOL_TYPE_FUNCTION],
        };
        ctx.set_default_symbol_types(default_types);

        // Don't consume - still add this verb to the command
        Ok(false)
    }

    fn is_non_constraining_selector(&self) -> bool {
        // Filter mode without a name pattern: this verb provides no meaningful
        // selection or constraint — it only filters by symbol type.
        self.filter_only && self.name_pattern.is_none()
    }

    fn suppresses_default_type_filter(&self) -> bool {
        true
    }
}

impl Filter for TypeSelector {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        if self.filter_only && self.name_pattern.is_some() {
            // When used as a namespace filter (e.g., mod("test", filter="true")),
            // only constrain by name pattern, not by type.
            let name = self.name_pattern.as_ref().unwrap();
            let dot_is_separator = !matches!(
                self.symbol_type_id,
                SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE
            );
            Some(CompositeFilter::leaf(CompoundNameMixin::with_options(name, false, dot_is_separator)))
        } else {
            Some(CompositeFilter::and(self.build_filter_parts()))
        }
    }
}

#[async_trait(?Send)]
impl Selector for TypeSelector {
    fn build_composite_filter(&self, command: &crate::command::Command) -> Option<CompositeFilter> {
        // TypeSelector implements as_filter(), so its get_composite_filter()
        // is already included via command.filters().
        let parts: Vec<CompositeFilter> =
            command.filters().filter_map(|f| f.get_composite_filter()).collect();
        if parts.is_empty() { None } else { Some(CompositeFilter::and(parts)) }
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        if self.filter_only {
            return Ok(None);
        }

        // `filter` already contains this TypeSelector's get_composite_filter()
        // (collected at compute_selected). Just use it directly.
        let selection = cfg.index.find_symbol(&filter, parent_scope, children_scope).await?;
        Ok(Some(selection))
    }
}

impl Display for TypeSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => write!(f, "TypeSelector(function)"),
            SYMBOL_TYPE_FILE => write!(f, "TypeSelector(file)"),
            SYMBOL_TYPE_MODULE => write!(f, "TypeSelector(module)"),
            SYMBOL_TYPE_DIRECTORY => write!(f, "TypeSelector(directory)"),
            SYMBOL_TYPE_TYPE => write!(f, "TypeSelector(type)"),
            SYMBOL_TYPE_DATA => write!(f, "TypeSelector(data)"),
            SYMBOL_TYPE_MACRO => write!(f, "TypeSelector(macro)"),
            SYMBOL_TYPE_FIELD => write!(f, "TypeSelector(field)"),
            _ => write!(f, "TypeSelector({})", self.symbol_type_id),
        }
    }
}

// ============================================================================
// GenericSelector — select
// ============================================================================

#[derive(Debug)]
pub struct GenericSelector {
    span: Span,
}

impl GenericSelector {
    pub const NAME: &'static str = "select";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for GenericSelector {
    fn name(&self) -> &str {
        GenericSelector::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn requires_name_constraint(&self) -> bool {
        true
    }

    fn get_tag(&self) -> Option<VerbTag> {
        Some(VerbTag::GenericSelector)
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        self.replace_verb(existing_verbs)
    }
}

#[async_trait(?Send)]
impl Selector for GenericSelector {
    fn build_composite_filter(&self, command: &crate::command::Command) -> Option<CompositeFilter> {
        let parts: Vec<CompositeFilter> =
            command.filters().filter_map(|f| f.get_composite_filter()).collect();
        if parts.is_empty() { None } else { Some(CompositeFilter::and(parts)) }
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let selection = cfg.index.find_symbol(&filter, parent_scope, children_scope).await?;
        Ok(Some(selection))
    }
}

impl Display for GenericSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericSelector")
    }
}
