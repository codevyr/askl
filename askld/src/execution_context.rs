use std::collections::{HashMap, HashSet};

use index::symbols::DeclarationId;

pub struct ExecutionContext {
    pub saved_labels: HashMap<String, HashSet<DeclarationId>>,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            saved_labels: HashMap::new(),
        }
    }
}
