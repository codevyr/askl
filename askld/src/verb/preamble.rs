use crate::parser_context::ParserContext;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::Arc;

use super::{DeriveMethod, Verb};

#[derive(Debug)]
pub(super) struct PreambleVerb {}

impl PreambleVerb {
    pub(super) const NAME: &'static str = "preamble";

    pub(super) fn new(
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if !positional.is_empty() {
            bail!("Unexpected positional arguments");
        };

        if !named.is_empty() {
            bail!("Unexpected named arguments");
        };

        Ok(Arc::new(Self {}))
    }
}

impl Verb for PreambleVerb {
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
