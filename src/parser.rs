use crate::{
    scope::{Scope, ScopeFactory},
    statement::{build_statement, DefaultStatement, Statement},
    verb::Verb,
};
use anyhow::Result;
use core::fmt::Debug;
use pest::{error::Error, Parser};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

#[derive(Debug)]
pub struct Identifier(pub String);

impl Identifier {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Identifier, Error<Rule>> {
        let ident = pair.as_str();
        Ok(Identifier(ident.into()))
    }
}

#[derive(Debug)]
pub struct Value(pub String);

impl Value {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Value, Error<Rule>> {
        let string = match pair.as_rule() {
            Rule::string => pair.as_str(),
            _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
        };
        Ok(Value(string.into()))
    }
}

#[derive(Debug)]
pub struct NamedArgument {
    pub name: Identifier,
    pub value: Value,
}

impl NamedArgument {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<NamedArgument, Error<Rule>> {
        let mut pair = pair.into_inner();
        let ident = pair.next().unwrap();
        let ident = Identifier::build(ident).unwrap();
        let value = pair.next().unwrap();
        let value = Value::build(value).unwrap();
        Ok(NamedArgument {
            name: ident,
            value: value,
        })
    }
}

#[derive(Debug, Default)]
pub struct ParserContext<'a> {
    prev: Option<&'a ParserContext<'a>>,
    scope_factory: Option<ScopeFactory>,
}

impl<'a> ParserContext<'a> {
    pub fn new(scope_factory: ScopeFactory) -> Self {
        Self {
            prev: None,
            scope_factory: Some(scope_factory),
        }
    }

    pub fn derive(&'a self) -> Box<Self> {
        Box::new(Self {
            prev: Some(self),
            ..Default::default()
        })
    }

    pub fn set_scope_factory(&mut self, scope_factory: ScopeFactory) {
        self.scope_factory = Some(scope_factory);
    }

    pub fn new_scope(&self, statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        if let Some(factory) = &self.scope_factory {
            return factory.create(statements);
        }

        let factory = self.prev.expect("Should never try uninitialized factory");
        factory.new_scope(statements)
    }

    pub fn consume(&mut self, verb: Box<dyn Verb>) -> Option<Box<dyn Verb>> {
        if !verb.update_context(self) {
            Some(verb)
        } else {
            None
        }
    }
}

pub fn parse(ask_code: &str) -> Result<Box<dyn Statement>> {
    let pairs = AsklParser::parse(Rule::ask, ask_code)?;

    let ctx = ParserContext::new(ScopeFactory::Children);
    let mut ast = vec![];
    for pair in pairs {
        match pair.as_rule() {
            Rule::statement => ast.push(build_statement(&ctx, pair)?),
            Rule::EOI => {}
            _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
        };
    }

    let scope = ctx.new_scope(ast);

    Ok(DefaultStatement::new_main(scope))
}
