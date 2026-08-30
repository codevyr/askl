use crate::execution_context::ExecutionContext;
use crate::hierarchy::Hierarchy;
use index::db_diesel::{CompositeFilter, EphContext, ScopeContext};

use super::Statement;

/// An **echo**: a substatement that can never originate selection data of its
/// own, only display what a neighbour already found.  Structurally that is a
/// weak unit verb (`{}`, `{{…}}`) whose whole subtree is weak, so no
/// descendant could have originated data either.
///
/// A bare `{}` (weak UnitVerb) can acquire a selection in two ways:
///
/// 1. **Top-down echo** — a strong ancestor above derived data downward
///    through weak intermediaries.  Including this in the parent merge would
///    feed the parent's own data back to it, diluting constraints.
///
/// 2. **Bottom-up signal** — a non-weak descendant (e.g. a NameSelector)
///    originated data that propagated upward through weak intermediaries.
///    This is real constraining data that the parent needs.
///
/// We distinguish the two structurally: if every descendant is weak, no node
/// below could have originated data, so any selection is necessarily a
/// top-down echo (case 1).  If a non-weak descendant exists, real data could
/// have flowed up (case 2) — not an echo.
///
/// A direct-children check (`child.children().all(weak)`) is insufficient
/// because `mark_weak_statements` propagates weakness downward via the
/// `parent_weak` rule: a statement can be weak (from its parent) while having
/// a non-weak child of its own.  So a weak grandchild may still carry data
/// from a non-weak great-grandchild.  We therefore recurse the full subtree.
///
/// Three rules turn on this one predicate and must agree: exclusion from the
/// bottom-up parent merge, whether a scope builder may defer its eager
/// neighbourhood query, and — see `stage_read` — whether a neighbour may be
/// handed budget-truncated rows.
pub(super) fn is_echo(stmt: &Statement) -> bool {
    stmt.get_state().weak && stmt.command().is_unit() && all_descendants_weak(stmt)
}

/// A statement that will originate selection data of its own — a non-unit
/// selector, or a subtree containing one.  The complement of [`is_echo`].
/// Both scope builders defer an eager neighbourhood query only when the other
/// side originates data (it will carry the relationship edges itself); an echo
/// needs the eager query to materialise what it shows.
pub(super) fn originates_data(stmt: &Statement) -> bool {
    !is_echo(stmt)
}

fn all_descendants_weak(stmt: &Statement) -> bool {
    stmt.children()
        .all(|child| child.get_state().weak && all_descendants_weak(&child))
}

/// Build scope context for the parent side of a statement's parent query.
/// If the parent already has a selection, use its instance IDs.
/// If no parent exists, return Skip.
/// If the parent hasn't been selected yet, fall back to mixin-based scoping.
pub(super) fn build_parent_scope(
    statement: &Statement,
    ctx: &ExecutionContext,
    eph: &EphContext,
) -> ScopeContext {
    match statement.parent().and_then(|p| p.upgrade()) {
        Some(parent) => {
            if parent.is_computed(ctx) {
                match parent.get_selection(ctx) {
                    Some(sel) => ScopeContext::Scope {
                        ids: sel.get_instance_ids(),
                        filter: None,
                    },
                    // None = parent has no opinion (filter-only, unit, or no selectors).
                    // Run unscoped — the parent is transparent.
                    None => ScopeContext::Unscoped,
                }
            } else {
                // Parent not yet computed but probe-resolved: its exact (or
                // composition-exact) instance set is already known — scope
                // by ids, skipping the condition the index would otherwise
                // have to materialise.
                if let Some(ids) = ctx
                    .probe_resolved
                    .get(&crate::execution_context::statement_key(&parent))
                {
                    return ScopeContext::Scope {
                        ids: ids.clone(),
                        filter: None,
                    };
                }
                // Parent not yet computed — fall back to filter-based scoping.
                //
                // ONE symmetric rule for both scope builders: materialise your
                // neighbour-facing list iff the neighbour is computed or
                // conditioned; otherwise DEFER iff your own statement is
                // conditioned (the neighbour's opposite, scoped-by-your-
                // condition query covers the edge) and the neighbour
                // originates data; if both sides are bare, this (child) side
                // materialises unscoped — someone must.
                match parent.command().get_selector_composite_filter(eph) {
                    Some(f) => ScopeContext::Scope {
                        ids: vec![],
                        filter: Some(f),
                    },
                    None => {
                        let self_conditioned = statement
                            .command()
                            .get_selector_composite_filter(eph)
                            .is_some();
                        if self_conditioned && originates_data(&parent) {
                            // The parent's children-side query is scoped by
                            // THIS statement's condition and covers the edge;
                            // the evidence union accepts it from that side.
                            ScopeContext::Skip
                        } else {
                            ScopeContext::Unscoped
                        }
                    }
                }
            }
        }
        None => ScopeContext::Unscoped, // Root-level: run parents unscoped
    }
}

/// Build scope context for the children side of a statement's children query.
/// Collects instance IDs from already-selected children + filters from unselected children.
/// If no children exist, return Skip.
pub(super) fn build_children_scope(
    statement: &Statement,
    ctx: &ExecutionContext,
    eph: &EphContext,
) -> ScopeContext {
    let mut has_children = false;
    let mut any_transparent = false;
    let mut any_display_uncomputed = false;
    let mut any_probe_resolved = false;
    let mut selected_ids: Vec<i64> = Vec::new();
    let mut unselected_filters: Vec<CompositeFilter> = Vec::new();

    for child in statement.children() {
        has_children = true;
        if child.is_computed(ctx) {
            match child.get_selection(ctx) {
                Some(sel) => selected_ids.extend(sel.get_instance_ids()),
                None => any_transparent = true,
            }
        } else {
            if !originates_data(&child) {
                any_display_uncomputed = true;
            }
            // A probe-resolved child contributes its exact id set instead
            // of a condition the index would have to materialise.  An
            // EMPTY resolved set still forces the Scope branch below — it
            // means "this child matches nothing", exactly like its
            // condition resolving to an empty scope, never Unscoped.
            if let Some(ids) = ctx
                .probe_resolved
                .get(&crate::execution_context::statement_key(&child))
            {
                any_probe_resolved = true;
                selected_ids.extend(ids.iter().copied());
            } else if let Some(f) = child.command().get_selector_composite_filter(eph) {
                unselected_filters.push(f);
            }
        }
    }

    if !has_children {
        return ScopeContext::Skip;
    }

    let combined_filter = if unselected_filters.is_empty() {
        None
    } else {
        Some(CompositeFilter::or(unselected_filters))
    };

    if selected_ids.is_empty() && combined_filter.is_none() && !any_probe_resolved {
        if any_transparent || any_display_uncomputed {
            // A weak/unit child ("show my children") or a computed-transparent
            // one contributes no selection of its own — the eager unscoped
            // neighbourhood query is what materialises the display edges.
            ScopeContext::Unscoped
        } else {
            // Every filter-less child (if any) is a data-originating statement
            // that will compute its own selection and relationship edges —
            // DEFER (Skip) the eager neighbourhood; constraints accept either
            // side's edge evidence and the graph renders the relationship from
            // the computed side.  This is what stops a broad container
            // (`mod("x") { func { … } }`) from materialising millions of edge
            // rows it then throws away.  (No children at all was returned as
            // Skip above.)
            ScopeContext::Skip
        }
    } else {
        ScopeContext::Scope {
            ids: selected_ids,
            filter: combined_filter,
        }
    }
}
