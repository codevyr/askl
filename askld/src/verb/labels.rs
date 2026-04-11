use crate::{
    command::NotificationResult, execution_context::selector_state_with,
    execution_state::{DependencyRole, RelationshipType}, parser::Rule, span::Span,
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::{
    db_diesel::{InnermostOnlyMixin, Index, ParentReference, ScopeContext, Selection, SymbolSearchMixin},
    models_diesel::SymbolRef,
};
use pest::error::ErrorVariant::CustomError;
use std::fmt::Display;
use std::sync::Arc;
use std::{collections::HashMap, sync::OnceLock};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, statement::Statement};

use super::{DeriveMethod, Labeler, NotificationContext, Selector, SelectorState, Verb};
use crate::verb::Filter;

#[derive(Debug)]
pub(super) struct LabelVerb {
    span: Span,
    pub(super) label: String,
    inherit: bool,
}

impl LabelVerb {
    pub(super) const NAME: &'static str = "label";

    pub(super) fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let inherit = if let Some(val) = named.get("inherit") {
            match val.as_str() {
                "true" => true,
                "false" => false,
                other => bail!("Unexpected value for inherit parameter: {}", other),
            }
        } else {
            false
        };

        for key in named.keys() {
            if key != "inherit" {
                bail!("Unexpected named argument: {}", key);
            }
        }

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                span,
                label: label.clone(),
                inherit,
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for LabelVerb {
    fn name(&self) -> &str {
        LabelVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn derive_method(&self) -> DeriveMethod {
        if self.inherit {
            DeriveMethod::Clone
        } else {
            DeriveMethod::Skip
        }
    }

    fn as_labeler<'a>(&'a self) -> Result<&'a dyn Labeler> {
        Ok(self)
    }
}

impl Labeler for LabelVerb {
    fn get_label(&self) -> Option<String> {
        Some(self.label.clone())
    }
}

#[derive(Debug)]
pub(super) struct UserVerb {
    span: Span,
    pub(super) label: String,
    pub(super) forced: bool,

    selection: Arc<OnceLock<Selection>>,
}

impl UserVerb {
    pub(super) const NAME: &'static str = "use";

