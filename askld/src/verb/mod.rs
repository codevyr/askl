use crate::cfg::ControlFlowGraph;
use crate::execution_context::{selector_state_with, ExecutionContext, SelectorRegistry};
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, DirectOnlyMixin, InnermostOnlyMixin, OuterParentFilterMixin,
    ScopeContext, SymbolInstanceIdMixin, Index, Selection,
};
use index::symbols::SymbolInstanceId;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;
use std::fmt::Display;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;

mod generic;
mod labels;
mod preamble;

pub use self::generic::{DefaultTypeFilter, DirectOnlyFilter, GenericFilter, GenericSelector, NameSelector, UnitVerb};

use self::generic::{build_generic_verb, ForcedVerb};
use self::labels::{LabelVerb, UserVerb};

pub fn build_verb(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<(), Error<Rule>> {
    let verb_span = Span::from_pest(pair.as_span(), ctx.source());
    debug!("Build verb {:#?}", pair);
    let verb = if let Some(verb) = pair.into_inner().next() {
        verb
    } else {
        return Err(Error::new_from_span(
            CustomError {
                message: format!("Expected a specific rule"),
            },
            verb_span.as_pest_span(),
        ));
    };

    let verb = if let Rule::generic_verb = verb.as_rule() {
        build_generic_verb(ctx.clone(), verb)?
    } else {
        match verb.as_rule() {
            Rule::label_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let positional = vec![label_ident.as_str().to_string()];
                LabelVerb::new(verb_span.clone(), &positional, &HashMap::new())
            }
            Rule::inherit_label_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let positional = vec![label_ident.as_str().to_string()];
                let mut named = HashMap::new();
                named.insert("inherit".to_string(), "true".to_string());
                LabelVerb::new(verb_span.clone(), &positional, &named)
            }
            Rule::use_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let positional = vec![label_ident.as_str().to_string()];
                UserVerb::new(verb_span.clone(), &positional, &HashMap::new())
            }
            Rule::plain_filter => {
                let ident = verb.into_inner().next().unwrap();
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".into(), ident.as_str().into());
                NameSelector::new(verb_span.clone(), &positional, &named)
            }
            Rule::forced_verb => {
                let ident = verb.into_inner().next().unwrap();
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".into(), ident.as_str().into());
                ForcedVerb::new(verb_span.clone(), &positional, &named)
            }
            _ => {
                return Err(Error::new_from_span(
                    pest::error::ErrorVariant::ParsingError {
                        positives: vec![Rule::generic_verb, Rule::plain_filter, Rule::forced_verb],
                        negatives: vec![verb.as_rule()],
                    },
                    verb_span.as_pest_span(),
                ))
            }
        }
        .map_err(|e| {
            Error::new_from_span(
                CustomError {
                    message: format!("Failed to create filter: {}", e),
                },
                verb_span.as_pest_span(),
            )
        })?
    };

    let verb = ctx.consume(verb).map_err(|e| {
        Error::new_from_span(
            CustomError {
                message: format!("Failed to consume verb: {}", e),
            },
            verb_span.as_pest_span(),
        )
    })?;

    if let Some(verb) = verb {
        ctx.extend_verb(verb)
    };

    Ok(())
}

pub fn derive_verb(verb: &Arc<dyn Verb>) -> Option<Arc<dyn Verb>> {
    match verb.derive_method() {
        DeriveMethod::Clone => Some(verb.clone()),
        DeriveMethod::Skip => None,
    }
}

