//! Which declaration a reference belongs to.
//!
//! A REFERENCE BELONGS TO EVERY DECLARATION AROUND IT; A RELATION BELONGS TO
//! ITS SOURCE ONLY.
//!
//! A reference is a point in its object's DFS order, and a declaration is an
//! interval of it: something written inside a function is written inside the
//! file and the directory too, so the function, the file and the directory
//! each refer to what it names.
//!
//! A relation row (`symbol_refs.relation <> 0`: the macro DECLARES it, the
//! slot HOLDS it) is not something a token says where it stands.  The indexer
//! places it on the declaration of its source -- a `HOLDS` row on the field's
//! declaration, a `DECLARES` row on the macro's `#define` -- and nothing
//! around the source has that relation: a struct does not hold what its field
//! holds.  Attributed like a reference, `file_operations (type) ->
//! bt_bmc_open` appeared in answers, an arrow nobody could read.  So a
//! relation row belongs to the one declaration it sits directly in, which is
//! the rule `nested` already reads: the reference is one level below its
//! owner.  Ingest guarantees there is exactly one such declaration
//! (`validate_object_tree`: a reference's level is its innermost holder's
//! plus one).
//!
//! This is the ONE place that test is written.  Every read that attributes a
//! reference to a declaration renders it from here: the two families and the
//! derive reads (through `build_children_query` / `build_parents_query`), the
//! probe's roles, its walk and its tests, the ask, the edges among a set of
//! instances, the scope-resolution semi-joins and the three listings.
//!
//! Cost.  The test reads two columns of the reference (`relation`,
//! `nesting_level`).  A read that enters `symbol_refs` by target symbol is
//! index-only on `symbol_refs_layer_uq`, so that index carries both
//! (`INCLUDE`, migration `2026-11-22-000001`): with them only in the table,
//! every reference visited cost a row fetch and the probe's walk doubled
//! (`perf/PROBE_BASELINE.md`, "A relation belongs to its source").  The planner
//! needs statistics for `relation`; the boot-time refresh provides them.

/// SQL: the reference `reference` belongs to the declaration `owner`.
///
/// `owner` names a row with `dfs_enter`, `dfs_exit` and `nesting_level` (an
/// instance, or a relation carrying those columns); `reference` a row with
/// `dfs_enter`, `nesting_level` and `relation` (a `symbol_refs` row, or a
/// relation carrying those columns).  Both are in the same object: that
/// equality is the caller's join key and is not repeated here.
pub(crate) fn belongs_to(reference: &str, owner: &str) -> String {
    format!(
        "{owner}.dfs_enter < {reference}.dfs_enter AND {reference}.dfs_enter < {owner}.dfs_exit \
         AND ({reference}.relation = 0 OR {reference}.nesting_level = {owner}.nesting_level + 1)"
    )
}

#[cfg(test)]
mod tests {
    use super::belongs_to;

    #[test]
    fn a_relation_row_is_one_level_below_its_owner() {
        assert_eq!(
            belongs_to("sr", "pd"),
            "pd.dfs_enter < sr.dfs_enter AND sr.dfs_enter < pd.dfs_exit \
             AND (sr.relation = 0 OR sr.nesting_level = pd.nesting_level + 1)"
        );
    }
}
