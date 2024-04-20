use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::{ParserContext, Rule};
use crate::statement::{build_statement, Statement};
use crate::symbols::SymbolChild;
use core::fmt::Debug;
use pest::error::Error;

pub fn build_scope(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(|p| build_statement(ctx, p)).collect();

    Ok(ctx.new_scope(statements?))
}

#[derive(Debug)]
pub enum ScopeFactory {
    Children,
    Global,
    Empty,
}

impl ScopeFactory {
    pub fn create(&self, statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        match self {
            Self::Children => DefaultScope::new(statements),
            Self::Global => GlobalScope::new(statements),
            _ => panic!("Impossible: {:?}", self),
        }
    }
}

type StatementIter<'a> = Box<dyn Iterator<Item=&'a Box<dyn Statement + 'a>>+ 'a>;
pub trait Scope: Debug {
    fn statements(&self) -> StatementIter;

    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild>;

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList);
}

#[derive(Debug)]
pub struct DefaultScope(Vec<Box<dyn Statement>>);

impl DefaultScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        Box::new(Self(statements))
    }
}

impl Scope for DefaultScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.0.iter())
    }

    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        cfg.symbols.get_children(&symbol.symbol_id)
    }

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        let mut res_symbols: Vec<SymbolChild> = vec![];
        let mut res_nodes = NodeList(vec![]);
        let mut res_edges = EdgeList(vec![]);

        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((passed_symbols, scope_nodes, scope_edges)) =
                statement.execute(cfg, &active_symbols)
            {
                res_symbols.extend(passed_symbols.into_iter());

                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_nodes
                    .0
                    .extend(res_symbols.iter().map(|s| s.symbol_id.clone()));
                res_edges.0.extend(scope_edges.0.into_iter());
            }
        }

        // Sort and deduplicate the sources
        res_symbols.sort();
        res_symbols.dedup();
        res_nodes.0.sort();
        res_nodes.0.dedup();
        res_edges.0.sort();
        res_edges.0.dedup();
        (res_symbols, res_nodes, res_edges)
    }
}

#[derive(Debug)]
pub struct GlobalScope(Vec<Box<dyn Statement>>);

impl GlobalScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        Box::new(Self(statements))
    }
}

impl Scope for GlobalScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.0.iter())
    }

    fn derive(&self, _cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        vec![symbol.clone()]
    }

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        let mut res_symbols: Vec<SymbolChild> = vec![];
        let mut nodes = NodeList(vec![]);
        let mut edges = EdgeList(vec![]);

        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((new_passed_symbols, node_list, edge_list)) =
                statement.execute(cfg, &active_symbols)
            {
                nodes.0.extend(node_list.0.into_iter());
                nodes
                    .0
                    .extend(new_passed_symbols.iter().map(|s| s.symbol_id.clone()));
                edges.0.extend(edge_list.0.into_iter());
                res_symbols.extend(new_passed_symbols.into_iter());
            }
        }

        // Sort and deduplicate the sources
        res_symbols.sort();
        res_symbols.dedup();
        nodes.0.sort();
        nodes.0.dedup();
        edges.0.sort();
        edges.0.dedup();
        (res_symbols, nodes, edges)
    }
}

#[derive(Debug)]
pub struct EmptyScope;

impl EmptyScope {
    pub fn new() -> Self {
        Self {}
    }
}

impl Scope for EmptyScope {
    fn statements(&self) -> StatementIter {
        Box::new(std::iter::empty::<_>())
    }

    fn derive(&self, _cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        vec![symbol.clone()]
    }

    fn run(
        &self,
        _cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        (
            active_symbols
                .iter()
                .map(|s| SymbolChild {
                    symbol_id: s.symbol_id.clone(),
                    occurence: None,
                })
                .collect(),
            NodeList(vec![]),
            EdgeList(vec![]),
        )
    }
}
