use crate::{parser::Value, parser_context::ParserContext, span::Span};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::Arc;

use super::{DeriveMethod, Verb, VerbClass};

#[derive(Debug)]
pub(super) struct PreambleVerb {
    span: Span,
}

impl PreambleVerb {
    pub(super) const NAME: &'static str = "preamble";

    pub(super) fn new(
        span: Span,
        positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        if !positional.is_empty() {
            bail!("Unexpected positional arguments");
        };

        if !named.is_empty() {
            bail!("Unexpected named arguments");
        };

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

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        if ctx.get_prev().is_none() {
            panic!("Expected to have global context");
        }

        let prev = ctx.get_prev().unwrap();
        if let Some(_) = prev.upgrade().unwrap().get_prev() {
            bail!("Preamble verb can only be used as the first verb statement in the askl code");
        }

        ctx.set_alternative_context(prev);

        Ok(true)
    }
}
