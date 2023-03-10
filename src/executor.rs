
use crate::scope::Scope;
use crate::{cfg::ControlFlowGraph, symbols::SymbolMap};
use anyhow::Result;
use log::debug;
use petgraph::graphmap::DiGraphMap;

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

    pub fn run<'a>(&'a self) -> DiGraphMap<&'a str, ()> {
        let cfg_in = ControlFlowGraph::from_symbols(&self.symbols);

        debug!("Global scope: {:#?}", self.global);

        let (outer, inner) = self.global.run(&cfg_in);

        let mut result_graph : DiGraphMap<&str, ()> = DiGraphMap::new();

        for (from, to) in inner.0 {
            let sym_from = self.symbols.map.get(from).unwrap();
            let sym_to = self.symbols.map.get(to).unwrap();

            result_graph.add_edge(&sym_from.name, &sym_to.name, ());
        }

        for loc in outer.0 {
            let sym= self.symbols.map.get(loc).unwrap();
            result_graph.add_node(&sym.name);
        }

        result_graph
    }
}
