//! Which references a read asks for.
//!
//! A READ OF REFERENCES READS THE ROWS ITS TEST ADMITS.
//!
//! An edge of a query is containment, or a reference that passes a test
//! ([`crate::edge::RefTest`]): any row of the reference table, the ordinary
//! references, those stating a role, the rows that are a relation.  Every
//! read that attributes a reference to a declaration takes the test of the
//! edge it reads for and puts it where the reference row is read, so a tested
//! read costs what passes and its limit counts what passes.
//!
//! This is the ONE place a test is written as SQL, as
//! [`super::attribution::belongs_to`] is for attribution.  The reads render it
//! from here: the two reference families and the derive reads (through
//! `build_parents_query` / `build_children_query`), the probe's roles, its
//! walk and its tests, and the ask.  In memory the same test is
//! [`RefTest::admits`], on the two columns this reads; a test pins that the
//! two agree row by row.
//!
//! EVERY ROW IS NO CONDITION.  [`RefTest::EVERY`] renders nothing at all, so
//! a read that asks for any reference is the statement it was before there
//! were tests -- same text, same plan, same cache key.  A narrower test is
//! literal text and so part of the SQL-cache key: two reads under two tests
//! never share a result.
//!
//! Cost.  The test reads `relation` and `roles`.  Both are in
//! `symbol_refs_layer_uq` (`INCLUDE`, migration `2026-11-22-000001`), so a
//! read that enters the references by target symbol stays index-only.

use crate::edge::{RefTest, Relations};

/// SQL: the reference row `reference` passes `test`.  `None` when every row
/// does and there is nothing to say.
///
/// `reference` names a row with `relation` and `roles` (a `symbol_refs` row).
/// The result is parenthesised: it can be ANDed to anything.
pub(crate) fn passes(reference: &str, test: RefTest) -> Option<String> {
    if test.is_every() {
        return None;
    }
    // An ordinary reference is a row that is no relation this engine knows
    // (`Relation::from_stored`): written as "not one of them" so that a
    // relation a newer indexer wrote reads here as it does in memory.
    let known = Relations::ALL
        .iter()
        .map(|relation| (relation as i16).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let ordinary = format!("{reference}.relation NOT IN ({known})");

    let mut arms: Vec<String> = Vec::new();
    if test.any_ordinary() {
        arms.push(ordinary);
    } else if test.roles().bits() != 0 {
        arms.push(format!(
            "({ordinary} AND ({reference}.roles & {}) <> 0)",
            test.roles().bits()
        ));
    }
    for relation in test.relations().iter() {
        arms.push(format!("{reference}.relation = {}", relation as i16));
    }
    Some(format!("({})", arms.join(" OR ")))
}

/// [`passes`] as a conjunct to append: `" AND (…)"`, or nothing at all when
/// every row passes.
pub(crate) fn and_passes(reference: &str, test: RefTest) -> String {
    passes(reference, test)
        .map(|sql| format!(" AND {sql}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{and_passes, passes};
    use crate::edge::RefTest;
    use crate::roles::{Relation, Roles};

    #[test]
    fn every_row_is_no_condition() {
        assert_eq!(passes("sr", RefTest::EVERY), None);
        assert_eq!(and_passes("sr", RefTest::EVERY), "");
        // However it was put together.
        let all = RefTest::ORDINARY
            .or(RefTest::relation(Relation::Declares))
            .or(RefTest::relation(Relation::Holds));
        assert_eq!(passes("sr", all), None);
    }

    #[test]
    fn an_ordinary_reference_is_no_relation() {
        assert_eq!(
            passes("sr", RefTest::ORDINARY).as_deref(),
            Some("(sr.relation NOT IN (1, 2))")
        );
    }

    #[test]
    fn a_role_is_a_bit_of_an_ordinary_reference() {
        let calls = RefTest::stating(Roles::CALL).unwrap();
        assert_eq!(
            passes("t", calls).as_deref(),
            Some("((t.relation NOT IN (1, 2) AND (t.roles & 1) <> 0))")
        );
        let calls_or_writes = RefTest::stating(Roles::CALL | Roles::WRITE).unwrap();
        assert_eq!(
            passes("t", calls_or_writes).as_deref(),
            Some("((t.relation NOT IN (1, 2) AND (t.roles & 3) <> 0))")
        );
    }

    #[test]
    fn a_relation_is_its_stored_value() {
        assert_eq!(
            passes("sr", RefTest::relation(Relation::Holds)).as_deref(),
            Some("(sr.relation = 2)")
        );
        assert_eq!(
            passes("sr", RefTest::relation(Relation::Declares)).as_deref(),
            Some("(sr.relation = 1)")
        );
    }

    #[test]
    fn a_union_is_a_disjunction() {
        let calls_or_holds = RefTest::stating(Roles::CALL)
            .unwrap()
            .or(RefTest::relation(Relation::Holds));
        assert_eq!(
            and_passes("sr", calls_or_holds),
            " AND ((sr.relation NOT IN (1, 2) AND (sr.roles & 1) <> 0) OR sr.relation = 2)"
        );
        let refs_or_declares = RefTest::ORDINARY.or(RefTest::relation(Relation::Declares));
        assert_eq!(
            and_passes("sr", refs_or_declares),
            " AND (sr.relation NOT IN (1, 2) OR sr.relation = 1)"
        );
    }
}
