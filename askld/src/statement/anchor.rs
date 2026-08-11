//! Anchor completeness: every query component must contain at least one
//! anchored statement (see [`crate::verb::AnchorKind`]).
//!
//! A component is a top-level statement tree merged with any tree it is
//! label-connected to (`User` / `PreSeedLabel` edges).  Non-anchored
//! statements never drive execution — they derive from their neighbours —
//! so a component with no anchor anywhere cannot produce anything.  Before
//! this check that shape silently returned an empty result; now it is a
//! structured error naming the offending statement and the fix.
//!
//! Only components that carry a real constraint somewhere (see
//! [`crate::command::Command::demands_anchoring`]) are held to the rule:
//! unit-only structure (`{{}}`) and preamble directives stay harmless
//! degenerate queries.

use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::statement::Statement;
use pest::error::{Error, ErrorVariant};
use std::collections::HashMap;
use std::rc::Rc;

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn find(&mut self, i: usize) -> usize {
        if self.0[i] != i {
            let root = self.find(self.0[i]);
            self.0[i] = root;
        }
        self.0[i]
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra] = rb;
        }
    }
}

fn collect_tree(statement: &Rc<Statement>, out: &mut Vec<Rc<Statement>>) {
    out.push(statement.clone());
    for child in statement.children() {
        collect_tree(&child, out);
    }
}

/// Check the whole query for anchor completeness.  `root` is the global
/// wrapper statement; its scope's statements are the top-level trees.  The
/// wrapper itself is deliberately excluded — it parents every tree, and
/// walking through it would fuse unrelated trees into one component.
///
/// Must run after `build_dependency_graph`: label connectivity is read
/// from the installed `User`/`PreSeedLabel` dependency edges.
pub(super) fn check_anchor_completeness(root: &Statement) -> Result<(), Error<Rule>> {
    use crate::execution_state::DependencyRole;

    let mut trees: Vec<Vec<Rc<Statement>>> = Vec::new();
    for top in root.scope().statements() {
        let mut nodes = Vec::new();
        collect_tree(&top, &mut nodes);
        trees.push(nodes);
    }

    let mut tree_of: HashMap<*const Statement, usize> = HashMap::new();
    for (i, tree) in trees.iter().enumerate() {
        for statement in tree {
            tree_of.insert(Rc::as_ptr(statement), i);
        }
    }

    // Merge trees connected by label edges.  Direction is irrelevant for
    // grounding: an anchor on either side bounds the whole component.
    let mut uf = UnionFind::new(trees.len());
    for (i, tree) in trees.iter().enumerate() {
        for statement in tree {
            for dep in statement.get_state().dependencies.iter() {
                if matches!(
                    dep.dependency_role,
                    DependencyRole::User | DependencyRole::PreSeedLabel(_)
                ) {
                    if let Some(&j) = tree_of.get(&Rc::as_ptr(&dep.dependency)) {
                        uf.union(i, j);
                    }
                }
            }
        }
    }

    // Component verdicts.  Trees are in source order and each tree is in
    // pre-order, so the first recorded demanding statement of a failing
    // component is the earliest one — the natural place for the error.
    let n = trees.len();
    let roots: Vec<usize> = (0..n).map(|i| uf.find(i)).collect();
    let mut has_anchor = vec![false; n];
    let mut first_demanding: Vec<Option<Rc<Statement>>> = vec![None; n];
    for (i, tree) in trees.iter().enumerate() {
        let r = roots[i];
        for statement in tree {
            let command = statement.command();
            if command.is_anchored() {
                has_anchor[r] = true;
            }
            if command.demands_anchoring() && first_demanding[r].is_none() {
                first_demanding[r] = Some(statement.clone());
            }
        }
    }

    for r in 0..n {
        if let Some(statement) = &first_demanding[r] {
            if !has_anchor[r] {
                return Err(Error::new_from_span(
                    ErrorVariant::CustomError {
                        message: "this query group selects nothing: no statement in it has a \
                                  selecting predicate.  Add an anchor — a name (\"foo\", g\"pat*\", \
                                  func(\"foo\")), a name filter, search(...), loc(...) — or use \
                                  `all` for an explicit budget-bounded enumeration"
                            .to_string(),
                    },
                    statement.command().span().as_pest_span(),
                ));
            }
        }
    }

    Ok(())
}
