//! What a reference says about its target.
//!
//! **A reference carries the set of things its token does to the target; a
//! relation is a fact between two symbols that no token of the source
//! spells.**
//!
//! * A *role* is one bit on a reference: [`Roles::CALL`], [`Roles::WRITE`],
//!   [`Roles::TAKE`].  Every bit is a positive fact about the text and an
//!   absent bit states nothing, so zero (an index written before roles
//!   existed, a name in a `#define` body, a type reference) reads "refers to"
//!   and nothing more.  A role is what the text does, not what runs.
//! * A *relation* is a kind on the row ([`Relation`]): the macro declared it,
//!   the slot holds it.  Zero for an ordinary reference.
//!
//! An edge of an answer is a pair of symbols and many references, so what is
//! [`Stated`] about it is the union over them, and [`Stated::kinds`] is the
//! one place that union becomes the words every output prints.  No reader of
//! an answer has to guess them from the two symbol types.

use serde::{Deserialize, Serialize};

use crate::symbols::SymbolType;

/// The role bits of one reference, as stored (`symbol_refs.roles`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Roles(i16);

impl Roles {
    /// Nothing is stated.
    pub const NONE: Roles = Roles(0);
    /// The token is the callee of a call: `foo(x)`, `s->cb(x)`.
    pub const CALL: Roles = Roles(1);
    /// The target is assigned or initialised: `s->cb = f`, `n++`.
    pub const WRITE: Roles = Roles(2);
    /// Its value or address is taken, not called: `request_irq(n, handler)`,
    /// `g = s->count`.
    pub const TAKE: Roles = Roles(4);

    /// The bits a row carries.  Bits this engine does not know are kept and
    /// never printed, so a newer indexer's index still loads.
    pub const fn from_bits(bits: i16) -> Roles {
        Roles(bits)
    }

    pub const fn bits(self) -> i16 {
        self.0
    }

    pub const fn contains(self, role: Roles) -> bool {
        self.0 & role.0 == role.0
    }
}

impl std::ops::BitOr for Roles {
    type Output = Roles;
    fn bitor(self, other: Roles) -> Roles {
        Roles(self.0 | other.0)
    }
}

/// What a row is, when it is not something a token says (`symbol_refs
/// .relation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i16)]
pub enum Relation {
    /// The source's invocation declared the target: `DEFINE_MUTEX → lock`.
    Declares = 1,
    /// The source is a dispatch point and the target something stored in it:
    /// `file_operations.read → bt_bmc_read`.
    Holds = 2,
}

impl Relation {
    /// The relation a stored value names.  `None` for an ordinary reference,
    /// and for a relation this engine does not know.
    pub fn from_stored(value: i16) -> Option<Relation> {
        match value {
            x if x == Relation::Declares as i16 => Some(Relation::Declares),
            x if x == Relation::Holds as i16 => Some(Relation::Holds),
            _ => None,
        }
    }
}

/// A word an answer prints for what an edge does or is.  Declared in the
/// order they are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Calls,
    Writes,
    /// A function's value or address is taken: passed, stored, returned.
    Takes,
    /// The value of data or of a field is taken.
    Reads,
    Declares,
    Holds,
}

impl RefKind {
    /// The word as an answer prints it.  The JSON spelling and the report's
    /// are one: both come from here.
    pub fn word(self) -> &'static str {
        match self {
            RefKind::Calls => "calls",
            RefKind::Writes => "writes",
            RefKind::Takes => "takes",
            RefKind::Reads => "reads",
            RefKind::Declares => "declares",
            RefKind::Holds => "holds",
        }
    }
}

/// What is stated about a reference, or about a pair of symbols: the union
/// over the pair's references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Stated {
    roles: Roles,
    /// Some row is an ordinary reference: a token that names the target.
    plain: bool,
    declares: bool,
    holds: bool,
}

impl Stated {
    /// What one stored row states.
    pub fn of(roles: i16, relation: i16) -> Stated {
        let relation = Relation::from_stored(relation);
        Stated {
            roles: Roles::from_bits(roles),
            // A relation this engine does not know is not printed as one, so
            // its row reads as what every row is underneath: a reference.
            plain: relation.is_none(),
            declares: relation == Some(Relation::Declares),
            holds: relation == Some(Relation::Holds),
        }
    }

    /// Sightings merge by union: no fact is ever dropped for another.
    pub fn add(&mut self, other: Stated) {
        self.roles = self.roles | other.roles;
        self.plain |= other.plain;
        self.declares |= other.declares;
        self.holds |= other.holds;
    }

    pub fn roles(&self) -> Roles {
        self.roles
    }

    /// The pair REFERS: some token of the source names the target.
    ///
    /// A relation row is not such a token (the macro declared it, the slot
    /// holds it, and nothing where the row sits spells the target), so a pair
    /// that has only relation rows does not refer.  Every other pair does,
    /// including one nothing was recorded about: an edge exists because a
    /// reference does.
    pub fn refers(&self) -> bool {
        self.plain || !(self.declares || self.holds)
    }

