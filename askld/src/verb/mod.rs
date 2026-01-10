use crate::cfg::ControlFlowGraph;
use crate::command::NotificationResult;
use crate::execution_context::{selector_state_with, ExecutionContext};
use crate::execution_state::DependencyRole;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{DeclarationIdMixin, Index, Selection, SymbolSearchMixin};
use index::symbols::DeclarationId;
use itertools::Itertools;
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

pub use self::generic::{NameSelector, UnitVerb};

use self::generic::{build_generic_verb, ForcedVerb};

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
    ModuleFilter,
    NameSelector,
    ChildrenSelector,
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

pub trait Verb: std::fmt::Debug + Sync {
    fn name(&self) -> &str;

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
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

    // Used to identify verb types for replacement
    fn get_tag(&self) -> Option<VerbTag> {
        None
    }

    fn is_unit(&self) -> bool {
        false
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
}

pub trait Filter: std::fmt::Debug + Display + Verb {
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![]
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

    pub fn constrain_selection(&mut self, dependency: &Selection, role: DependencyRole) -> bool {
        if self.selection.is_none() {
            return false;
        }

        let len_before = self.selection.as_ref().unwrap().nodes.len();

        if let Some(_) = &mut self.selection {
            match role {
                DependencyRole::Parent => {
                    self.constrain_by_child(dependency);
                }
                DependencyRole::Child => {
                    self.constrain_by_parent(dependency);
                }
                DependencyRole::User => {
                    self.constrain_by_owner(dependency);
                }
            }
            self.prune_references();
        }

        let len_after = self.selection.as_ref().unwrap().nodes.len();
        len_before != len_after
    }

    fn constrain_by_parent(&mut self, parent: &Selection) {
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|s| {
            parent
                .children
                .iter()
                .any(|r| r.declaration.id == s.declaration.id)
        });
    }

    fn constrain_by_child(&mut self, child: &Selection) {
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|s| {
            child
                .parents
                .iter()
                .any(|r| r.from_declaration.id == s.declaration.id)
        });
    }

    fn constrain_by_owner(&mut self, owner: &Selection) {
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|u| {
            owner
                .nodes
                .iter()
                .any(|o| o.declaration.id == u.declaration.id)
        });
    }

    fn prune_references(&mut self) {
        if let Some(selection) = &mut self.selection {
            selection.prune_references();
        }
    }
}

pub type SelectorId = usize;

#[async_trait(?Send)]
pub trait Selector: std::fmt::Debug + Verb {
    fn id(&self) -> SelectorId {
        ptr::from_ref(self) as *const () as SelectorId
    }

    fn get_label(&self) -> Option<String> {
        None
    }

    fn score(&self, state: &SelectorState) -> Option<usize> {
        state.selection.as_ref().map(|sel| sel.nodes.len())
    }

    fn dependency_ready(&self, _dependency_role: DependencyRole) -> bool {
        true
    }

    // Normally selectors do not update their state automatically.
    // They rely on notifications from statements they depend on.
    fn update_state(&self, _state: &mut SelectorState) {}

    fn get_selection_mut<'a>(&'a self, state: &'a mut SelectorState) -> Option<&'a mut Selection> {
        state.selection.as_mut()
    }

    fn get_selection<'a>(&'a self, state: &'a SelectorState) -> Option<&'a Selection> {
        state.selection.as_ref()
    }

