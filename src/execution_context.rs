use std::collections::{HashMap, HashSet};

use crate::symbols::SymbolId;

pub struct ExecutionContext {
    pub saved_labels: HashMap<String, HashSet<SymbolId>>,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            saved_labels: HashMap::new(),
        }
    }
}
