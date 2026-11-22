//! What a relationship word passed over.
//!
//! **A filter that passed over what it would otherwise have answered says so,
//! once, with a count and the rewrite.**
//!
//! A relationship word selects the edges drawn with its mark, and the four
//! relationships are disjoint: `field("f") refs { func }` does not list what
//! the slot holds.  That is the word doing its job, and it is also the one
//! way an answer comes out empty with nothing wrong in the query.  So where a
//! statement's answer is EMPTY and the word is why, the answer says:
//!
//!  1. `refs` passed over relation rows: "`refs` lists only ordinary
//!     references (`→`); `field("f")` holds 212 symbols …".  The rows named
//!     are the ones a bare `{ }` would have listed there: the statement's own
//!     predicate, related to its neighbour by the relation.
//!  2. `holds` / `declares` was asked of something that cannot be its source:
//!     only a slot holds (a field, or an interface method) and only a macro
//!     declares.
//!
//! Worked out once per edge, after the statements have run, from what they
//! ended as.  An empty answer has no rows to start from, so the neighbour's
//! rows are taken from where they still are: its final selection, or what
//! its probe resolved before the narrowing emptied it
//! (`ExecutionContext::probe_resolved`, a superset of what it ends as).  The
//! reads are the derive's own (`find_child_instance_ids` /
//! `find_parent_instance_ids`) under the passed-over relation, so they are
//! bounded as it is (`read_cap + 1`, "at least" when cut) and answered from
//! the SQL cache as it is: a repeated query says what the first one said.
//!
//! Nothing is read unless a relationship word was written AND an answer came
//! out empty: not cut (that answer is said to be incomplete), and not empty
//! because its own words match nothing anywhere (that one is told so).

use std::rc::Rc;

use index::db_diesel::{
    CompositeFilter, DirectOnlyMixin, InnermostOnlyMixin, OuterParentFilterMixin,
    SymbolInstanceIdMixin, SymbolTypeMixin, TraversalMode, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_MACRO,
};
use index::edge::{Edge, RefTest, Relations};
use index::roles::Relation;
use index::symbols::SymbolInstanceId;

use super::Statement;
use crate::cfg::ControlFlowGraph;
use crate::diagnostic::Diagnostic;
use crate::execution_context::{statement_key, ExecutionContext};
use crate::hierarchy::Hierarchy;

/// The word, the mark and the verb form of a relation.
fn words(relation: Relation) -> (&'static str, &'static str, &'static str, &'static str) {
    match relation {
        // (word, mark, "X <verbs> them", "they <verb> X")
        Relation::Declares => ("declares", "↦", "declares", "declare"),
        Relation::Holds => ("holds", "⇢", "holds", "hold"),
    }
}

/// The only kind of symbol a relation can start at, and how to say so.
fn source_of(relation: Relation) -> (i32, &'static str) {
    match relation {
        Relation::Declares => (SYMBOL_TYPE_MACRO, "only a macro declares"),
        Relation::Holds => (
            SYMBOL_TYPE_FIELD,
            "only a slot holds (a field, or an interface method)",
        ),
    }
}

/// The rows a statement can still be asked about: what it ended as, or, when
/// narrowing emptied it, what its probe had resolved before.
fn rows(statement: &Rc<Statement>, ctx: &ExecutionContext) -> Option<Vec<i64>> {
    if let Some(selection) = statement.get_selection(ctx) {
        if !selection.is_empty() {
            return Some(selection.get_instance_ids());
        }
    }
    ctx.probe_resolved
        .get(&statement_key(statement))
        .filter(|ids| !ids.is_empty())
        .cloned()
}

/// The statement's answer is empty, and is all of what it means.
///
/// A cut selection is a subset, empty or not, and its statement is already
/// said to be incomplete: nothing here explains it.  It is also the
/// expensive case -- on the kernel `mod("amdgpu") refs { func }` leaves
/// `func` cut and empty, and reading what 931 module instances declare and
/// hold added 100 ms to a 250 ms query to say nothing.
fn answered_nothing(statement: &Rc<Statement>, ctx: &ExecutionContext) -> bool {
    statement
        .get_selection(ctx)
        .is_none_or(|selection| selection.is_empty() && !selection.cut())
}

/// The statement names nothing itself and lists what its neighbour relates
/// to.  A listing leaves folded symbols out; a statement that names a symbol
/// shows it folded or not.
fn is_listing(statement: &Rc<Statement>) -> bool {
    statement
        .command()
        .selectors()
        .all(|s| s.is_non_constraining_selector())
}

