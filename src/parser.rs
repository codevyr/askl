use anyhow::Result;
use pest::{error::Error, Parser};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

trait AstType {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>>;
}

#[derive(Debug)]
pub struct Identifier(pub String);

impl AstType for Identifier {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let ident = pair.as_str();
        Ok(AstNode::Identifier(Identifier(ident.into())))
    }
}

#[derive(Debug)]
pub struct Value(pub String);

impl AstType for Value {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let string = pair.as_str();
        Ok(AstNode::Value(Value(string.into())))
    }
}

#[derive(Debug)]
pub struct NamedArgument {
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
pub struct Verb {
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

impl Verb {
    pub fn ident(&self) -> String {
        match &*self.ident {
            AstNode::Identifier(i) => i.0.clone(),
            node => unreachable!("Impossible AstNode: {:?}", node),
        }
    }

    pub fn arguments(&self) -> Vec<(Option<String>, String)> {
        self.args
            .iter()
            .map(|arg| match arg {
                AstNode::Argument(a) => {
                    match (&*a.name, &*a.value) {
                        (AstNode::Identifier(i), AstNode::Value(v)) => (Some(i.0.clone()), v.0.clone()),
                        (i, v) => unreachable!("Impossible AstNodes: {:?} {:?}", i, v),
                    }
                }
                node => unreachable!("Impossible AstNode: {:?}", node),
            })
            .collect()
    }
}
#[derive(Debug)]
pub struct Statement {
    pub verbs: Vec<AstNode>,
    pub scope: Box<AstNode>,
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
pub struct Scope(pub Vec<AstNode>);

impl AstType for Scope {
    fn build(pair: pest::iterators::Pair<Rule>) -> Result<AstNode, Error<Rule>> {
        let statements: Result<Vec<AstNode>, _> = pair.into_inner().map(Statement::build).collect();
        Ok(AstNode::Scope(Scope(statements?)))
    }
}

#[derive(Debug)]
pub struct Ast {
    statements: Vec<AstNode>,
}

impl Ast {
    pub fn parse(ask_code: &str) -> Result<Ast> {
        let pairs = AsklParser::parse(Rule::ask, ask_code)?;

        let mut ast = vec![];
        for pair in pairs {
            match pair.as_rule() {
                Rule::statement => ast.push(Statement::build(pair)?),
                Rule::EOI => {}
                _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
            };
        }

        Ok(Self { statements: ast })
    }

    pub fn iter_statements(&self) -> core::slice::Iter<AstNode> {
        self.statements.iter()
    }
}

#[derive(Debug)]
pub enum AstNode {
    Statement(Statement),
    Scope(Scope),
    Verb(Verb),
    Argument(NamedArgument),
    Identifier(Identifier),
    Value(Value),
    None,
}
