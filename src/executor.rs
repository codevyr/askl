use std::collections::HashMap;

use crate::{
    cfg::ControlFlowGraph,
    parser::{self, Ast, AstNode},
    symbols::SymbolMap,
};

#[derive(Debug)]
pub struct Verb {
    name: String,
    positional: Vec<String>,
    named: HashMap<String, String>,
}

impl Verb {
    fn from_ast(ast: &parser::Verb) -> Self {
        let mut positional = vec![];
        let mut named = HashMap::new();
        for (name, value) in ast.arguments().iter() {
            match name {
                Some(name) => {
                    named.insert(name.clone(), value.clone());
                }
                None => positional.push(value.clone()),
            }
        }

        Self {
            name: ast.ident(),
            positional: positional,
            named: named,
        }
    }

    fn apply(&self, cfg: &ControlFlowGraph) -> ControlFlowGraph {
        cfg.clone()
    }
}

#[derive(Debug)]
pub struct Statement {
    verbs: Vec<Verb>,
    scope: Option<Box<Scope>>,
}

impl Statement {
    fn from_ast(ast: &parser::Statement) -> Self {
        let mut vec = vec![];
        for node in ast.verbs.iter() {
            match node {
                AstNode::Verb(v) => vec.push(Verb::from_ast(&v)),
                _ => unreachable!("Impossible AstNode: {:?}", node),
            }
        }

        Self {
            verbs: vec,
            scope: None,
        }
    }

    fn run(&self, cfg_in: &ControlFlowGraph) -> ControlFlowGraph {
        let mut outer: ControlFlowGraph = cfg_in.clone();
        for verb in self.verbs.iter() {
            outer = verb.apply(&outer);
        }

        if let Some(scope) = &self.scope {
            let inner = scope.run(cfg_in);
            scope.combine(&outer, &inner)
        } else {
            outer
        }
    }
}

#[derive(Debug)]
pub struct Scope {
    statements: Vec<Statement>,
}

impl Scope {
    fn from_ast_iter(nodes: core::slice::Iter<AstNode>) -> Self {
        let mut vec = vec![];
        for node in nodes {
            match node {
                AstNode::Statement(s) => vec.push(Statement::from_ast(s)),
                _ => unreachable!("Impossible AstNode: {:?}", node),
            }
        }

        Self { statements: vec }
    }

    fn run(&self, cfg_in: &ControlFlowGraph) -> ControlFlowGraph {
        let mut cfg_out = ControlFlowGraph::new();
        for statement in self.statements.iter() {
            cfg_out.merge(&statement.run(cfg_in));
        }
        cfg_out
    }

    fn combine(&self, outer: &ControlFlowGraph, inner: &ControlFlowGraph) -> ControlFlowGraph {
        outer.clone()
    }
}

pub struct Executor {
    global: Scope,
    symbols: SymbolMap,
}

impl Executor {
    pub fn new(ast: Ast) -> Self {
        Self {
            global: Scope::from_ast_iter(ast.iter_statements()),
            symbols: SymbolMap::new(),
        }
    }

    pub fn add_symbols<'a>(&'a mut self, symbols: SymbolMap) -> &'a mut Self {
        self.symbols.merge(symbols);
        self
    }

    pub fn run(&self) -> ControlFlowGraph {
        let cfg_in = ControlFlowGraph::from_symbols(&self.symbols);
        self.global.run(&cfg_in)
    }
}
