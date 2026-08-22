use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::{DependencyKind, DependencyRole};
use crate::name_pattern::NamePattern;
use crate::parser::Value;
use crate::parser_context::{
    ParserContext, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, CompoundNameMixin, EphContext, ExactNameMixin, Index, LeafNameMixin,
    ParentReference, ScopeContext, Selection, SymbolTypeMixin,
};
use index::models_diesel::SymbolRef;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, OnceLock};

use super::super::{
    AnchorKind, DeriveMethod, Dimension, Filter, OwnPredicate, Selector, Verb, VerbClass,
};
use super::name_filter;

#[derive(Debug)]
pub struct NameSelector {
    span: Span,
    pub pattern: NamePattern,
}

impl NameSelector {
    pub(in crate::verb) const NAME: &'static str = "_name_select";

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                pattern: NamePattern::from_value(name)?,
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
    }

    fn anchor_kind(&self) -> Option<AnchorKind> {
        Some(AnchorKind::Name)
    }

    fn name(&self) -> &str {
        NameSelector::NAME
    }

    fn matched_token(&self) -> Option<String> {
        // Only plain names get "did you mean?" — a glob matching nothing is a
        // different situation and fuzzy-matching its raw pattern is meaningless.
        match &self.pattern {
            NamePattern::Exact(name) => Some(name.clone()),
            NamePattern::Glob(_) => None,
        }
    }
}

#[async_trait(?Send)]
impl Selector for NameSelector {
    fn build_composite_filter(
        &self,
        command: &crate::command::Command,
        eph: &EphContext,
    ) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> = command
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        parts.push(name_filter(&self.pattern));
        Some(CompositeFilter::and(parts))
    }

    fn own_predicate(&self, _eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Filter(Some(name_filter(&self.pattern)))
    }

    fn fusable(&self) -> bool {
        true
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        let combined = CompositeFilter::and(vec![filter, name_filter(&self.pattern)]);
        let selection = cfg
            .index
            .find_symbol(&combined, parent_scope, children_scope, eph)
            .await?
            .into_inner();
        Ok(Some(selection))
    }
}

#[derive(Debug)]
pub(in crate::verb) struct ForcedVerb {
    span: Span,
    pattern: NamePattern,
    selection: Arc<OnceLock<Selection>>,
}

impl ForcedVerb {
    pub(in crate::verb) const NAME: &'static str = "forced";

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                pattern: NamePattern::from_value(name)?,
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn anchor_kind(&self) -> Option<AnchorKind> {
        Some(AnchorKind::Name)
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
    }
}