pub enum DeriveMethod {
    Clone,
    Skip,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerbTag {
    ProjectFilter,
    NameSelector,
    TypeFilter,
    GenericFilter(&'static str),
    GenericSelector,
    Unnest,
}

/// Bundles the notification parameters that always travel together through
/// the notification chain: dependency role, resolved relationship type, and
/// whether the receiver uses unnest mode.
#[derive(Debug, Clone, Copy)]
pub struct NotificationContext {
    pub role: DependencyRole,
    pub rel_type: RelationshipType,
    pub unnest: bool,
}

pub fn add_verb(existing_verbs: Vec<Arc<dyn Verb>>, new_verb: Arc<dyn Verb>) -> Vec<Arc<dyn Verb>> {
    let mut verbs = existing_verbs;
    verbs.push(new_verb);

    let mut updated_verbs = vec![];
    for verb in verbs.into_iter() {
        updated_verbs = verb.add_verb(updated_verbs);
        updated_verbs.push(verb);
    }

    updated_verbs
}

pub trait Verb: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    /// Create a new instance of this verb for derivation into child scopes.
    fn derive_new_instance(&self) -> Option<Arc<dyn Verb>> {
        None
    }

    fn extend_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        existing_verbs
    }

    fn replace_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        existing_verbs
            .into_iter()
            .filter(|v| self.get_tag() != v.get_tag())
            .collect()
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        self.extend_verb(existing_verbs)
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        Ok(false)
    }

    fn get_tag(&self) -> Option<VerbTag> {
        None
    }

    fn is_unit(&self) -> bool {
        false
    }

    fn is_non_constraining_selector(&self) -> bool {
        self.is_unit()
    }

    fn span(&self) -> pest::Span<'_> {
        panic!("Verb does not have a span")
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        bail!("Not a selector verb")
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        bail!("Not a filter verb")
    }

    fn as_labeler<'a>(&'a self) -> Result<&'a dyn Labeler> {
        bail!("Not a marker verb")
    }

    fn suppresses_default_type_filter(&self) -> bool {
        false
    }

    fn requires_name_constraint(&self) -> bool {
        false
    }

    fn has_name_constraint(&self) -> bool {
        false
    }

}

/// Filter trait for verbs that constrain symbol selection.
///
/// Two filtering stages:
/// - `get_composite_filter()` — returns a `CompositeFilter` tree compiled into
///   SQL WHERE clauses. This is the primary filtering mechanism.
/// - `filter_impl()` — optional in-memory post-filter on the returned `Selection`.
///   Use only when SQL cannot express the constraint (e.g., application-level logic).
///   When both are implemented, the SQL filter should be at least as broad as the
///   in-memory filter (it pre-filters, the in-memory path refines).
pub trait Filter: std::fmt::Debug + Display + Verb {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        None
    }

    fn filter(&self, selection: &mut Selection) {
        let filter_name = format!("{}", self);
        let _filter = tracing::info_span!("filter", name = %filter_name).entered();
        self.filter_impl(selection);
    }

    fn filter_impl(&self, _selection: &mut Selection) {}
}

#[derive(Debug)]
pub struct SelectorState {
    pub selection: Option<Selection>,
}

impl SelectorState {
    pub fn new() -> Self {
        Self { selection: None }
    }

    pub fn constrain_selection(
        &mut self,
        dependency: &Selection,
        role: DependencyRole,
        rel_type: RelationshipType,
    ) -> bool {
        if self.selection.is_none() {
            return false;
        }

        let len_before = self.selection.as_ref().unwrap().nodes.len();

        if let Some(_) = &mut self.selection {
            match role {
                DependencyRole::Parent => {
                    self.constrain_by_child(dependency, rel_type);
                }
                DependencyRole::Child => {
                    self.constrain_by_parent(dependency, rel_type);
                }
                DependencyRole::User => {
                    self.constrain_by_owner(dependency);
                }
            }
        }

        let len_after = self.selection.as_ref().unwrap().nodes.len();
        len_before != len_after
    }

