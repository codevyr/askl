use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::statement::Statement;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{ChildReference, ParentReference, Selection, SymbolSearchMixin};
use index::symbols::{DeclarationId, DeclarationRefs};
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

mod generic;
mod labels;
mod preamble;

pub use self::generic::{ChildrenVerb, NameSelector, UnitVerb};

use self::generic::{build_generic_verb, ForcedVerb};

pub fn build_verb(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<(), Error<Rule>> {
    let span = pair.as_span();
    debug!("Build verb {:#?}", pair);
    let verb = if let Some(verb) = pair.into_inner().next() {
        verb
    } else {
        return Err(Error::new_from_span(
            CustomError {
                message: format!("Expected a specific rule"),
            },
            span,
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
                NameSelector::new(&positional, &named)
            }
            Rule::forced_verb => {
                let ident = verb.into_inner().next().unwrap();
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".into(), ident.as_str().into());
                ForcedVerb::new(&positional, &named)
            }
            _ => unreachable!("Unknown rule: {:#?}", verb.as_rule()),
        }
        .map_err(|e| {
            Error::new_from_span(
                CustomError {
                    message: format!("Failed to create filter: {}", e),
                },
                span,
            )
        })?
    };

    let verb = ctx.consume(verb).map_err(|e| {
        Error::new_from_span(
            CustomError {
                message: format!("Failed to consume verb: {}", e),
            },
            span,
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

pub trait Verb: std::fmt::Debug + Sync {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        Ok(false)
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        bail!("Not a selector verb")
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        bail!("Not a filter verb")
    }

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        bail!("Not a deriver verb")
    }

    fn as_marker<'a>(&'a self) -> Result<&'a dyn Marker> {
        bail!("Not a marker verb")
    }
}

pub trait Filter: std::fmt::Debug {
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![]
    }

    fn filter(&self, cfg: &ControlFlowGraph, selection: &mut Selection) {
        let filter_name = format!("{:?}", self);
        let _filter = tracing::info_span!("filter", name = %filter_name).entered();
        self.filter_impl(cfg, selection);
    }

    fn filter_impl(&self, _cfg: &ControlFlowGraph, _selection: &mut Selection) {}
}

#[async_trait(?Send)]
pub trait Selector: std::fmt::Debug {
    async fn select_from_all(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Selection> {
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
    ) -> Result<Selection> {
        let mut search_mixins = search_mixins;
        cfg.index.find_symbol(&mut search_mixins).await
    }
}

#[async_trait(?Send)]
pub trait Deriver: std::fmt::Debug {
    async fn derive_children(
        &self,
        statement: &Statement,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        let derive_children_name = format!("{:?}", self);
        let _derive_children =
            tracing::info_span!("derive_children", name = %derive_children_name).entered();
        self.derive_children_impl(statement, ctx, cfg, children)
            .await
    }

    async fn derive_children_impl(
        &self,
        statement: &Statement,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children: &Vec<ChildReference>,
    ) -> Option<Selection>;

    async fn derive_parents(
        &self,
        ctx: &mut ExecutionContext,
        statement: &Statement,
        cfg: &ControlFlowGraph,
        parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        let derive_parents_name = format!("{:?}", self);
        let _derive_parents =
            tracing::info_span!("derive_parents", name = %derive_parents_name).entered();
        self.derive_parents_impl(ctx, statement, cfg, parents).await
    }

    async fn derive_parents_impl(
        &self,
        ctx: &mut ExecutionContext,
        statement: &Statement,
        cfg: &ControlFlowGraph,
        parents: &Vec<ParentReference>,
    ) -> Option<Selection>;

    fn constrain_references(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
        let constrain_references_name = format!("{:?}", self);
        let _constrain_references =
            tracing::info_span!("constrain_references", name = %constrain_references_name)
                .entered();
        self.constrain_references_impl(_cfg, selection)
    }

    fn constrain_references_impl(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
        let node_declaration_ids: HashSet<_> = selection
            .nodes
            .iter()
            .map(|s| DeclarationId::new(s.declaration.id))
            .collect();
        selection
            .parents
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.to_declaration.id)));
        selection
            .children
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.symbol_ref.from_decl)));
    }

    fn constrain_by_parents(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ChildReference>,
    ) {
        let constrain_by_parents_name = format!("{:?}", self);
        let _constrain_by_parents =
            tracing::info_span!("constrain_by_parents", name = %constrain_by_parents_name)
                .entered();
        self.constrain_by_parents_impl(cfg, selection, references)
    }

    fn constrain_by_parents_impl(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ChildReference>,
    ) {
        selection.nodes.retain(|s| {
            references
                .iter()
                .any(|r| r.declaration.id == s.declaration.id)
        });

        self.constrain_references(cfg, selection);
    }

    fn constrain_by_children(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ParentReference>,
    ) {
        let constrain_by_children_name = format!("{:?}", self);
        let _constrain_by_children =
            tracing::info_span!("constrain_by_children", name = %constrain_by_children_name)
                .entered();
        self.constrain_by_children_impl(cfg, selection, references)
    }

    fn constrain_by_children_impl(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ParentReference>,
    ) {
        selection.nodes.retain(|s| {
            references
                .iter()
                .any(|r| r.symbol_ref.from_decl == s.declaration.id)
        });

        self.constrain_references(cfg, selection);
    }
}

pub trait Marker: std::fmt::Debug {
    fn mark(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: &DeclarationRefs,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests;