#[async_trait(?Send)]
impl Selector for ForcedVerb {
    fn build_composite_filter(
        &self,
        command: &crate::command::Command,
        eph: &EphContext,
    ) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> = command
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        parts.push(name_filter(&self.pattern));
        Some(CompositeFilter::and(parts))
    }

    /// Predicate-expressible (feeds scope narrowing), but NOT fusable: the
    /// forced emission path caches its match set and returns `None`.
    fn own_predicate(&self, _eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Filter(Some(name_filter(&self.pattern)))
    }

    fn dependency_kind(&self, role: DependencyRole) -> DependencyKind {
        match role {
            // ForcedVerb cannot compute without its parent's selection.
            DependencyRole::Child => DependencyKind::Necessary,
            _ => DependencyKind::Sufficient,
        }
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        let combined = CompositeFilter::and(vec![filter, name_filter(&self.pattern)]);
        let selection = cfg
            .index
            .find_symbol(&combined, parent_scope, children_scope, eph)
            .await?
            .into_inner();

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
        _notif_ctx: &super::super::NotificationContext,
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
                        // Synthetic in-memory ref: the layer follows the
                        // FROM side like real refs' does (from_object/
                        // from_offset_range come from the parent).  The
                        // parent is from a Checked selection, so the id is
                        // visible to the leak check either way.
                        layer: parent_node.symbol_instance.layer,
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
        write!(f, "ForcedVerb(name={})", self.pattern)
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
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
        _eph: &EphContext,
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
/// - **Constraint mode** (no name): Returns `None` from `select_from_all`,
///   forcing derivation from parent. Much more efficient when used inside `has { }`.
///   At root level (no parent), returns empty results.
///
/// - **Selector mode** (with a name): Queries the named symbols of this type
///   from the database. Works at any level including root.
///
/// # Default Behavior
///
/// Name presence decides the role — the old `filter=` plan toggle is a
/// parse error; cost-based execution decides materialisation:
///
/// - `func` (no name) -> pure constraint (derives from neighbours)
/// - `func("foo")` (with name) -> selector (name-anchored)
/// - `func select` -> enumerate all functions, budget-bounded
///
/// # Examples
///
/// ```text
/// file("main.go") has { func }             // constraint: derives functions from file
/// func("main")                              // selector: queries for "main" function
/// func select                               // enumerates ALL functions (budget-bounded)
/// func                                      // constraint: anchor error at root, derives in scopes
/// ```
#[derive(Debug)]
pub(in crate::verb) struct TypeSelector {
    span: Span,
    /// The symbol type this verb constrains to, or `None` for `any` — the
    /// member of the family that constrains no type at all.  `any` is not a
    /// separate kind of verb: it selects, anchors, globs, strips inherited
    /// type filters and weakens exactly like its typed siblings, and differs
    /// only in contributing no type predicate.
    symbol_type_id: Option<i32>,
    name_pattern: Option<NamePattern>,
    /// If true, don't select from all - only act as a filter when deriving from parent.
    /// This is much more efficient for queries like `file has { func }`.
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
    pub(in crate::verb) const NAME_ANY: &'static str = "any";

    pub fn new(
        span: Span,
        positional: &Vec<Value>,
        named: &HashMap<String, Value>,
        symbol_type_id: Option<i32>,
    ) -> Result<Arc<dyn Verb>> {
        let name_pattern = positional
            .first()
            .map(NamePattern::from_value)
            .transpose()?;

        // `filter=` was a manual query-plan toggle; statements are
        // cost-planned now, so it is gone rather than silently ignored
        // (its namespace mode changed a query's MEANING, not just its plan).
        if named.contains_key("filter") {
            bail!(
                "`filter=` was removed: statements are cost-planned. Use `select` to \
                 make a statement binding (was filter=\"false\"); for namespace \
                 scoping use composition (mod(\"x\") has {{ ... }}) or \
                 filter(\"compound_name\", ...) (was mod(\"x\", filter=\"true\") name)"
            );
        }

        // Bare type selectors (no name) inherit by default so they propagate
        // the type filter into child scopes. Named type selectors don't inherit.
        let inherit = match named.get("inherit") {
            Some(v) => v.as_plain()?.eq_ignore_ascii_case("true"),
            None => name_pattern.is_none(),
        };

        let leaf_anchored = match crate::parser::named_plain(named, "match")? {
            Some("contains") => false,
            // Files and directories anchor to the leaf by default; code
            // symbols (where '.' separates labels) do not.  `any` follows the
            // type-agnostic convention of `name_filter`, which anchors — that
            // is what makes `any("foo")` and `any "foo"` the same query.
            _ => match symbol_type_id {
                Some(id) => !index::symbols::dot_is_separator(id),
                None => true,
            },
        };

        Ok(Arc::new(Self {
            span,
            symbol_type_id,
            name_pattern,
            inherit,
            leaf_anchored,
        }))
    }

    /// Returns the appropriate name filter for the given pattern and symbol type.
    fn name_filter_leaf(
        pattern: &NamePattern,
        symbol_type_id: i32,
        leaf_anchored: bool,
    ) -> CompositeFilter {
        let name = match pattern {
            NamePattern::Exact(name) => name,
            NamePattern::Glob(glob) => {
                return glob.filter(
                    index::symbols::dot_is_separator(symbol_type_id),
                    leaf_anchored,
                );
            }
        };
        match symbol_type_id {
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE if name.starts_with('/') => {
                CompositeFilter::leaf(ExactNameMixin::new(name))
            }
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE => {
                let is_compound = name.contains('/') || name.contains(':');
                if is_compound {
                    CompositeFilter::leaf(CompoundNameMixin::with_options(
                        name,
                        leaf_anchored,
                        false,
                    ))
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
    ///
    /// `any` contributes no type part, and takes its name predicate from
    /// [`super::name_filter`] — the same helper a bare `"name"` selector uses —
    /// so `any("foo")` and `any "foo"` are one query, not two spellings that
    /// can drift apart.
    fn build_filter_parts(&self) -> Vec<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> = Vec::new();
        if let Some(id) = self.symbol_type_id {
            parts.push(CompositeFilter::leaf(SymbolTypeMixin::new(id)));
        }
        if let Some(ref name) = self.name_pattern {
            parts.push(match self.symbol_type_id {
                Some(id) => Self::name_filter_leaf(name, id, self.leaf_anchored),
                None => super::name_filter(name),
            });
        }
        parts
    }
}

impl Verb for TypeSelector {
    fn name(&self) -> &str {
        let Some(id) = self.symbol_type_id else {
            return TypeSelector::NAME_ANY;
        };
        match id {
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

    /// `func("foo")` anchors by name; bare `func` is a pure constraint
    /// (`func select` is the enumerate-all opt-in).
    fn anchor_kind(&self) -> Option<AnchorKind> {
        self.name_pattern.as_ref().map(|_| AnchorKind::Name)
    }

    fn matched_token(&self) -> Option<String> {
        // A typed selector like `func "vfs_read"` carries the name here; expose
        // it so no-match diagnostics offer suggestions for these too, not just
        // bare NameSelector. Globs don't get suggestions (see NameSelector).
        match &self.name_pattern {
            Some(NamePattern::Exact(name)) => Some(name.clone()),
            _ => None,
        }
    }

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        // Instance-conditional role: an inherited bare type selector is a
        // pure filter (it propagates the type constraint, never anchors or
        // holds execution state of its own).
        if self.name_pattern.is_none() && self.inherit {
            None
        } else {
            Some(self)
        }
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
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
            inherit: self.inherit,
            leaf_anchored: self.leaf_anchored,
        }))
    }

    /// Any type selector — bare or named — writes the type dimension, so it
    /// replaces an earlier type constraint (`data` or `data("foo")` inside a
    /// `func` scope replaces the inherited func filter rather than AND'ing
    /// with it).  It no longer evicts `search`/`loc`/`layer`: those suppress
    /// the DEFAULT type filter but do not occupy the type dimension.
    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Type)
    }

    /// Set default symbol types and relationship type for child scopes.
    /// Container types (dir, file, mod) implicitly set refs+has with inherit.
    /// func explicitly sets REFS to override any inherited refs+has.
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        // `any` asks for no type, and that answer propagates: an EMPTY default
        // list means child scopes get no `DefaultTypeFilter` at all.  Falling
        // into the catch-all arm below would silently constrain `any`'s
        // children to functions, which is the opposite of what it means.
        let default_types = match self.symbol_type_id {
            None => vec![],
            Some(SYMBOL_TYPE_FUNCTION) => vec![SYMBOL_TYPE_FUNCTION],
            Some(SYMBOL_TYPE_TYPE) => vec![SYMBOL_TYPE_TYPE],
            Some(SYMBOL_TYPE_FIELD) => vec![SYMBOL_TYPE_FUNCTION],
            Some(SYMBOL_TYPE_MACRO) => vec![SYMBOL_TYPE_MACRO, SYMBOL_TYPE_FUNCTION],
            Some(SYMBOL_TYPE_MODULE) => vec![SYMBOL_TYPE_MODULE, SYMBOL_TYPE_FUNCTION],
            Some(SYMBOL_TYPE_FILE) => vec![SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MODULE],
            Some(SYMBOL_TYPE_DIRECTORY) => vec![SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FILE],
            Some(_) => vec![SYMBOL_TYPE_FUNCTION],
        };
        ctx.set_default_symbol_types(default_types);

        // Don't consume - still add this verb to the command
        Ok(false)
    }

    fn is_non_constraining_selector(&self) -> bool {
        // No name pattern: this verb provides no meaningful selection or
        // constraint of its own — it only filters by symbol type.
        self.name_pattern.is_none()
    }

    fn suppresses_default_type_filter(&self) -> bool {
        true
    }
}

