use crate::{execution_context::selector_state_with, execution_state::DependencyRole};
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::{
    db_diesel::{Index, ParentReference, Selection, SymbolSearchMixin},
    models_diesel::SymbolRef,
};
use std::fmt::Display;
use std::sync::Arc;
use std::{collections::HashMap, sync::OnceLock};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, statement::Statement};

use super::{DeriveMethod, Labeler, Selector, SelectorState, Verb};
use crate::verb::Filter;

#[derive(Debug)]
pub(super) struct LabelVerb {
    pub(super) label: String,
}

impl LabelVerb {
    pub(super) const NAME: &'static str = "label";

    pub(super) fn new(
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if !named.is_empty() {
            bail!("Unexpected named arguments");
        }

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                label: label.clone(),
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for LabelVerb {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
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
    pub(super) label: String,
    pub(super) forced: bool,

    selection: Arc<OnceLock<Selection>>,
}

impl UserVerb {
    pub(super) const NAME: &'static str = "use";

    pub(super) fn new(
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
                    to_declaration: child_node.declaration.clone(),
                    from_declaration: parent_node.declaration.clone(),
                    symbol_ref: SymbolRef {
                        rowid: 0,
                        from_decl: parent_node.declaration.id,
                        to_symbol: child_node.symbol.id,
                        from_file: parent_node.file.id.into(),
                        from_line: parent_node.declaration.line_start as i32,
                        from_col_start: parent_node.declaration.col_start as i32,
                        from_col_end: parent_node.declaration.col_end as i32,
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
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        child: &Statement,
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

        cached_selection.nodes.retain(|s| {
            child
                .parents
                .iter()
                .any(|p| p.to_declaration.id == s.declaration.id)
        });

        Ok(Some(cached_selection))
    }

    async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        notifier: &Statement,
        role: DependencyRole,
    ) -> Result<bool> {
        if !notifier.command().has_selectors() {
            return Ok(false);
        }

        let dependency = match notifier.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(false),
        };

        if role == DependencyRole::User {
            let notifier_labels = notifier.command().get_labels();
            let self_label = match self.get_label() {
                Some(label) => label,
                None => return Ok(false),
            };
            if !notifier_labels.contains(&self_label) {
                return Ok(false);
            }
        }

        // For forced parent dependencies, we always derive fake selection.
        if !self.forced || role != DependencyRole::Child {
            let mut changed = false;
            let constrained = selector_state_with(&mut ctx.registry, self, |state| {
                if state.selection.is_some() {
                    changed = state.constrain_selection(&dependency, role);
                    true
                } else {
                    false
                }
            });

            if constrained {
                return Ok(changed);
            }
        }

        let mut selection = self
            .derive_selection(ctx, index, selector_filters, notifier, role)
            .await?;

        if let Some(ref mut selection) = selection {
            selector_filters.iter().for_each(|f| {
                f.filter(selection);
            });
        }

        selector_state_with(&mut ctx.registry, self, |state| {
            state.selection = selection;
            state.prune_references();
        });
        Ok(true)
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