    fn constrain_by_parent(&mut self, parent: &Selection, rel_type: RelationshipType) {
        let parent_node_ids: std::collections::HashSet<_> =
            parent.nodes.iter().map(|n| n.symbol_instance.id).collect();
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|s| {
            (rel_type.contains(RelationshipType::REFS)
                && parent.children.iter().any(|r| {
                    r.symbol_instance.id == s.symbol_instance.id
                        && parent_node_ids.contains(&r.from_instance.id)
                }))
                || (rel_type.contains(RelationshipType::HAS)
                    && parent.has_children.iter().any(|r| {
                        r.child_instance.id == s.symbol_instance.id
                            && parent_node_ids.contains(&r.parent_instance.id)
                    }))
        });
    }

    fn constrain_by_child(&mut self, child: &Selection, rel_type: RelationshipType) {
        let child_node_ids: std::collections::HashSet<_> =
            child.nodes.iter().map(|n| n.symbol_instance.id).collect();
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|s| {
            (rel_type.contains(RelationshipType::REFS)
                && child.parents.iter().any(|r| {
                    r.from_instance.id == s.symbol_instance.id
                        && child_node_ids.contains(&r.to_instance.id)
                }))
                || (rel_type.contains(RelationshipType::HAS)
                    && child.has_parents.iter().any(|r| {
                        r.parent_instance.id == s.symbol_instance.id
                            && child_node_ids.contains(&r.child_instance.id)
                    }))
        });
    }

    /// Constrain selection and produce a warning if the result is empty.
    /// Returns `(constrained, changed, warnings)` where `constrained` means a selection
    /// existed to constrain, `changed` means it was actually narrowed.
    pub fn constrain_with_warning(
        &mut self,
        dependency: &Selection,
        role: DependencyRole,
        rel_type: RelationshipType,
        span: pest::Span<'_>,
        context: &str,
    ) -> (bool, bool, Vec<Error<Rule>>) {
        if self.selection.is_none() {
            return (false, false, vec![]);
        }
        let changed = self.constrain_selection(dependency, role, rel_type);
        let mut warnings = vec![];
        if changed && self.selection.as_ref().unwrap().nodes.is_empty() {
            warnings.push(Error::new_from_span(
                CustomError {
                    message: format!(
                        "Statement did not match any symbols after applying constraints from {}.",
                        context,
                    ),
                },
                span,
            ));
        }
        (true, changed, warnings)
    }

    fn constrain_by_owner(&mut self, owner: &Selection) {
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|u| {
            owner
                .nodes
                .iter()
                .any(|o| o.symbol_instance.id == u.symbol_instance.id)
        });
    }

}

pub type SelectorId = usize;

/// Result of per-selector constraint logic in `try_constrain_notification`.
pub enum ConstraintAction {
    /// Selector should be skipped (e.g., weak statement already has selection).
    Skip,
    /// Selection was constrained. Contains (changed, warnings).
    Constrained(bool, Vec<pest::error::Error<Rule>>),
    /// No existing selection to constrain; proceed to derive.
    Derive,
}

#[async_trait(?Send)]
pub trait Selector: std::fmt::Debug + Verb {
    fn id(&self) -> SelectorId {
        ptr::from_ref(self) as *const () as SelectorId
    }

    fn get_label(&self) -> Option<String> {
        None
    }

    /// Build a composite filter representing this selector's filter criteria.
    /// Used by scope builders to construct ScopeContext for parent/children scoping.
    /// Default: `None` (no scope filter — scope is unscoped). Override in selectors
    /// that should contribute to scope narrowing.
    fn build_composite_filter(&self, _command: &crate::command::Command) -> Option<CompositeFilter> {
        None
    }

    fn score(&self, state: &SelectorState) -> Option<usize> {
        state.selection.as_ref().map(|sel| sel.nodes.len())
    }

    fn dependency_ready(&self, _dependency_role: DependencyRole) -> bool {
        true
    }

    fn update_state(&self, _state: &mut SelectorState) {}

    fn get_selection_mut<'a>(&'a self, state: &'a mut SelectorState) -> Option<&'a mut Selection> {
        state.selection.as_mut()
    }

    fn get_selection<'a>(&'a self, state: &'a SelectorState) -> Option<&'a Selection> {
        state.selection.as_ref()
    }

