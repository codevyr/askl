use crate::scope::Scope;
use crate::{cfg::ControlFlowGraph, symbols::SymbolMap};
use anyhow::Result;

pub struct Executor {
    global: Box<dyn Scope>,
    symbols: SymbolMap,
}

impl Executor {
    pub fn new(global: Box<dyn Scope>) -> Result<Self> {
        Ok(Self {
            global: global,
            symbols: SymbolMap::new(),
        })
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
