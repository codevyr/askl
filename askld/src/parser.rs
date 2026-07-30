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

/// The type of a quoted string, selected by an optional prefix glued to the
/// opening quote (`g"..."` = glob; `re"..."` reserved for regex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKind {
    Plain,
    Glob,
}

/// A typed argument value.  Today only strings exist; this enum is the
/// groundwork for a richer type system (bool, numbers, lists).
#[derive(Debug, Clone)]
pub enum Value {
    Str { kind: StringKind, text: String },
}

impl Value {
    /// Construct a plain string value (used for synthetic arguments built
    /// from shortcuts like `@label`).
    pub fn plain(text: impl Into<String>) -> Value {
        Value::Str {
            kind: StringKind::Plain,
            text: text.into(),
        }
    }

    /// The string content, requiring a plain (unprefixed) string.  Use for
    /// arguments where pattern types make no sense (flags, search queries).
    pub fn as_plain(&self) -> anyhow::Result<&str> {
        match self {
            Value::Str {
                kind: StringKind::Plain,
                text,
            } => Ok(text),
            Value::Str {
                kind: StringKind::Glob,
                text,
            } => anyhow::bail!("expected a plain string, found glob string g\"{}\"", text),
        }
    }

    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Value, Error<Rule>> {
        match pair.as_rule() {
            Rule::string => Ok(Value::Str {
                kind: StringKind::Plain,
                text: pair.as_str().into(),
            }),
            Rule::prefixed_string => {
                let span = pair.as_span();
                let mut inner = pair.into_inner();
                let prefix = inner.next().unwrap();
                let string = inner.next().unwrap();
                let kind = match prefix.as_str() {
                    "g" => StringKind::Glob,
                    "re" => {
                        return Err(Error::new_from_span(
                            pest::error::ErrorVariant::CustomError {
                                message: "regex strings (re\"...\") are reserved but not \
                                          supported yet"
                                    .into(),
                            },
                            span,
                        ))
                    }
                    other => {
                        return Err(Error::new_from_span(
                            pest::error::ErrorVariant::CustomError {
                                message: format!(
                                    "unknown string prefix '{}' (supported: g\"...\" for glob \
                                     patterns)",
                                    other
                                ),
                            },
                            span,
                        ))
                    }
                };
                Ok(Value::Str {
                    kind,
                    text: string.as_str().into(),
                })
            }
            _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
        }
    }
}

/// Look up an optional named argument and require it to be a plain string.
/// Collapses the `named.get(k).map(|v| v.as_plain()).transpose()?` pattern
/// that verb constructors would otherwise repeat.
pub fn named_plain<'a>(
    named: &'a std::collections::HashMap<String, Value>,
    key: &str,
) -> anyhow::Result<Option<&'a str>> {
    named.get(key).map(|v| v.as_plain()).transpose()
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
        let ident = Identifier::build(ident)?;
        let value = pair.next().unwrap();
        let value = Value::build(value)?;
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
        let value = Value::build(value)?;
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