    async fn select_from_all(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        let select_from_all_name = format!("{:?}", self);
        let _select_from_all =
            tracing::info_span!("select_from_all", name = %select_from_all_name).entered();
        self.select_from_all_impl(_ctx, cfg, search_mixins).await
    }

    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        let mut search_mixins = search_mixins;
        let selection = cfg.index.find_symbol(&mut search_mixins).await?;
        Ok(Some(selection))
    }

    /// Accept a notification from a statement that this selector should update its selection
    /// based on the statement's selection.
    ///
    /// Returns Ok(true) if the selection was updated, Ok(false) if not.
    async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        notifier: &Statement,
        role: DependencyRole,
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        if !notifier.command().has_selectors() {
            return Ok(NotificationResult::new(false, vec![]));
        }

        let dependency = match notifier.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(NotificationResult::new(false, vec![])),
        };

        if role == DependencyRole::User {
            let notifier_labels = notifier.command().get_labels();
            let self_label = match self.get_label() {
                Some(label) => label,
                None => return Ok(NotificationResult::new(false, vec![])),
            };
            if !notifier_labels.contains(&self_label) {
                return Ok(NotificationResult::new(false, vec![]));
            }
        }

        // Weak statements do not constrain the selection of their dependencies.
        if notifier.get_state().weak {
            let state_exists =
                selector_state_with(&mut ctx.registry, self, |state| state.selection.is_some());
            if state_exists {
                return Ok(NotificationResult::new(false, vec![]));
            }
        }

        let mut changed = false;
        let (constrained, warnings) = selector_state_with(&mut ctx.registry, self, |state| {
            if state.selection.is_some() {
                changed = state.constrain_selection(&dependency, role);
                let mut warnings = vec![];
                if changed && state.selection.as_ref().unwrap().nodes.is_empty() {
                    warnings.push(Error::new_from_span(
                        CustomError {
                            message: format!(
                                "Statement did not match any symbols after applying constraints from {}.",
                                notifier.command().span(),
                            ),
                        },
                        self.span(),
                    ));
                }
                (true, warnings)
            } else {
                (false, vec![])
            }
        });

        if constrained {
            return Ok(NotificationResult::new(changed, warnings));
        }

        let mut selection = self
            .derive_selection(ctx, index, selector_filters, notifier, role)
            .await
            .map_err(|e| {
                Error::new_from_span(
                    CustomError {
                        message: format!("Failed to derive selection: {}", e),
                    },
                    self.span(),
                )
            })?;

        if let Some(ref mut selection) = selection {
            selector_filters.iter().for_each(|f| {
                f.filter(selection);
            });
        }

        selector_state_with(&mut ctx.registry, self, |state| {
            state.selection = selection;
            state.prune_references();
        });
        Ok(NotificationResult::new(true, vec![]))
    }

    async fn derive_selection(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        notifier: &Statement,
        role: DependencyRole,
    ) -> Result<Option<Selection>> {
        let selection = match role {
            DependencyRole::Child => {
                self.derive_from_parent(ctx, index, selector_filters, notifier)
                    .await?
            }
            DependencyRole::Parent => {
                self.derive_from_child(ctx, index, selector_filters, notifier)
                    .await?
            }
            DependencyRole::User => {
                self.derive_from_provider(ctx, index, selector_filters, notifier)
                    .await?
            }
        };

        Ok(selection)
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        parent: &Statement,
    ) -> Result<Option<Selection>> {
        let parent = match parent.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let decl_ids = parent
            .children
            .iter()
            .map(|p| DeclarationId::new(p.declaration.id))
            .unique()
            .collect::<Vec<_>>();

        let children_selection = self
            .find_symbol_by_declid(index, selector_filters, &decl_ids)
            .await?;

        Ok(Some(children_selection))
    }

    async fn derive_from_child(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        child: &Statement,
    ) -> Result<Option<Selection>> {
        let child = match child.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let decl_ids = child
            .parents
            .iter()
            .map(|p| DeclarationId::new(p.from_declaration.id))
            .unique()
            .collect::<Vec<_>>();
        let parent_selection = self
            .find_symbol_by_declid(index, selector_filters, &decl_ids)
            .await?;

        Ok(Some(parent_selection))
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

    async fn find_symbol_by_declid(
        &self,
        index: &Index,
        selector_filters: &[&dyn Filter],
        declarations: &Vec<DeclarationId>,
    ) -> Result<Selection> {
        let mixin = DeclarationIdMixin::new(declarations);
        let mut mixins: Vec<Box<dyn SymbolSearchMixin>> = selector_filters
            .iter()
            .flat_map(|f| f.get_filter_mixins())
            .collect();
        mixins.push(Box::new(mixin));
        index.find_symbol(&mut mixins).await
    }
}

pub trait Labeler: std::fmt::Debug {
    fn get_label(&self) -> Option<String>;
}

#[cfg(test)]
mod tests;