    /// The words for what is stated, in the order of [`RefKind`]; empty when
    /// nothing is.
    ///
    /// The target's type picks one word and only one: a value taken is
    /// "takes" for a function (its address is what travels) and "reads" for
    /// anything else.  Every other word is the bit or the relation itself.
    pub fn kinds(&self, target: SymbolType) -> Vec<RefKind> {
        let mut kinds = Vec::new();
        if self.roles.contains(Roles::CALL) {
            kinds.push(RefKind::Calls);
        }
        if self.roles.contains(Roles::WRITE) {
            kinds.push(RefKind::Writes);
        }
        if self.roles.contains(Roles::TAKE) {
            kinds.push(if target == SymbolType::Function {
                RefKind::Takes
            } else {
                RefKind::Reads
            });
        }
        if self.declares {
            kinds.push(RefKind::Declares);
        }
        if self.holds {
            kinds.push(RefKind::Holds);
        }
        kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_and_the_json_spell_a_kind_the_same_way() {
        for kind in [
            RefKind::Calls,
            RefKind::Writes,
            RefKind::Takes,
            RefKind::Reads,
            RefKind::Declares,
            RefKind::Holds,
        ] {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.word())
            );
        }
    }

    const CALL: i16 = 1;
    const WRITE: i16 = 2;
    const TAKE: i16 = 4;

    #[test]
    fn nothing_stated_prints_nothing() {
        // An old index, a `#define` body token, a type reference.
        for target in [SymbolType::Function, SymbolType::Type, SymbolType::Macro] {
            assert!(Stated::of(0, 0).kinds(target).is_empty());
        }
        assert!(Stated::default().kinds(SymbolType::Function).is_empty());
    }

    #[test]
    fn a_pair_refers_unless_all_it_has_is_relation_rows() {
        // An ordinary reference, whatever it states, and a pair nothing was
        // recorded about: an edge exists because a reference does.
        assert!(Stated::of(0, 0).refers());
        assert!(Stated::of(CALL, 0).refers());
        assert!(Stated::default().refers());
        // The slot holds it, the macro declared it: no token names the target.
        assert!(!Stated::of(0, 2).refers());
        assert!(!Stated::of(0, 1).refers());
        // Both kinds of row between one pair, in either order.
        for rows in [[(0, 2), (TAKE, 0)], [(TAKE, 0), (0, 2)]] {
            let mut pair = Stated::default();
            for (roles, relation) in rows {
                pair.add(Stated::of(roles, relation));
            }
            assert!(pair.refers());
            assert_eq!(
                pair.kinds(SymbolType::Function),
                vec![RefKind::Takes, RefKind::Holds]
            );
        }
        // A relation this engine does not know reads as a reference.
        assert!(Stated::of(0, 77).refers());
    }

    #[test]
    fn each_role_is_its_own_word() {
        assert_eq!(
            Stated::of(CALL, 0).kinds(SymbolType::Function),
            vec![RefKind::Calls]
        );
        // A call through a slot is a call, whatever the target is.
        assert_eq!(
            Stated::of(CALL, 0).kinds(SymbolType::Field),
            vec![RefKind::Calls]
        );
        assert_eq!(
            Stated::of(WRITE, 0).kinds(SymbolType::Field),
            vec![RefKind::Writes]
        );
    }

    #[test]
    fn a_value_taken_is_takes_for_a_function_and_reads_otherwise() {
        assert_eq!(
            Stated::of(TAKE, 0).kinds(SymbolType::Function),
            vec![RefKind::Takes],
            "`request_irq(n, handler)`"
        );
        for target in [SymbolType::Data, SymbolType::Field] {
            assert_eq!(
                Stated::of(TAKE, 0).kinds(target),
                vec![RefKind::Reads],
                "`g = s->count`"
            );
        }
    }

    #[test]
    fn one_token_can_do_two_things() {
        // `n++`
        assert_eq!(
            Stated::of(WRITE | TAKE, 0).kinds(SymbolType::Data),
            vec![RefKind::Writes, RefKind::Reads]
        );
    }

    #[test]
    fn a_relation_is_its_own_word() {
        assert_eq!(
            Stated::of(0, 1).kinds(SymbolType::Data),
            vec![RefKind::Declares]
        );
        assert_eq!(
            Stated::of(0, 2).kinds(SymbolType::Function),
            vec![RefKind::Holds]
        );
    }

    #[test]
    fn a_pair_states_the_union_of_its_references() {
        let mut pair = Stated::default();
        pair.add(Stated::of(CALL, 0));
        pair.add(Stated::of(TAKE, 0));
        pair.add(Stated::of(WRITE | TAKE, 0));
        pair.add(Stated::of(0, 2));
        assert_eq!(
            pair.kinds(SymbolType::Field),
            vec![
                RefKind::Calls,
                RefKind::Writes,
                RefKind::Reads,
                RefKind::Holds
            ]
        );
        // Adding what is already stated changes nothing.
        let before = pair;
        pair.add(Stated::of(CALL, 0));
        assert_eq!(pair, before);
    }

    #[test]
    fn what_a_newer_indexer_says_is_kept_and_not_printed() {
        let unknown = Stated::of(CALL | 64, 9);
        assert_eq!(unknown.kinds(SymbolType::Function), vec![RefKind::Calls]);
        assert_eq!(unknown.roles().bits(), CALL | 64);
    }

    #[test]
    fn the_words_are_lowercase_on_the_wire() {
        let json = serde_json::to_string(&vec![RefKind::Calls, RefKind::Reads]).unwrap();
        assert_eq!(json, r#"["calls","reads"]"#);
    }
}
