use crate::symbols::SymbolMap;

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {

}

impl ControlFlowGraph {
    pub fn new() -> Self {
        Self{}
    }
    
    pub fn from_symbols(_symbols: &SymbolMap) -> Self {
        Self{}
    }

    pub fn merge(&mut self, _other: &ControlFlowGraph) {}
}