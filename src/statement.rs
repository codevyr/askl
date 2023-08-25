use crate::parser::Rule;
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::verb::{build_verb, UnitVerb, Verb};
use core::fmt::Debug;
use pest::error::Error;

#[derive(Debug)]
pub struct DefaultStatement {
    pub verb: Box<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

pub fn build_statement<'a>(
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut verb: Box<dyn Verb> = UnitVerb::new();
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    for pair in pair.into_inner() {
        match pair.as_rule() {
            Rule::verb => {
                verb = build_verb(verb, pair)?;
            }
            Rule::scope => {
                scope = build_scope(pair)?;
            }
            _ => Err(Error::new_from_span(
                pest::error::ErrorVariant::ParsingError {
                    positives: vec![Rule::verb, Rule::scope],
                    negatives: vec![pair.as_rule()],
                },
                pair.as_span(),
            ))?,
        }
    }

    Ok(Box::new(DefaultStatement {
        verb: verb,
        scope: scope,
    }))
}

pub trait Statement: Debug {
    fn verb(&self) -> &dyn Verb;
    fn scope(&self) -> &dyn Scope;
}

impl Statement for DefaultStatement {
    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
