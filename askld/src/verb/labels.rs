use crate::verb::Args;
use crate::{
    execution_context::{selector_state_with, SelectorRegistry},
    execution_state::{DependencyKind, DependencyRole},
    parser::Rule,
    span::Span,
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::{
    db_diesel::{CompositeFilter, EphContext, Index, ParentReference, ScopeContext, Selection},
    models_diesel::SymbolRef,
};
use pest::error::ErrorVariant::CustomError;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, statement::Statement};

use super::{
    weak_notifier_blocks, ConstraintAction, DeriveMethod, Labeler, NotificationContext, Selector,
    SelectorState, Verb, VerbClass,
};
use crate::verb::Filter;

#[derive(Debug)]
pub(super) struct LabelVerb {
    span: Span,
    pub(super) label: String,
    inherit: bool,
}

impl LabelVerb {
    pub(super) const NAME: &'static str = "label";

    pub(super) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.allow(&["inherit"])?;

        let inherit = args.named_bool("inherit")?.unwrap_or(false);

        if args.no_positional() {
            bail!("Expected a positional argument");
        }
        Ok(Arc::new(Self {
            span,
            label: args.str_at(0, "label name")?.to_string(),
            inherit,
        }))
    }
}

impl Verb for LabelVerb {
    fn name(&self) -> &str {
        LabelVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Binding
    }

    fn derive_method(&self) -> DeriveMethod {
        if self.inherit {
            DeriveMethod::Clone
        } else {
            DeriveMethod::Skip
        }
    }

    fn as_labeler<'a>(&'a self) -> Option<&'a dyn Labeler> {
        Some(self)
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

    pub(super) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.allow(&["forced"])?;

        let forced = args.named_bool("forced")?.unwrap_or(false);

        if args.no_positional() {
            bail!("Expected a positional argument");
        }
        Ok(Arc::new(Self {
            span,
            label: args.str_at(0, "label name")?.to_string(),
            forced,
            selection: Arc::new(OnceLock::new()),
        }))
    }
}

impl Verb for UserVerb {
    fn name(&self) -> &str {
        UserVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Binding
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        Some(self)
    }
}

#[async_trait(?Send)]
impl Selector for UserVerb {
    fn dependency_kind(&self, role: DependencyRole) -> DependencyKind {
        match role {
            DependencyRole::User => DependencyKind::Necessary,
            _ => DependencyKind::Sufficient,
        }
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
        _cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
        _eph: &EphContext,
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
        _notif_ctx: &NotificationContext,
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

    /// Override that forwards through to the default — kept as an
    /// override only because of UserVerb's labelled-forced semantics
    /// (see body).
    fn try_constrain_notification(
        &self,
        registry: &mut SelectorRegistry,
        dependency: &Selection,
        notif_ctx: &NotificationContext,
        notifier: &Statement,
    ) -> Result<ConstraintAction, pest::error::Error<Rule>> {
        // The weakness rule holds here as well: a weak provider or parent may
        // seed this selector, but must not narrow one that has already
        // resolved.  This override used to ignore the notifier entirely and
        // so escaped the rule the default implementation applies.
        let has_selection = selector_state_with(registry, self, |state| state.selection.is_some());
        if weak_notifier_blocks(notifier.get_state().weak, has_selection) {
            return Ok(ConstraintAction::Skip);
        }

        // For forced parent dependencies, we always derive fake selection.
        if !self.forced || notif_ctx.role != DependencyRole::Child {
            let mut changed = false;
            let constrained = selector_state_with(registry, self, |state| {
                if state.selection.is_some() {
                    changed =
                        state.constrain_selection(dependency, &notif_ctx.role, notif_ctx.rel_type);
                    true
                } else {
                    false
                }
            });

            if constrained {
                return Ok(ConstraintAction::Constrained(changed, vec![]));
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
        Ok(ConstraintAction::Derive)
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
