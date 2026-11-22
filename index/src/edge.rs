//! What a scope asks of the edge between two statements.
//!
//! **An edge is containment, or a reference that passes a test.**
//!
//! A scope relates a statement to the statements in its braces, and there are
//! four relationships it can ask for, the four marks an answer draws:
//! containment (`has`), an ordinary reference (`refs`), and the two relations
//! no token spells (`declares`, `holds`).  The role words (`calls`, `writes`,
//! `takes`) narrow the ordinary reference to one that states that role.
//!
//! Containment is a table of its own and is asked for or not: [`Edge::has`].
//! The other three are rows of ONE table, the reference table, so what a
//! scope asks of them is a test on a row, [`RefTest`]: which ordinary
//! references pass, and which relations.  Relationship words join with `or`
//! only, so a test is a union and a union of tests is a test
//! ([`RefTest::or`]): every sentence of the language is one condition on a
//! reference row, plus containment.
//!
//! A bare `{ }` asks for all four.  For the reference table that is EVERY
//! row, [`RefTest::EVERY`], which is no condition at all: a read under it is
//! the read it always was.

use crate::roles::{Relation, Roles};

/// Which rows of the reference table an edge admits.
///
/// Never empty: an edge that admits no reference has no test
/// (`Edge::refs() == None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefTest {
    /// Every ordinary reference passes, whatever it states and also when it
    /// states nothing: the word `refs`.
    any: bool,
    /// An ordinary reference passes when it states one of these roles: the
    /// words `calls`, `writes`, `takes`.  Empty when `any` is set, which
    /// admits them all already.
    roles: Roles,
    /// The relation rows that pass: the words `declares`, `holds`.
    relations: Relations,
}

/// A set of relations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Relations {
    declares: bool,
    holds: bool,
}

impl Relations {
    pub const NONE: Relations = Relations {
        declares: false,
        holds: false,
    };
    pub const ALL: Relations = Relations {
        declares: true,
        holds: true,
    };

    pub const fn of(relation: Relation) -> Relations {
        match relation {
            Relation::Declares => Relations {
                declares: true,
                holds: false,
            },
            Relation::Holds => Relations {
                declares: false,
                holds: true,
            },
        }
    }

    pub const fn contains(self, relation: Relation) -> bool {
        match relation {
            Relation::Declares => self.declares,
            Relation::Holds => self.holds,
        }
    }

    pub const fn is_empty(self) -> bool {
        !self.declares && !self.holds
    }

    const fn or(self, other: Relations) -> Relations {
        Relations {
            declares: self.declares || other.declares,
            holds: self.holds || other.holds,
        }
    }

    /// The relations in the set, in the order they are stored.
    pub fn iter(self) -> impl Iterator<Item = Relation> {
        [Relation::Declares, Relation::Holds]
            .into_iter()
            .filter(move |relation| self.contains(*relation))
    }
}

impl RefTest {
    /// Every row of the reference table: ordinary references and both
    /// relations.  What a bare `{ }` asks, and what "a reference" has always
    /// meant to a read.
    pub const EVERY: RefTest = RefTest {
        any: true,
        roles: Roles::NONE,
        relations: Relations::ALL,
    };

    /// Every ordinary reference: a token that names the target.
    pub const ORDINARY: RefTest = RefTest {
        any: true,
        roles: Roles::NONE,
        relations: Relations::NONE,
    };

    /// The ordinary references that state one of `roles`.  `None` when
    /// `roles` is empty: a test admits something.
    pub fn stating(roles: Roles) -> Option<RefTest> {
        (roles != Roles::NONE).then_some(RefTest {
            any: false,
            roles,
            relations: Relations::NONE,
        })
    }

    /// The rows of one relation.
    pub const fn relation(relation: Relation) -> RefTest {
        RefTest {
            any: false,
            roles: Roles::NONE,
            relations: Relations::of(relation),
        }
    }

    /// Either: the union of what the two admit.  "Every ordinary reference"
    /// absorbs "the ordinary references that state a role".
    pub fn or(self, other: RefTest) -> RefTest {
        let any = self.any || other.any;
        RefTest {
            any,
            roles: if any {
                Roles::NONE
            } else {
                self.roles | other.roles
            },
            relations: self.relations.or(other.relations),
        }
    }

    /// The test admits every row, so a read under it needs no condition.
    pub fn is_every(self) -> bool {
        self == RefTest::EVERY
    }

