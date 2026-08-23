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

/// The type of an argument value.  Used to say what a verb wanted when it
/// got something else, and to recognise the pre-typed-arguments spelling of
/// a value so the error can teach the replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Str,
    Int,
    Bool,
}

impl ValueType {
    /// Reads after "expects": `expects an integer`.
    pub fn describe(&self) -> &'static str {
        match self {
            ValueType::Str => "a string",
            ValueType::Int => "an integer",
            ValueType::Bool => "a boolean (true or false)",
        }
    }
}

/// A typed argument value.  Strings carry a [`StringKind`] so a glob pattern
/// stays distinguishable from a plain name; integers are 64-bit.
#[derive(Debug, Clone)]
pub enum Value {
    Str { kind: StringKind, text: String },
    Int(i64),
    Bool(bool),
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

    pub fn type_of(&self) -> ValueType {
        match self {
            Value::Str { .. } => ValueType::Str,
            Value::Int(_) => ValueType::Int,
            Value::Bool(_) => ValueType::Bool,
        }
    }

    /// Reads after "found": `found the string "500"`.
    pub fn describe(&self) -> String {
        match self {
            Value::Str {
                kind: StringKind::Plain,
                text,
            } => format!("the string {:?}", text),
            Value::Str {
                kind: StringKind::Glob,
                text,
            } => format!("the glob string g{:?}", text),
            Value::Int(n) => format!("the integer {}", n),
            Value::Bool(b) => format!("the boolean {}", b),
        }
    }

    /// `Some(literal)` when this is a quoted string that spells a `want`-typed
    /// value — that is, the spelling arguments used before they were typed.
    /// Drives the migration hint (`write limit=500`); `None` when the value is
    /// wrong in a way dropping the quotes would not fix.
    pub fn unquoted_spelling(&self, want: ValueType) -> Option<&str> {
        let text = match self {
            Value::Str {
                kind: StringKind::Plain,
                text,
            } => text.as_str(),
            _ => return None,
        };
        match want {
            ValueType::Str => None,
            ValueType::Int => text.parse::<i64>().is_ok().then_some(text),
            ValueType::Bool => matches!(text, "true" | "false").then_some(text),
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
            other => anyhow::bail!("expected a plain string, found {}", other.describe()),
        }
    }

    pub fn build(pair: pest::iterators::Pair<Rule>) -> Result<Value, Error<Rule>> {
        match pair.as_rule() {
            // `quoted_string` wraps the `string` it contains (it is not
            // silent, so that it shows up in parse errors); unwrap and recurse.
            Rule::quoted_string => Value::build(pair.into_inner().next().unwrap()),
            Rule::string => Ok(Value::Str {
                kind: StringKind::Plain,
                text: pair.as_str().into(),
            }),
            Rule::boolean => Ok(Value::Bool(pair.as_str() == "true")),
            Rule::integer => pair.as_str().parse::<i64>().map(Value::Int).map_err(|_| {
                Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!(
                            "integer literal '{}' does not fit in a 64-bit integer",
                            pair.as_str()
                        ),
                    },
                    pair.as_span(),
                )
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

/// Rename grammar rules to what a query author would call them, so pest's
/// "expected ..." lists read as the language rather than as grammar
/// internals — `expected a quoted string, a glob string, an integer, or a
/// boolean`, not `expected prefixed_string, integer, or boolean`.
fn name_rules_for_humans(err: Error<Rule>) -> Error<Rule> {
    err.renamed_rules(|rule| match rule {
        Rule::quoted_string => "a quoted string".to_string(),
        Rule::prefixed_string | Rule::string_prefix => "a glob string (g\"...\")".to_string(),
        Rule::integer => "an integer".to_string(),
        Rule::boolean => "a boolean (true or false)".to_string(),
        Rule::ident => "a verb name".to_string(),
        Rule::named_argument => "a named argument".to_string(),
        Rule::positional_argument => "a positional argument".to_string(),
        Rule::statement => "a statement".to_string(),
        other => format!("{:?}", other),
    })
}

pub fn parse(ask_code: &str) -> Result<Rc<Statement>, pest::error::Error<Rule>> {
    parse_inner(ask_code).map_err(name_rules_for_humans)
}

fn parse_inner(ask_code: &str) -> Result<Rc<Statement>, pest::error::Error<Rule>> {
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
