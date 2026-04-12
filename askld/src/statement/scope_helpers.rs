use crate::execution_context::ExecutionContext;
use crate::hierarchy::Hierarchy;
use index::db_diesel::{CompositeFilter, ScopeContext};

use super::Statement;

/// Whether a child should be excluded from the bottom-up parent merge.
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
/// We distinguish the two structurally: if every descendant of the child is
/// weak, no node below could have originated data, so any selection is
/// necessarily a top-down echo (case 1) — skip it.  If a non-weak descendant
/// exists, real data could have flowed up (case 2) — include it.
///
/// A direct-children check (`child.children().all(weak)`) is insufficient
/// because `mark_weak_statements` propagates weakness downward via the
/// `parent_weak` rule: a statement can be weak (from its parent) while having
/// a non-weak child of its own.  So a weak grandchild may still carry data
/// from a non-weak great-grandchild.  We therefore recurse the full subtree.
pub(super) fn should_skip_in_parent_merge(child: &Statement) -> bool {
    child.get_state().weak && child.command().is_unit() && all_descendants_weak(child)
}

fn all_descendants_weak(stmt: &Statement) -> bool {
    stmt.children()
        .all(|child| child.get_state().weak && all_descendants_weak(&child))
}

/// Build scope context for the parent side of a statement's parent query.
/// If the parent already has a selection, use its instance IDs.
/// If no parent exists, return Skip.
/// If the parent hasn't been selected yet, fall back to mixin-based scoping.
pub(super) fn build_parent_scope(statement: &Statement, ctx: &ExecutionContext) -> ScopeContext {
    match statement.parent().and_then(|p| p.upgrade()) {
        Some(parent) => {
            if parent.is_computed(ctx) {
                match parent.get_selection(ctx) {
                    Some(sel) => ScopeContext::Scope { ids: sel.get_instance_ids(), filter: None },
                    // None = parent has no opinion (filter-only, unit, or no selectors).
                    // Run unscoped — the parent is transparent.
                    None => ScopeContext::Unscoped,
                }
            } else {
                // Parent not yet computed — fall back to filter-based scoping
                match parent.command().get_selector_composite_filter() {
                    Some(f) => ScopeContext::Scope { ids: vec![], filter: Some(f) },
                    None => ScopeContext::Unscoped,
                }
            }
        },
        None => ScopeContext::Unscoped, // Root-level: run parents unscoped
    }
}

/// Build scope context for the children side of a statement's children query.
/// Collects instance IDs from already-selected children + filters from unselected children.
/// If no children exist, return Skip.
pub(super) fn build_children_scope(statement: &Statement, ctx: &ExecutionContext) -> ScopeContext {
    let mut has_children = false;
    let mut any_uncomputed = false;
    let mut any_transparent = false;
    let mut selected_ids: Vec<i32> = Vec::new();
    let mut unselected_filters: Vec<CompositeFilter> = Vec::new();

    for child in statement.children() {
        has_children = true;
        if child.is_computed(ctx) {
            match child.get_selection(ctx) {
                Some(sel) => selected_ids.extend(sel.get_instance_ids()),
                None => any_transparent = true,
            }
        } else {
            any_uncomputed = true;
            if let Some(f) = child.command().get_selector_composite_filter() {
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

    if selected_ids.is_empty() && combined_filter.is_none() {
        if any_uncomputed || any_transparent {
            ScopeContext::Unscoped
        } else {
            ScopeContext::Skip
        }
    } else {
        ScopeContext::Scope { ids: selected_ids, filter: combined_filter }
    }
}
