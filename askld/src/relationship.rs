//! The relationship a scope asks for, with the words that asked for it.
//!
//! **A relationship word selects the edges drawn with its mark, and words
//! join with `or`.**
//!
//! There are four relationships, one per mark an answer draws, and they are
//! disjoint: `has` (`▸`, containment), `refs` (`→`, an ordinary reference: a
//! token of the parent names the child), `declares` (`↦`, the parent's
//! invocation declared the child) and `holds` (`⇢`, the parent is a slot and
//! the child is stored in it, or an interface method and an implementer).
//! A bare `{ }` asks for all four.
//!
//! Three more words narrow `refs` to the references that STATE what they do
//! to the child: `calls` (the token is the callee of a call), `writes` (the
//! child is assigned or initialised) and `takes` (its value or address is
//! taken, not called).  `reads` is `takes` under the name an answer prints
//! for data and fields: one bit, either word, any target.  AN UNSTATED ROLE
//! IS UNKNOWN, NEVER "NO" -- and never "yes" either, so no role word matches a
//! reference that states nothing; what a word passed over for that reason
//! the answer says (`crate::statement::passed_over`).
//!
//! `calls` is literal.  A call through a slot is a call of the slot; the
//! functions stored in it are one more hop, written
//! `calls { field holds { func } }`.
//!
//! A PAIR MATCHES IF ANY OF ITS ROWS DOES.  What the engine reads is the
//! union of what the words ask for, one [`Edge`]; this type keeps the words
//! beside it, in the order they were written, because an answer that says
//! what an expression passed over has to say it in the query's own words
//! (`reads` and `takes` are one bit and two spellings).

use index::edge::{Edge, RefTest};
use index::roles::{Relation, Roles};

/// What one relationship word asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asks {
    Has,
    Refs,
    Relation(Relation),
    Role(Roles),
}

/// One relationship word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Word {
    text: &'static str,
    /// The form after "they": "whether they `call` it".
    they: &'static str,
    asks: Asks,
}

impl Word {
    /// Every relationship word: the four relationships in the order their
    /// marks are drawn, then the roles.
    pub const ALL: [Word; 8] = [
        Word::new("has", "have", Asks::Has),
        Word::new("refs", "refer to", Asks::Refs),
        Word::new("declares", "declare", Asks::Relation(Relation::Declares)),
        Word::new("holds", "hold", Asks::Relation(Relation::Holds)),
        Word::new("calls", "call", Asks::Role(Roles::CALL)),
        Word::new("writes", "write", Asks::Role(Roles::WRITE)),
        Word::new("takes", "take", Asks::Role(Roles::TAKE)),
        Word::new("reads", "read", Asks::Role(Roles::TAKE)),
    ];

    const fn new(text: &'static str, they: &'static str, asks: Asks) -> Word {
        Word { text, they, asks }
    }

    /// The word spelled `text`, if it is a relationship word.
    pub fn named(text: &str) -> Option<Word> {
        Word::ALL.into_iter().find(|word| word.text == text)
    }

    /// The word as it is written.
    pub fn text(self) -> &'static str {
        self.text
    }

    /// The form after "they" (`call` for `calls`).
    pub fn they(self) -> &'static str {
        self.they
    }

    /// The role a role word asks for.
    pub fn role(self) -> Option<Roles> {
        match self.asks {
            Asks::Role(role) => Some(role),
            _ => None,
        }
    }

    /// The relation a relation word asks for.
    pub fn relation(self) -> Option<Relation> {
        match self.asks {
            Asks::Relation(relation) => Some(relation),
            _ => None,
        }
    }

    /// One of the four relationships (a mark of its own), not a role.
    pub fn is_relationship(self) -> bool {
        !matches!(self.asks, Asks::Role(_))
    }

    /// The word is `has`.
    pub fn is_has(self) -> bool {
        self.asks == Asks::Has
    }

    /// The edges the word selects.
    pub fn edge(self) -> Edge {
        match self.asks {
            Asks::Has => Edge::HAS,
            Asks::Refs => Edge::references(RefTest::ORDINARY),
            Asks::Relation(relation) => Edge::references(RefTest::relation(relation)),
            Asks::Role(role) => Edge::references(
                RefTest::stating(role).expect("a role word names a role: the table above"),
            ),
        }
    }
}

/// The relationship a scope asks for: the edge to read, and the words that
/// asked for it.
///
/// The two cannot disagree: the edge is the union of the words' edges, built
/// here and nowhere else.  A scope that wrote no word has none
/// ([`Relationship::unwritten`]): the bare `{ }`, and a top-level statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    edge: Edge,
    words: Vec<Word>,
}

impl Relationship {
    /// The relationship of a scope that wrote no word.
    pub fn unwritten(edge: Edge) -> Relationship {
        Relationship {
            edge,
            words: Vec::new(),
        }
    }

    /// The relationship one word asks for.
    pub fn of(word: Word) -> Relationship {
        Relationship {
            edge: word.edge(),
            words: vec![word],
        }
    }

    /// Either: the union of the edges, and the words of both in the order
    /// they were written, each once.
    pub fn or(mut self, other: Relationship) -> Relationship {
        self.edge = self.edge | other.edge;
        for word in other.words {
            if !self.words.contains(&word) {
                self.words.push(word);
            }
        }
        self
    }

    /// What the engine reads.
    pub fn edge(&self) -> Edge {
        self.edge
    }

    /// The words, in the order written.  Empty for a scope that wrote none.
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    /// The expression as the query wrote it, without its parentheses:
    /// `calls or holds`.  `None` for a scope that wrote no word.
    pub fn written(&self) -> Option<String> {
        (!self.words.is_empty()).then(|| join(self.words.iter().map(|word| word.text)))
    }
}

/// Relationship words joined the one way they join.
pub fn join<'a>(words: impl Iterator<Item = &'a str>) -> String {
    words.collect::<Vec<_>>().join(" or ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relationship_is_the_union_of_its_words() {
        let word = |text: &str| Relationship::of(Word::named(text).unwrap());
        let calls_or_holds = word("calls").or(word("holds"));
        assert_eq!(
            calls_or_holds.edge(),
            Word::named("calls").unwrap().edge() | Word::named("holds").unwrap().edge()
        );
        assert_eq!(calls_or_holds.written().as_deref(), Some("calls or holds"));

        // The four relationships are every edge there is.
        let all = word("has")
            .or(word("refs"))
            .or(word("declares"))
            .or(word("holds"));
        assert_eq!(all.edge(), Edge::ALL);
        // A word is kept once, in the place it was first written.
        assert_eq!(
            word("holds")
                .or(word("calls"))
                .or(word("holds"))
                .written()
                .as_deref(),
            Some("holds or calls")
        );
        // `reads` and `takes` are one bit and two words.
        assert_eq!(word("reads").edge(), word("takes").edge());
        assert_eq!(
            word("takes").or(word("reads")).written().as_deref(),
            Some("takes or reads")
        );
        // A scope that wrote nothing has no words.
        assert_eq!(Relationship::unwritten(Edge::ALL).written(), None);
        assert_eq!(Word::named("derive"), None);
    }
}
