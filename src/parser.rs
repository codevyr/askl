use anyhow::Result;
use pest::{error::Error, iterators::Pairs, Parser};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "askl.pest"]
struct AsklParser;

#[derive(Debug)]
enum AstNode {
    Statement {
        verbs: Box<AstNode>,
        scope: Box<AstNode>,
    },
    Scope(Vec<AstNode>),
    Verbs(Vec<AstNode>),
    Verb {
        ident: Box<AstNode>,
        args: Vec<AstNode>,
    },
    NamedArgument {
        name: Box<AstNode>,
        value: Box<AstNode>,
    },
    Identifier(String),
    Value(String),
    None,
}

fn build_ast_from_ident(pair: pest::iterators::Pair<Rule>) -> AstNode {
    match pair.as_rule() {
        Rule::ident => {
            let ident = pair.into_inner().as_str();
            AstNode::Identifier(ident.into())
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_value(pair: pest::iterators::Pair<Rule>) -> AstNode {
    println!("AST FROM VALUE {:#?}\n\n", pair);
    match pair.as_rule() {
        Rule::string => {
            let string = pair.into_inner().as_str();
            AstNode::Value(string.into())
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_arg(pair: pest::iterators::Pair<Rule>) -> AstNode {
    match pair.as_rule() {
        Rule::named_argument => {
            let mut pair = pair.into_inner();
            let ident = pair.next().unwrap();
            let ident = build_ast_from_ident(ident);
            let value = pair.next().unwrap();
            let value = build_ast_from_value(value);
            AstNode::NamedArgument {
                name: Box::new(ident),
                value: Box::new(value),
            }
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_verb(pair: pest::iterators::Pair<Rule>) -> AstNode {
    println!("AST FROM VERBS {:#?}\n\n", pair);
    match pair.as_rule() {
        Rule::verb => {
            let mut pair = pair.into_inner();
            let ident = pair.next().unwrap();
            let ident = build_ast_from_ident(ident);
            let args: Vec<AstNode> = pair.map(build_ast_from_arg).collect();
            AstNode::Verb {
                ident: Box::new(ident),
                args: args,
            }
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_verbs(pair: pest::iterators::Pair<Rule>) -> AstNode {
    match pair.as_rule() {
        Rule::verbs => {
            let verbs: Vec<AstNode> = pair.into_inner().map(build_ast_from_verb).collect();
            AstNode::Verbs(verbs)
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_scope(pair: pest::iterators::Pair<Rule>) -> AstNode {
    match pair.as_rule() {
        Rule::scope => {
            let statements: Vec<AstNode> =
                pair.into_inner().map(build_ast_from_statement).collect();
            AstNode::Scope(statements)
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn build_ast_from_statement(pair: pest::iterators::Pair<Rule>) -> AstNode {
    println!("{:#?}\n\n", pair);

    match pair.as_rule() {
        Rule::statement => {
            let mut pair = pair.into_inner();
            let verbs = pair.next().unwrap();
            let verbs = build_ast_from_verbs(verbs);
            let scope = if let Some(pair) = pair.next() {
                build_ast_from_scope(pair)
            } else {
                AstNode::None
            };
            AstNode::Statement {
                verbs: Box::new(verbs),
                scope: Box::new(scope),
            }
        }
        _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
    }
}

fn parse_ask(pairs: Pairs<Rule>) -> Result<Vec<AstNode>, Error<Rule>> {
    let mut ast = vec![];
    for pair in pairs {
        match pair.as_rule() {
            Rule::statement => ast.push(build_ast_from_statement(pair)),
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
