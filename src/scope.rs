use crate::cfg::{EdgeList, ControlFlowGraph};
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use crate::symbols::SymbolId;
use core::fmt::Debug;
use pest::error::Error;

pub fn build_scope(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(build_statement).collect();
    Ok(Box::new(DefaultScope(statements?)))
}

pub trait Scope: Debug {
    fn statements(&self) -> &Vec<Box<dyn Statement>>;

    fn matched_symbols(&self, cfg: &ControlFlowGraph, symbols: &Vec<SymbolId>) -> Option<(Vec<SymbolId>, EdgeList)>{
        let mut result = EdgeList(vec![]);
        let mut result_sources : Vec<SymbolId> = vec![];

        if self.statements().len() == 0 {
            return Some((result_sources, result));
        }
        
        // Iterate through all the statements in the scope or subscope of
        // the query
        for statement in self.statements().iter() {

            let statement_symbols = statement.verb().symbols(cfg, symbols);

            // Iterate through all the symbols in the CFG
            for symbol_id in statement_symbols.iter() {
                let children = cfg.symbols.get_children(symbol_id).into_iter().map(|child| child.symbol_id).collect();

                    // If the statement matches the symbol, add it to the result
                if let Some((source_ids, mut edges)) = statement.scope().matched_symbols(cfg, &children) {
                    for source_id in source_ids.into_iter() {
                        edges.0.push((symbol_id.clone(), source_id.clone()));
                    }

                    // This nodes matches the pattern, so remember it
                    result_sources.push(symbol_id.clone());
                    result.0.extend(edges.0.into_iter()); 
                }
            }
        }

        if result_sources.len() == 0 {
            return None;
        }

        // Sort and deduplicate the sources
        result_sources.sort();
        result_sources.dedup();
        result.0.sort();
        result.0.dedup();
        Some((result_sources, result))
    }
}

#[derive(Debug)]
pub struct DefaultScope(Vec<Box<dyn Statement>>);

impl DefaultScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Self {
        Self(statements)
    }
}

impl Scope for DefaultScope {
    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }
}

#[derive(Debug)]
pub struct EmptyScope(Vec<Box<dyn Statement>>);

impl EmptyScope {
    pub fn new() -> Self {
        Self(vec![])
    }
}

impl Scope for EmptyScope {
    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }
}
