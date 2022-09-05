use crate::{
    scope::{DefaultScope, Scope},
    statement::build_statement,
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
        let string = pair.as_str();
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

pub fn parse(ask_code: &str) -> Result<Box<dyn Scope>> {
    let pairs = AsklParser::parse(Rule::ask, ask_code)?;

    let mut ast = vec![];
    for pair in pairs {
        match pair.as_rule() {
            Rule::statement => ast.push(build_statement(pair)?),
            Rule::EOI => {}
            _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
        };
    }

    Ok(Box::new(DefaultScope::new(ast)))
}