impl Filter for TypeSelector {
    fn get_composite_filter(&self, _eph: &EphContext) -> Option<CompositeFilter> {
        // Uniform: type ∧ name.  The old namespace mode (name-only predicate
        // under `filter="true"`) went with the `filter=` toggle; namespace
        // scoping is composition (`mod("x") has { … }`) or
        // filter("compound_name", …) now.
        //
        // Bare `any` constrains neither, and says so with `None` rather than
        // an empty `AND` — a filter that means "no restriction" should be
        // absent from the tree, not a node in it.
        let parts = self.build_filter_parts();
        if parts.is_empty() {
            return None;
        }
        Some(CompositeFilter::and(parts))
    }
}

#[async_trait(?Send)]
impl Selector for TypeSelector {
    fn build_composite_filter(
        &self,
        command: &crate::command::Command,
        eph: &EphContext,
    ) -> Option<CompositeFilter> {
        // TypeSelector implements as_filter(), so its get_composite_filter()
        // is already included via command.filters().
        let parts: Vec<CompositeFilter> = command
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(CompositeFilter::and(parts))
        }
    }

    /// No extra constraint beyond the filter conjunction (this selector's
    /// type/name criteria arrive via `as_filter`).  Fusable only in selector
    /// mode: filter-only emission is `None` (waits to be derived), which one
    /// shared statement query must not replace with rows.
    fn own_predicate(&self, _eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Filter(None)
    }

    fn fusable(&self) -> bool {
        self.name_pattern.is_some()
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        if self.name_pattern.is_none() {
            // Pure constraint: nothing to emit; the statement derives.
            return Ok(None);
        }

        // `filter` already contains this TypeSelector's get_composite_filter()
        // (collected at compute_selected). Just use it directly.
        let selection = cfg
            .index
            .find_symbol(&filter, parent_scope, children_scope, eph)
            .await?
            .into_inner();
        Ok(Some(selection))
    }
}

