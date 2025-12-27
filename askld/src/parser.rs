use crate::{
    hierarchy,
    parser_context::{ParserContext, ScopeFactory},
    span::Span,
    statement::{build_statement, Statement},
};
use anyhow::Result;
use core::fmt::Debug;
use pest::{error::Error, Parser};
use pest_derive::Parser;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

#[derive(Debug)]
pub struct Identifier(pub String);

impl Identifier {
    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Identifier, Error<Rule>> {
        match pair.as_rule() {
            Rule::ident => {}
            rule => Err(Error::new_from_span(
                pest::error::ErrorVariant::ParsingError {
                    positives: vec![Rule::ident],
                    negatives: vec![rule],
                },
                pair.as_span(),
            ))?,
        }
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

pub fn parse(ask_code: &str) -> Result<Rc<Statement>, pest::error::Error<Rule>> {
    let source = Arc::new(ask_code.to_string());
    let pairs = AsklParser::parse(Rule::ask, &source)?;

    let ctx = ParserContext::new(source.clone(), ScopeFactory::Children);
    let mut ast = vec![];
    for pair in pairs {
        match pair.as_rule() {
            Rule::statement => ast.push(build_statement(ctx.clone(), pair)?),
            Rule::EOI => {}
            _ => {
                return Err(Error::new_from_span(
                    pest::error::ErrorVariant::ParsingError {
                        positives: vec![Rule::statement],
                        negatives: vec![pair.as_rule()],
                    },
                    pair.as_span(),
                ))?
            }
        };
    }

    let scope = ctx.new_scope(ast);

    for statement in scope.statements() {
        hierarchy::populate_parents(&statement);
    }

    let whole_span: Span = Span::entire(source.clone());
    let statement = Statement::new(ctx.command(whole_span), scope.clone());
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}
