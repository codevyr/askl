use crate::verb::Args;
use crate::{parser_context::ParserContext, span::Span};
use anyhow::{bail, Result};
use std::sync::Arc;

use super::{DeriveMethod, Verb, VerbClass};

#[derive(Debug)]
pub(super) struct PreambleVerb {
    span: Span,
}

impl PreambleVerb {
    pub(super) const NAME: &'static str = "preamble";

    pub(super) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for PreambleVerb {
    fn name(&self) -> &str {
        PreambleVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Env
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    /// Redirect this statement's verbs to the global context, so they apply
    /// to every statement in the query.
    ///
    /// Two shapes cannot mean anything and are refused here.  `ctx` is the
    /// context [`ParserContext::consume`] picked: once a preamble is in
    /// effect that is the global context, and the global context is the only
    /// one with no predecessor — so no predecessor means this `preamble` sits
    /// inside another.  Otherwise a top-level statement's context hangs
    /// directly off the global one, and anything deeper is nested in a scope,
    /// where "applies to every statement" has no reading.
    ///
    /// Position among the top-level statements is NOT constrained: a query
    /// may open several preambles, and each applies to the statements that
    /// follow it.
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        let Some(prev) = ctx.get_prev() else {
            bail!(
                "`preamble` cannot appear inside another `preamble`: the outer one \
                 already applies to every statement in the query, so there is nothing \
                 for a second one to configure"
            );
        };

        if prev.upgrade().unwrap().get_prev().is_some() {
            bail!(
                "`preamble` must be a top-level statement: its verbs apply to every \
                 statement in the query, so it has no reading inside a scope.  Move it \
                 out to the top level — a preamble may appear at any position there, \
                 and configures the statements that follow it"
            );
        }

        ctx.set_alternative_context(prev);

        Ok(true)
    }
}