    /// Everything this test admits, `wider` admits too.  A read made under
    /// `wider` therefore holds every row this test asks about.
    pub fn within(self, wider: RefTest) -> bool {
        self.or(wider) == wider
    }

    /// Whether a stored row passes: its `roles` and `relation` columns.
    ///
    /// A relation this engine does not know is not one of the two it can be
    /// asked for, so its row is what every row is underneath, an ordinary
    /// reference (as `Stated::of` reads it).
    pub fn admits(self, roles: i16, relation: i16) -> bool {
        match Relation::from_stored(relation) {
            Some(relation) => self.relations.contains(relation),
            None => self.any || roles & self.roles.bits() != 0,
        }
    }

    /// Every ordinary reference passes.
    pub fn any_ordinary(self) -> bool {
        self.any
    }

    /// The roles one of which an ordinary reference must state, when not
    /// every ordinary reference passes.
    pub fn roles(self) -> Roles {
        self.roles
    }

    /// The relations whose rows pass.
    pub fn relations(self) -> Relations {
        self.relations
    }
}

/// What a scope asks of an edge: containment, references under a test, or
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    has: bool,
    refs: Option<RefTest>,
}

impl Edge {
    /// Containment only: the word `has`.
    pub const HAS: Edge = Edge {
        has: true,
        refs: None,
    };

    /// Every row of the reference table and no containment.
    pub const EVERY_REFERENCE: Edge = Edge {
        has: false,
        refs: Some(RefTest::EVERY),
    };

    /// All four relationships: a bare `{ }`.
    pub const ALL: Edge = Edge {
        has: true,
        refs: Some(RefTest::EVERY),
    };

    /// The references that pass `test`, and no containment.
    pub const fn references(test: RefTest) -> Edge {
        Edge {
            has: false,
            refs: Some(test),
        }
    }

    /// The edge of these parts: `None` when there is neither containment nor
    /// a reference test, which is no edge.  For what is left of an edge when
    /// part of it is settled (`Unproven`).
    pub const fn of(has: bool, refs: Option<RefTest>) -> Option<Edge> {
        if has || refs.is_some() {
            Some(Edge { has, refs })
        } else {
            None
        }
    }

    /// Either: a pair is related if it is related by one or the other.
    pub fn or(self, other: Edge) -> Edge {
        Edge {
            has: self.has || other.has,
            refs: match (self.refs, other.refs) {
                (Some(a), Some(b)) => Some(a.or(b)),
                (a, b) => a.or(b),
            },
        }
    }

    /// The edge accepts containment.
    pub fn has(self) -> bool {
        self.has
    }

    /// The test a reference must pass for the edge to accept it; `None` when
    /// the edge accepts no reference.
    pub fn refs(self) -> Option<RefTest> {
        self.refs
    }
}

