use index::db_diesel::{Selection, SelectionNode};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, statement::Statement};

#[derive(Debug)]
pub struct ExecutionState {
    pub completed: bool,
    pub current: Option<Selection>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            completed: false,
            current: None,
        }
    }

    /// Initializes the execution state with the selected nodes from the given statement.
    pub async fn select_nodes(
        &mut self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        statement: &Statement,
    ) {
        let mut selection = if let Some(s) = statement.command().compute_selected(ctx, cfg).await {
            s
        } else {
            println!("No selection found for the statement.");
            return;
        };

        statement.command().filter(cfg, &mut selection);

        self.current = Some(selection);
    }

    pub fn nodes_iter(&self) -> impl Iterator<Item = &SelectionNode> {
        self.current.iter().flat_map(|s| s.nodes.iter())
    }

    /// Removes from the current state all the symbols that are not in `declarations`.
    pub fn retain(&mut self, _ctx: &mut ExecutionContext, constraint: &Selection) {
        println!("Retaining only : {:?} // {:?}", self.current, constraint);
        if let Some(current) = &mut self.current {
            let old_size = current.nodes.len();
            current.nodes.retain(|cur| {
                constraint
                    .nodes
                    .iter()
                    .any(|con| cur.declaration.id == con.declaration.id)
            });

            if old_size != current.nodes.len() {
                self.completed = false;
                println!("Updated state after removing: {:?}", self.current);
            }
        } else {
            self.current = Some(constraint.clone());
        }
    }
}