    pub(super) fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let forced = if let Some(forced) = named.get("forced") {
            if forced == "true" {
                true
            } else if forced == "false" {
                false
            } else {
                bail!("Unexpected value for forced parameter")
            }
        } else {
            false
        };

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                span,
                label: label.clone(),
                forced,
                selection: Arc::new(OnceLock::new()),
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for UserVerb {
    fn name(&self) -> &str {
        UserVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for UserVerb {
    fn dependency_ready(&self, _dependency_role: DependencyRole) -> bool {
        if !self.forced {
            return true;
        }

        self.selection.get().is_some()
    }

    fn score(&self, state: &SelectorState) -> Option<usize> {
        if state.selection.is_some() {
            return Some(state.selection.as_ref().unwrap().nodes.len());
        }

        if self.selection.get().is_none() {
            return None;
        }
        self.selection.get().map(|s| s.nodes.len())
    }

    fn update_state(&self, state: &mut SelectorState) {
        if self.forced {
            if state.selection.is_none() && self.selection.get().is_some() {
                state.selection = self.selection.get().cloned();
            }
        }
    }

    fn get_selection_mut<'a>(&'a self, state: &'a mut SelectorState) -> Option<&'a mut Selection> {
        if !self.forced {
            return state.selection.as_mut();
        }

        if state.selection.is_none() {
            return None;
        }

        state.selection.as_mut()
    }

    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        Ok(None)
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

        // If not forced, just return the provider's selection as is.
        if !self.forced {
            return Ok(Some(provider));
        }

        // Otherwise, need to store the selector state until notification from the parent.
        let _ = self.selection.set(provider.clone());

        return Ok(None);
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        parent: &Statement,
        _notif_ctx: NotificationContext,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        if !self.forced {
            bail!("Cannot derive from parent when not forced");
        }

        let parent_selection = match parent.get_selection(ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };

        let cached_selection = self.selection.get().cloned();

        let mut normal_selection = match cached_selection {
            Some(selection) => selection,
            None => {
                println!(
                    "UserVerb: No symbols found with label {}",
                    self.label.as_str()
                );
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

    async fn derive_from_child(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        _selector_filters: &[&dyn Filter],
        child: &Statement,
        notif_ctx: NotificationContext,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        let child = match child.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };

        let mut cached_selection = if let Some(sel) = self.selection.get().cloned() {
            sel
        } else {
            return Ok(Some(Selection::new()));
        };

        // Use DB query instead of stale child.parents vector
        let child_ids = child.get_instance_ids();
        let mut find_mixins: Vec<Box<dyn SymbolSearchMixin>> = vec![];
        if !notif_ctx.unnest {
            find_mixins.push(Box::new(InnermostOnlyMixin));
        }
        let parent_ids = index.find_parent_instance_ids(
            &child_ids,
            notif_ctx.rel_type.contains(RelationshipType::REFS),
            notif_ctx.rel_type.contains(RelationshipType::HAS),
            &mut find_mixins,
        ).await.map_err(|e| anyhow::anyhow!("Failed to find parent instance IDs: {}", e))?;
        let parent_id_set: std::collections::HashSet<i32> =
            parent_ids.into_iter().map(Into::<i32>::into).collect();

        cached_selection.nodes.retain(|s|
            parent_id_set.contains(&s.symbol_instance.id)
        );

        Ok(Some(cached_selection))
    }

    async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        notifier: &Statement,
        notif_ctx: NotificationContext,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        if !notifier.command().has_selectors() {
            return Ok(NotificationResult::new(false, vec![]));
        }

        let dependency = match notifier.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(NotificationResult::new(false, vec![])),
        };

        if notif_ctx.role == DependencyRole::User {
            let notifier_labels = notifier.command().get_labels();
            let self_label = match self.get_label() {
                Some(label) => label,
                None => return Ok(NotificationResult::new(false, vec![])),
            };
            if !notifier_labels.contains(&self_label) {
                return Ok(NotificationResult::new(false, vec![]));
            }
        }

        // For forced parent dependencies, we always derive fake selection.
        if !self.forced || notif_ctx.role != DependencyRole::Child {
            let mut changed = false;
            let constrained = selector_state_with(&mut ctx.registry, self, |state| {
                if state.selection.is_some() {
                    changed = state.constrain_selection(&dependency, notif_ctx.role, notif_ctx.rel_type);
                    true
                } else {
                    false
                }
            });

            if constrained {
                return Ok(NotificationResult::new(changed, vec![]));
            }

            if notif_ctx.role == DependencyRole::Child {
                return Err(pest::error::Error::new_from_span(
                    CustomError {
                        message: format!(
                            "Use verb '{}' is not resolvable because of a circular dependency.",
                            self.label
                        ),
                    },
                    self.span.as_pest_span(),
                ));
            }
        }

        let mut selection = self
            .derive_selection(ctx, index, selector_filters, notifier, notif_ctx, parent_scope, children_scope)
            .await
            .map_err(|e| {
                pest::error::Error::new_from_span(
                    CustomError {
                        message: format!("Error deriving selection for user verb: {}", e),
                    },
                    self.span.as_pest_span(),
                )
            })?;

        if let Some(ref mut selection) = selection {
            selector_filters.iter().for_each(|f| {
                f.filter(selection);
            });
        }

        selector_state_with(&mut ctx.registry, self, |state| {
            state.selection = selection;
        });
        Ok(NotificationResult::new(true, vec![]))
    }

    fn get_label(&self) -> Option<String> {
        Some(self.label.clone())
    }
}

impl Display for UserVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UserVerb(label={}, forced={})", self.label, self.forced)
    }
}