    /// Per-selector constraint logic for `accept_notification`.
    /// Returns whether to skip, constrain, or derive for this notification.
    /// Override to customize constraint behavior (e.g. UserVerb's forced/circular logic).
    fn try_constrain_notification(
        &self,
        registry: &mut SelectorRegistry,
        dependency: &Selection,
        notif_ctx: NotificationContext,
        notifier: &Statement,
    ) -> Result<ConstraintAction, pest::error::Error<Rule>> {
        // Weak statements do not constrain the selection of their dependencies.
        if notifier.get_state().weak {
            let state_exists =
                selector_state_with(registry, self, |state| state.selection.is_some());
            if state_exists {
                return Ok(ConstraintAction::Skip);
            }
        }

        let span = self.span();
        let context = format!("{}", notifier.command().span());
        let (constrained, changed, warnings) = selector_state_with(registry, self, |state| {
            state.constrain_with_warning(dependency, notif_ctx.role, notif_ctx.rel_type, span, &context)
        });

        if constrained {
            Ok(ConstraintAction::Constrained(changed, warnings))
        } else {
            Ok(ConstraintAction::Derive)
        }
    }

    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let selection = cfg.index.find_symbol(&filter, parent_scope, children_scope).await?;
        Ok(Some(selection))
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        parent: &Statement,
        notif_ctx: NotificationContext,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let parent_sel = match parent.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let parent_ids = parent_sel.get_instance_ids();
        let mut find_parts: Vec<CompositeFilter> = vec![];
        if !notif_ctx.unnest {
            find_parts.push(CompositeFilter::leaf(DirectOnlyMixin));
            find_parts.push(CompositeFilter::leaf(OuterParentFilterMixin::new(&parent_ids)));
        }
        let find_filter = CompositeFilter::and(find_parts);
        let decl_ids = index.find_child_instance_ids(
            &parent_ids,
            notif_ctx.rel_type.contains(RelationshipType::REFS),
            notif_ctx.rel_type.contains(RelationshipType::HAS),
            &find_filter,
        ).await.map_err(|e| anyhow::anyhow!("Failed to find child instance IDs: {}", e))?;

        let selection = find_symbol_by_instance_id(index, selector_filters, &decl_ids, parent_scope, children_scope)
            .await?;

        Ok(Some(selection))
    }

    async fn derive_from_child(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        child: &Statement,
        notif_ctx: NotificationContext,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let child_sel = match child.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let child_ids = child_sel.get_instance_ids();
        let mut find_parts: Vec<CompositeFilter> = vec![];
        if !notif_ctx.unnest {
            find_parts.push(CompositeFilter::leaf(InnermostOnlyMixin));
        }
        let find_filter = CompositeFilter::and(find_parts);
        let decl_ids = index.find_parent_instance_ids(
            &child_ids,
            notif_ctx.rel_type.contains(RelationshipType::REFS),
            notif_ctx.rel_type.contains(RelationshipType::HAS),
            &find_filter,
        ).await.map_err(|e| anyhow::anyhow!("Failed to find parent instance IDs: {}", e))?;

        let selection = find_symbol_by_instance_id(index, selector_filters, &decl_ids, parent_scope, children_scope)
            .await?;

        Ok(Some(selection))
    }

    async fn derive_from_provider(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        provider: &Statement,
    ) -> Result<Option<Selection>> {
        let provider = match provider.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        Ok(Some(provider.clone()))
    }

}

pub(crate) async fn find_symbol_by_instance_id(
    index: &Index,
    selector_filters: &[&dyn Filter],
    instances: &Vec<SymbolInstanceId>,
    parent_scope: ScopeContext,
    children_scope: ScopeContext,
) -> Result<Selection> {
    let mut parts: Vec<CompositeFilter> = selector_filters
        .iter()
        .filter_map(|f| f.get_composite_filter())
        .collect();
    parts.push(CompositeFilter::leaf(SymbolInstanceIdMixin::new(instances)));
    let filter = CompositeFilter::and(parts);
    index.find_symbol(&filter, parent_scope, children_scope).await
}

pub trait Labeler: std::fmt::Debug {
    fn get_label(&self) -> Option<String>;
}

#[cfg(test)]
mod tests;
