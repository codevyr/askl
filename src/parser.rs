use crate::{
    scope::{Scope, ScopeFactory},
    statement::{build_statement, GlobalStatement, Statement},
    verb::{ChildrenVerb, Verb}, command::Command,
};
use anyhow::Result;
use core::fmt::Debug;
use pest::{error::Error, Parser};
use pest_derive::Parser;
use std::sync::Arc;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

#[derive(Debug)]
pub struct Identifier(pub String);

impl Identifier {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Identifier, Error<Rule>> {
        match pair.as_rule() {
            Rule::generic_ident => {}
            rule => Err(Error::new_from_span(
                pest::error::ErrorVariant::ParsingError {
                    positives: vec![Rule::generic_ident],
                    negatives: vec![rule],
                },
                pair.as_span(),
            ))?,
        }
        let pair = pair.into_inner();
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

#[derive(Debug)]
pub struct PositionalArgument {
    pub value: Value,
}

impl PositionalArgument {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Self, Error<Rule>> {
        let mut pair = pair.into_inner();
        let value = pair.next().unwrap();
        let value = Value::build(value).unwrap();
        Ok(Self { value })
    }
}

#[derive(Debug)]
pub struct ParserContext<'a> {
    prev: Option<&'a ParserContext<'a>>,
    scope_factory: Option<ScopeFactory>,
    command: Command,
}

impl<'a> ParserContext<'a> {
    pub fn new(scope_factory: ScopeFactory) -> Self {
        let mut command = Command::new();
        command.extend(ChildrenVerb::new());
        Self {
            prev: None,
            command: command,
            scope_factory: Some(scope_factory),
        }
    }

    pub fn derive(&'a self) -> Box<Self> {
        println!("{:#?}", self);
        Box::new(Self {
            prev: Some(self),
            command: self.command.derive(),
            scope_factory: None,
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

    pub fn consume(&mut self, verb: Arc<dyn Verb>) -> Option<Arc<dyn Verb>> {
        if !verb.update_context(self) {
            Some(verb)
        } else {
            None
        }
    }

    pub fn command(self) -> Command {
        self.command
    }

    pub fn extend_verb(&mut self, verb: Arc<dyn Verb>) {
        self.command.extend(verb)
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

    Ok(GlobalStatement::new(scope))
}