impl Display for TypeSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some(id) = self.symbol_type_id else {
            return write!(f, "TypeSelector(any)");
        };
        match id {
            SYMBOL_TYPE_FUNCTION => write!(f, "TypeSelector(function)"),
            SYMBOL_TYPE_FILE => write!(f, "TypeSelector(file)"),
            SYMBOL_TYPE_MODULE => write!(f, "TypeSelector(module)"),
            SYMBOL_TYPE_DIRECTORY => write!(f, "TypeSelector(directory)"),
            SYMBOL_TYPE_TYPE => write!(f, "TypeSelector(type)"),
            SYMBOL_TYPE_DATA => write!(f, "TypeSelector(data)"),
            SYMBOL_TYPE_MACRO => write!(f, "TypeSelector(macro)"),
            SYMBOL_TYPE_FIELD => write!(f, "TypeSelector(field)"),
            _ => write!(f, "TypeSelector({id})"),
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
        _positional: &Vec<Value>,
        _named: &HashMap<String, Value>,
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
    }

    /// `select` is the bindness verb: it declares the tree binding ("I
    /// want instances"), makes its own command strong, and carries the All
    /// anchor — an unanchored command with `select` enumerates everything
    /// its filters allow, budget-bounded with a truncation warning.
    fn anchor_kind(&self) -> Option<AnchorKind> {
        Some(AnchorKind::All)
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Select)
    }
}

#[async_trait(?Send)]
impl Selector for GenericSelector {
    fn build_composite_filter(
        &self,
        command: &crate::command::Command,
        eph: &EphContext,
    ) -> Option<CompositeFilter> {
        let parts: Vec<CompositeFilter> = command
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(CompositeFilter::and(parts))
        }
    }

    /// No constraint of its own: `select` emits whatever the command's
    /// filters allow.
    fn own_predicate(&self, _eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Filter(None)
    }

    fn fusable(&self) -> bool {
        true
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        let selection = cfg
            .index
            .find_symbol(&filter, parent_scope, children_scope, eph)
            .await?
            .into_inner();
        Ok(Some(selection))
    }
}

impl Display for GenericSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericSelector")
    }
}
