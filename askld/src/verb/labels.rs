use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::statement::Statement;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{ChildReference, ParentReference, Selection, SymbolSearchMixin};
use index::symbols::DeclarationRefs;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::sync::Arc;

use super::{DeriveMethod, Deriver, Marker, Selector, Verb};

#[derive(Debug)]
pub(super) struct LabellerVerb {
    pub(super) label: String,
}

impl LabellerVerb {
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

impl Verb for LabellerVerb {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_marker<'a>(&'a self) -> Result<&'a dyn Marker> {
        Ok(self)
    }
}

impl Marker for LabellerVerb {
    fn mark(
        &self,
        ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        declarations: &DeclarationRefs,
    ) -> Result<()> {
        let ids: HashSet<_> = declarations.iter().map(|(id, _)| *id).collect();

        if ctx.saved_labels.contains_key(&self.label) {
            bail!("Label {} already exists", self.label);
        }

        ctx.saved_labels.insert(self.label.clone(), ids);

        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct UserVerb {
    pub(super) label: String,
    pub(super) forced: bool,
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
            true
        };

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                label: label.clone(),
                forced,
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

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Deriver for UserVerb {
    async fn derive_children_impl(
        &self,
        _statement: &Statement,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        unimplemented!("UserVerb does not support derive_children");
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        _cfg: &ControlFlowGraph,
        _parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        unimplemented!("UserVerb does not support derive_parents");
    }
}

#[async_trait(?Send)]
impl Selector for UserVerb {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Selection> {
        unimplemented!("UserVerb does not support select_from_all");
    }
}

impl Display for UserVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UserVerb(label={}, forced={})", self.label, self.forced)
    }
}
