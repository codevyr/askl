use anyhow::Result;
use pest::{error::Error, iterators::Pairs, Parser};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

trait AstType {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>>;
}

#[derive(Debug)]
struct Identifier(pub String);

impl AstType for Identifier {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let ident = pair.as_str();
        Ok(AstNode::Identifier(Identifier(ident.into())))
    }
}

#[derive(Debug)]
struct Value(pub String);

impl AstType for Value {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let string = pair.as_str();
        Ok(AstNode::Value(Value(string.into())))
    }
}

#[derive(Debug)]
struct NamedArgument {
    name: Box<AstNode>,
    value: Box<AstNode>,
}

impl AstType for NamedArgument {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let mut pair = pair.into_inner();
        let ident = pair.next().unwrap();
        let ident = Identifier::build(ident).unwrap();
        let value = pair.next().unwrap();
        let value = Value::build(value).unwrap();
        Ok(AstNode::Argument(NamedArgument {
            name: Box::new(ident),
            value: Box::new(value),
        }))
    }
}

#[derive(Debug)]
struct Verb {
    ident: Box<AstNode>,
    args: Vec<AstNode>,
}

impl AstType for Verb {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let mut pair = pair.into_inner();
        let ident = pair.next().unwrap();
        let ident = Identifier::build(ident).unwrap();
        let args: Result<Vec<AstNode>, _> = pair.map(NamedArgument::build).collect();
        Ok(AstNode::Verb(Verb {
            ident: Box::new(ident),
            args: args?,
        }))
    }
}
#[derive(Debug)]
struct Statement {
    verbs: Vec<AstNode>,
    scope: Box<AstNode>,
}

impl AstType for Statement {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let mut verbs = vec![];
        let mut scope = AstNode::None;

        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::verb => {
                    verbs.push(Verb::build(pair)?);
                }
                Rule::scope => {
                    scope = Scope::build(pair)?;
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

        Ok(AstNode::Statement(Statement {
            verbs: verbs,
            scope: Box::new(scope),
        }))
    }
}

#[derive(Debug)]
struct Scope(pub Vec<AstNode>);

impl AstType for Scope {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let statements: Result<Vec<AstNode>, _> = pair.into_inner().map(Statement::build).collect();
        Ok(AstNode::Scope(statements?))
    }
}

#[derive(Debug)]
enum AstNode {
    Statement(Statement),
    Scope(Vec<AstNode>),
    Verb(Verb),
    Argument(NamedArgument),
    Identifier(Identifier),
    Value(Value),
    None,
}

fn parse_ask(pairs: Pairs<Rule>) -> Result<Vec<AstNode>, Error<Rule>> {
    let mut ast = vec![];
    for pair in pairs {
        match pair.as_rule() {
            Rule::statement => ast.push(Statement::build(pair)?),
            Rule::EOI => {}
            _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
        };
    }

    Ok(ast)
}

pub struct Askl {}

impl Askl {
    pub fn new(ask_code: &str) -> Result<Askl> {
        let pairs = AsklParser::parse(Rule::ask, ask_code)?;

        let askl = parse_ask(pairs);
        println!("{:#?}", &askl);
        Ok(Self {})
    }
}
