use crate::symbols::SymbolMap;

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {

}

impl ControlFlowGraph {
    pub fn new() -> Self {
        Self{}
    }
    
    pub fn from_symbols(symbols: &SymbolMap) -> Self {
        Self{}
    }

    pub fn merge(&mut self, other: &ControlFlowGraph) {}
}