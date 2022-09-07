use crate::cfg::ControlFlowGraph;
use crate::parser::Rule;
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::verb::{build_verb, Verb};
use core::fmt::Debug;
use pest::error::Error;

#[derive(Debug)]
pub struct DefaultStatement {
    pub verbs: Vec<Box<dyn Verb>>,
    pub scope: Box<dyn Scope>,
}

pub fn build_statement<'a>(
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut verbs = vec![];
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    for pair in pair.into_inner() {
        match pair.as_rule() {
            Rule::verb => {
                verbs.push(build_verb(pair)?);
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
        verbs: verbs,
        scope: scope,
    }))
}

pub trait Statement: Debug {
    fn run(&self, cfg_in: &ControlFlowGraph) -> ControlFlowGraph;
}

impl Statement for DefaultStatement {
    fn run(&self, cfg_in: &ControlFlowGraph) -> ControlFlowGraph {
        let mut outer: ControlFlowGraph = cfg_in.clone();
        for verb in self.verbs.iter() {
            outer = verb.apply(&outer);
        }

        let inner = self.scope.run(cfg_in);
        self.scope.combine(&cfg_in, &outer, &inner)
    }
}