/// What the statement itself asks for, as the probe phase reads it: its
/// fused predicate, else its scope condition, else (a listing) its filters.
fn predicate(statement: &Rc<Statement>, ctx: &ExecutionContext) -> Option<CompositeFilter> {
    let command = statement.command();
    let listing = is_listing(statement);
    command
        .probe_predicate(&ctx.eph)
        .or_else(|| command.get_selector_composite_filter(&ctx.eph))
        .or_else(|| {
            listing.then(|| {
                command
                    .constraint_predicate(&ctx.eph)
                    .unwrap_or_else(|| CompositeFilter::and(vec![]))
            })
        })
}

/// One relation a `refs` passed over, as found.
struct Found {
    relation: Relation,
    names: Vec<String>,
    at_least: bool,
}

/// Which end of the edge answered nothing, and so is the one asked about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Child,
    Parent,
}

pub(super) async fn notices(
    statements: &[Rc<Statement>],
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> Vec<Diagnostic> {
    let mut said = Vec::new();
    for child in statements {
        let Some(parent) = child.parent().and_then(|p| p.upgrade()) else {
            continue;
        };
        let edge: Edge = child.get_relationship_type();
        // Containment is not a row of the reference table.  (A scope that
        // admits every such row passed over none: nothing below reads.)
        let Some(test) = edge.refs() else {
            continue;
        };
        if !answered_nothing(child, ctx) && !answered_nothing(&parent, ctx) {
            continue;
        }
        let notice = if test.any_ordinary() {
            passed_over_relations(child, &parent, test, ctx, cfg).await
        } else if test.roles().bits() == 0 {
            not_a_source(&parent, test, ctx, cfg).await
        } else {
            Ok(None)
        };
        match notice {
            Ok(Some(message)) => said.push(Diagnostic::passed_over(
                parent.command().query_statement_span().clone(),
                message,
            )),
            Ok(None) => {}
            Err(e) => tracing::debug!("what a relationship word passed over: {}", e),
        }
    }
    said
}

/// Notice 1: the relation rows an ordinary-reference edge passed over.
async fn passed_over_relations(
    child: &Rc<Statement>,
    parent: &Rc<Statement>,
    test: RefTest,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Option<String>> {
    // The end that still has rows is where to read from; the other end is
    // the one whose answer is being explained.  With rows at both ends (two
    // statements that resolved on their own and emptied each other) the
    // child is asked about: "the parent holds N of them".
    let (from, asked) = match (rows(parent, ctx), rows(child, ctx)) {
        (Some(parent_rows), _) if answered_nothing(child, ctx) => (parent_rows, Asked::Child),
        (_, Some(child_rows)) if answered_nothing(parent, ctx) => (child_rows, Asked::Parent),
        _ => return Ok(None),
    };
    let asked_statement = match asked {
        Asked::Child => child,
        Asked::Parent => parent,
    };
    let Some(own) = predicate(asked_statement, ctx) else {
        return Ok(None);
    };
    // A statement whose own words match nothing anywhere would not have
    // answered under any relationship, and is told so ("matched no
    // symbols").  One row decides it, before reads that enter the reference
    // table by the neighbour: on the kernel `func("typo") refs { "u32" }`
    // spent 160 ms reading what holds and declares `u32`.
    if !cfg.index.has_symbol_matching(&own, &ctx.eph).await? {
        return Ok(None);
    }

    let mut found: Vec<Found> = Vec::new();
    for relation in Relations::ALL.iter() {
        if test.relations().contains(relation) {
            continue;
        }
        let edge = Edge::references(RefTest::relation(relation));
        // The derive's own read (`Selector::derive_from_parent`,
        // `Command::notify_from_selection`), under the relation instead of
        // the edge: a listing passes over folded symbols there, so it does
        // here, and what it would not have listed is not counted.
        let mode = asked_statement.traversal_mode();
        let lists = is_listing(asked_statement) && mode != TraversalMode::Unnest;
        let mut parts = vec![own.clone()];
        let (instances, cut): (Vec<SymbolInstanceId>, bool) = match asked {
            Asked::Child => {
                if lists {
                    parts.push(CompositeFilter::leaf(DirectOnlyMixin::new(mode)));
                    parts.push(CompositeFilter::leaf(OuterParentFilterMixin::new(&from)));
                }
                cfg.index
                    .find_child_instance_ids(
                        &from,
                        edge,
                        &CompositeFilter::and(parts),
                        None,
                        &ctx.eph,
                    )
                    .await?
            }
            Asked::Parent => {
                if lists {
                    parts.push(CompositeFilter::leaf(InnermostOnlyMixin::new(mode)));
                }
                cfg.index
                    .find_parent_instance_ids(
                        &from,
                        edge,
                        &CompositeFilter::and(parts),
                        None,
                        &ctx.eph,
                    )
                    .await?
            }
        };
        if instances.is_empty() {
            continue;
        }
        // The read applies what of the predicate its channel carries (types,
        // exact names); the names are those the whole predicate matches.
        let matching = CompositeFilter::and(vec![
            own.clone(),
            CompositeFilter::leaf(SymbolInstanceIdMixin::new(&instances)),
        ]);
        let names = cfg.index.symbol_names_matching(&matching, &ctx.eph).await?;
        if !names.is_empty() {
            found.push(Found {
                relation,
                names,
                at_least: cut,
            });
        }
    }
    if found.is_empty() {
        return Ok(None);
    }

    let parent_words = parent.command().own_words();
    let child_words = child.command().own_words();
    let clauses: Vec<String> = found
        .iter()
        .map(|f| clause(f, asked, parent_words.as_deref(), child_words.as_deref()))
        .collect();
    let one = found.len() == 1 && found[0].names.len() == 1 && !found[0].at_least;
    let rewrites: Vec<String> = found
        .iter()
        .map(|f| format!("`{} {{ … }}`", words(f.relation).0))
        .collect();
    Ok(Some(format!(
        "`refs` lists only ordinary references (`→`); {}. {} or `{{ … }}` {} {}",
        clauses.join("; "),
        rewrites.join(", "),
        if one { "lists" } else { "list" },
        if one { "it" } else { "them" },
    )))
}

/// "`field("f")` holds 212 symbols that `func` matches (`⇢`): `a`, `b`, `c`
/// and 209 more", or, asked of the parent, "3 symbols hold `"my_read"`".
fn clause(found: &Found, asked: Asked, parent: Option<&str>, child: Option<&str>) -> String {
    const SHOWN: usize = 3;
    let (_, mark, verbs, verb) = words(found.relation);
    let count = found.names.len();
    let mut listed = found
        .names
        .iter()
        .take(SHOWN)
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if count > SHOWN {
        if found.at_least {
            listed.push_str(" and more");
        } else {
            listed.push_str(&format!(" and {} more", count - SHOWN));
        }
    }
    let how_many = format!(
        "{}{} symbol{}",
        if found.at_least { "at least " } else { "" },
        count,
        if count == 1 && !found.at_least {
            ""
        } else {
            "s"
        },
    );
    let quoted = |text: &str| format!("`{}`", Diagnostic::quote(text));
    let matching = |text: Option<&str>| match text {
        Some(text) => format!(" that {} matches", quoted(text)),
        None => String::new(),
    };
    match asked {
        Asked::Child => format!(
            "{} {verbs} {how_many}{} (`{mark}`): {listed}",
            parent.map(quoted).unwrap_or_else(|| "it".to_string()),
            matching(child),
        ),
        Asked::Parent => format!(
            "{how_many}{} {} {} (`{mark}`): {listed}",
            matching(parent),
            if count == 1 && !found.at_least {
                verbs
            } else {
                verb
            },
            child.map(quoted).unwrap_or_else(|| "it".to_string()),
        ),
    }
}

/// Notice 2: a relation word asked of rows none of which can be its source.
async fn not_a_source(
    parent: &Rc<Statement>,
    test: RefTest,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Option<String>> {
    // Said of a source somebody named.  A parent that was itself derived
    // through this edge has no rows of its own to be the wrong kind.
    let (Some(parent_rows), Some(parent_words)) = (rows(parent, ctx), parent.command().own_words())
    else {
        return Ok(None);
    };
    let mut reasons = Vec::new();
    let mut relation_words = Vec::new();
    for relation in test.relations().iter() {
        let (source_type, only) = source_of(relation);
        let of_the_kind = CompositeFilter::and(vec![
            CompositeFilter::leaf(SymbolInstanceIdMixin::new(
                &parent_rows
                    .iter()
                    .map(|id| SymbolInstanceId::new(*id))
                    .collect::<Vec<_>>(),
            )),
            CompositeFilter::leaf(SymbolTypeMixin::new(source_type)),
        ]);
        if cfg
            .index
            .has_symbol_matching(&of_the_kind, &ctx.eph)
            .await?
        {
            // Some row could be a source: the emptiness is a fact about the
            // code, not about the word.
            return Ok(None);
        }
        reasons.push(only);
        relation_words.push(format!("`{}`", words(relation).0));
    }
    if reasons.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{} related nothing: {}, and `{}` selects none",
        relation_words.join(" or "),
        reasons.join("; "),
        Diagnostic::quote(&parent_words),
    )))
}
