use std::collections::HashSet;

use index::symbols::DeclarationId;

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, statement::Statement};

#[derive(Debug)]
pub struct ExecutionState {
    pub completed: bool,
    pub current: Option<HashSet<DeclarationId>>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            completed: false,
            current: None,
        }
    }

    /// Initializes the execution state with the selected nodes from the given statement.
    pub fn select_nodes(
        &mut self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        statement: &Statement,
    ) {
        let references = statement.command().compute_selected(ctx, cfg);
        if references.is_none() {
            println!("No references found for the statement.");
            return;
        }
        let mut selected: HashSet<DeclarationId> = HashSet::new();
        for (id, _) in references.unwrap().into_iter() {
            selected.insert(id);
        }

        let filtered_declarations = statement.command().filter_nodes(cfg, selected);

        self.current = Some(filtered_declarations);
        println!("Initial statement state: {:?}", self.current);
    }

    pub fn nodes_iter(&self) -> impl Iterator<Item = &DeclarationId> {
        self.current.iter().flat_map(|s| s.iter())
    }

    /// Removes from the current state all the symbols that are not in `declarations`.
    pub fn retain(&mut self, _ctx: &mut ExecutionContext, declarations: &HashSet<DeclarationId>) {
        println!("Retaining only : {:?} // {:?}", self.current, declarations);
        if let Some(current) = &mut self.current {
            let old_size = current.len();
            current.retain(|id| declarations.contains(id));

            if old_size != current.len() {
                self.completed = false;
                println!("Updated state after removing: {:?}", self.current);
            }
        } else {
            self.current = Some(declarations.clone());
            println!("Initial state set to: {:?}", self.current);
        }
    }
}