impl std::ops::BitOr for Edge {
    type Output = Edge;
    fn bitor(self, other: Edge) -> Edge {
        self.or(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARES: i16 = Relation::Declares as i16;
    const HOLDS: i16 = Relation::Holds as i16;
    const CALL: i16 = Roles::CALL.bits();
    const WRITE: i16 = Roles::WRITE.bits();
    const TAKE: i16 = Roles::TAKE.bits();

    fn calls() -> RefTest {
        RefTest::stating(Roles::CALL).unwrap()
    }

    #[test]
    fn every_admits_every_row_whatever_it_states_or_is() {
        for roles in [0, CALL, WRITE | TAKE, 64] {
            for relation in [0, DECLARES, HOLDS, 9] {
                assert!(RefTest::EVERY.admits(roles, relation));
            }
        }
        assert!(RefTest::EVERY.is_every());
    }

    #[test]
    fn an_ordinary_reference_is_one_that_is_no_relation() {
        assert!(RefTest::ORDINARY.admits(0, 0), "one that states nothing");
        assert!(RefTest::ORDINARY.admits(CALL, 0));
        assert!(!RefTest::ORDINARY.admits(0, DECLARES));
        assert!(!RefTest::ORDINARY.admits(0, HOLDS));
        // A relation this engine cannot be asked for is a reference
        // underneath, as an answer draws it.
        assert!(RefTest::ORDINARY.admits(0, 9));
        assert!(!RefTest::ORDINARY.is_every());
    }

    #[test]
    fn a_role_word_admits_the_references_that_state_it_and_no_other() {
        assert!(calls().admits(CALL, 0));
        assert!(calls().admits(CALL | TAKE, 0), "a token can do two things");
        assert!(!calls().admits(WRITE | TAKE, 0));
        // Unstated is unknown, never "yes".
        assert!(!calls().admits(0, 0));
        // A relation row is not a token: it states no role.
        assert!(!calls().admits(CALL, HOLDS));
        assert_eq!(
            RefTest::stating(Roles::NONE),
            None,
            "a test admits something"
        );
    }

    #[test]
    fn a_relation_word_admits_its_rows_and_no_other() {
        let holds = RefTest::relation(Relation::Holds);
        assert!(holds.admits(0, HOLDS));
        assert!(!holds.admits(0, DECLARES));
        assert!(!holds.admits(CALL, 0));
    }

    #[test]
    fn or_is_the_union_of_what_each_admits() {
        let either = calls().or(RefTest::relation(Relation::Holds));
        assert!(either.admits(CALL, 0));
        assert!(either.admits(0, HOLDS));
        assert!(!either.admits(WRITE, 0));
        assert!(!either.admits(0, DECLARES));

        let calls_or_writes = calls().or(RefTest::stating(Roles::WRITE).unwrap());
        assert!(calls_or_writes.admits(WRITE, 0));
        assert!(!calls_or_writes.admits(TAKE, 0));

        // Commutative and idempotent.
        assert_eq!(
            either,
            RefTest::relation(Relation::Holds).or(calls()),
            "order does not matter"
        );
        assert_eq!(either.or(either), either);
    }

    #[test]
    fn every_ordinary_reference_absorbs_the_ones_that_state_a_role() {
        assert_eq!(RefTest::ORDINARY.or(calls()), RefTest::ORDINARY);
        assert_eq!(calls().or(RefTest::ORDINARY), RefTest::ORDINARY);
        assert_eq!(RefTest::EVERY.or(calls()), RefTest::EVERY);
    }

    #[test]
    fn the_three_reference_relationships_together_are_every_row() {
        let all = RefTest::ORDINARY
            .or(RefTest::relation(Relation::Declares))
            .or(RefTest::relation(Relation::Holds));
        assert_eq!(all, RefTest::EVERY);
        assert!(all.is_every(), "so a bare scope adds no condition");
    }

    #[test]
    fn a_narrower_test_is_within_a_wider_one() {
        assert!(calls().within(RefTest::ORDINARY));
        assert!(calls().within(RefTest::EVERY));
        assert!(RefTest::ORDINARY.within(RefTest::EVERY));
        assert!(calls().within(calls()));
        assert!(!RefTest::ORDINARY.within(calls()));
        assert!(!RefTest::relation(Relation::Holds).within(RefTest::ORDINARY));
        assert!(!RefTest::EVERY.within(RefTest::ORDINARY));
    }

    #[test]
    fn an_edge_is_containment_or_references_under_a_test() {
        assert!(Edge::HAS.has());
        assert_eq!(Edge::HAS.refs(), None);
        assert!(!Edge::EVERY_REFERENCE.has());
        assert_eq!(Edge::EVERY_REFERENCE.refs(), Some(RefTest::EVERY));
        // A bare scope: all four relationships.
        assert_eq!(Edge::HAS | Edge::EVERY_REFERENCE, Edge::ALL);
        assert_eq!(Edge::ALL.refs(), Some(RefTest::EVERY));
    }

    #[test]
    fn edges_join_like_their_tests() {
        let calls_or_has = Edge::references(calls()) | Edge::HAS;
        assert!(calls_or_has.has());
        assert_eq!(calls_or_has.refs(), Some(calls()));

        let two = Edge::references(calls()) | Edge::references(RefTest::relation(Relation::Holds));
        assert_eq!(
            two.refs(),
            Some(calls().or(RefTest::relation(Relation::Holds)))
        );
        assert!(!two.has());
        assert_eq!(Edge::HAS | Edge::HAS, Edge::HAS);
    }

    #[test]
    fn an_edge_of_no_part_is_no_edge() {
        assert_eq!(Edge::of(false, None), None);
        assert_eq!(Edge::of(true, None), Some(Edge::HAS));
        assert_eq!(
            Edge::of(false, Some(RefTest::EVERY)),
            Some(Edge::EVERY_REFERENCE)
        );
        assert_eq!(Edge::of(true, Some(RefTest::EVERY)), Some(Edge::ALL));
        assert_eq!(
            Edge::of(true, Some(calls())),
            Some(Edge::references(calls()) | Edge::HAS)
        );
    }
}
