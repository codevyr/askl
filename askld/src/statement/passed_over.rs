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
//! A ROLE WORD passes over something else, and says so whether or not the
//! answer is empty: AN UNSTATED ROLE IS UNKNOWN, NEVER "NO".
//!
//!  3. `calls` passed over references that state no role.  One mechanism,
//!     two things to say, per project the edge touched:
//!     - the index of that project states no roles at all (an observation
//!       stored with the project, `Index::what_projects_state`): "the
//!       `linux` index states no roles; `calls` cannot match there".  One
//!       such reference on the edge is enough to say it;
//!     - it does, and some references on this edge still state none
//!       (`#define` bodies, macro uses): "`calls` passed over 37 symbols
//!       …".  Counted are the symbols that `refs`, written in that one
//!       place, would list and `calls` does not.  They are read from an end
//!       of the edge whose rows the word had no part in choosing
//!       (`rows_not_read_through`), under [`RefTest::UNSTATED`]; less what
//!       the edge itself finds from the same rows; and kept only if the
//!       statement's other edges keep them (`Index::which_relate`).  Only
//!       function, field and data targets: a reference to a type or a
//!       macro has no role to state.
//!  4. Only when the answer is empty and nothing was undecided: nothing
//!     calls `my_read` by name and a slot holds it -- "nothing calls
//!     `my_read` directly; `ops.read` holds it".
//!  5. Only when the answer is empty: a relation word was asked of an index
//!     that has no rows of that relation -- "the `linux` index has no
//!     `declares` rows".
//!
//! Worked out once per edge, after the statements have run.  The reads are
//! the derive's own (`find_child_instance_ids` / `find_parent_instance_ids`)
//! under the passed-over test, so they are bounded as it is (`read_cap + 1`
//! rows, a budget of references, "at least" when cut) and answered from the
//! SQL cache as it is: a repeated query says what the first one said.
//!
//! For the empty-answer notices the neighbour's rows are taken from where
//! they still are: its final selection, or what its probe resolved before
//! the narrowing emptied it (`ExecutionContext::probe_resolved`).  Nothing
//! is read for them unless an answer came out empty: not cut (that answer is
//! said to be incomplete), and not empty because its own words match nothing
//! anywhere (that one is told so).
//!
//! The role notice is the one that reads on a query whose answer is fine.
//! It costs one small read of what the projects state (cached), the rows of
//! one end by id, and one bounded read; a second and a third only when the
//! first found something to count.

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use index::db_diesel::{
    AskSide, CompositeFilter, DefaultSymbolTypeMixin, DirectOnlyMixin, EphContext,
    InnermostOnlyMixin, OuterParentFilterMixin, ProjectStates, SymbolInstanceIdMixin,
    SymbolTypeMixin, TraversalMode, SYMBOL_TYPE_DATA, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FUNCTION,
    SYMBOL_TYPE_MACRO,
};
use index::edge::{Edge, RefTest, Relations};
use index::roles::{Relation, Roles};
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
    let mut said: Vec<Diagnostic> = Vec::new();
    // Once: an index that states no roles is one fact, however many scopes
    // carry the word that ran into it.
    let mut messages: HashSet<String> = HashSet::new();
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
        let empty = answered_nothing(child, ctx) || answered_nothing(&parent, ctx);
        let role_word = !test.any_ordinary() && test.roles().bits() != 0;
        let relation_word =
            !test.any_ordinary() && test.roles().bits() == 0 && !test.relations().is_empty();

        let mut notices: Vec<anyhow::Result<Option<String>>> = Vec::new();
        if role_word {
            // By default, empty or not: what the word could not decide.
            let undecided = unstated_roles(child, &parent, test, ctx, cfg).await;
            // Every reference here states what it does: then an empty answer
            // is a fact, and the one thing left to say is where the target
            // went.  (Undecided, "nothing calls it" is not known.)
            let decided = matches!(&undecided, Ok(none) if none.is_empty());
            match undecided {
                Ok(undecided) => notices.extend(undecided.into_iter().map(|m| Ok(Some(m)))),
                Err(e) => notices.push(Err(e)),
            }
            if decided {
                notices.push(held_by_a_slot(child, &parent, test, ctx, cfg).await);
            }
        }
        if empty && test.any_ordinary() {
            notices.push(passed_over_relations(child, &parent, test, ctx, cfg).await);
        }
        if empty && relation_word {
            let wrong_kind = not_a_source(&parent, test, ctx, cfg).await;
            // The source could be one: then whether the index records the
            // relation at all is the next thing an empty answer may be about.
            let could_be_a_source = matches!(&wrong_kind, Ok(None));
            notices.push(wrong_kind);
            if could_be_a_source {
                notices.push(no_such_rows(child, &parent, test, ctx, cfg).await);
            }
        }
        for notice in notices {
            match notice {
                Ok(Some(message)) => {
                    if messages.insert(message.clone()) {
                        said.push(Diagnostic::passed_over(
                            parent.command().query_statement_span().clone(),
                            message,
                        ));
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!("what a relationship word passed over: {}", e),
            }
        }
    }
    said
}

/// The derive's own read (`Selector::derive_from_parent`,
/// `Command::notify_from_selection`), under `edge` instead of the edge the
/// query wrote: the rows of the asked statement that `from` relates to.  A
/// listing passes over folded symbols there, so it does here, and what it
/// would not have listed is not counted.
async fn related(
    asked: Asked,
    asked_statement: &Rc<Statement>,
    from: &[i64],
    edge: Edge,
    mut parts: Vec<CompositeFilter>,
    eph: &EphContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<(Vec<SymbolInstanceId>, bool)> {
    let mode = asked_statement.traversal_mode();
    let lists = is_listing(asked_statement) && mode != TraversalMode::Unnest;
    match asked {
        Asked::Child => {
            if lists {
                parts.push(CompositeFilter::leaf(DirectOnlyMixin::new(mode)));
                parts.push(CompositeFilter::leaf(OuterParentFilterMixin::new(from)));
            }
            cfg.index
                .find_child_instance_ids(from, edge, &CompositeFilter::and(parts), None, eph)
                .await
        }
        Asked::Parent => {
            if lists {
                parts.push(CompositeFilter::leaf(InnermostOnlyMixin::new(mode)));
            }
            cfg.index
                .find_parent_instance_ids(from, edge, &CompositeFilter::and(parts), None, eph)
                .await
        }
    }
}

fn ids_leaf(ids: &[i64]) -> CompositeFilter {
    CompositeFilter::leaf(SymbolInstanceIdMixin::new(
        &ids.iter()
            .map(|id| SymbolInstanceId::new(*id))
            .collect::<Vec<_>>(),
    ))
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
        let (instances, cut) = related(
            asked,
            asked_statement,
            &from,
            edge,
            vec![own.clone()],
            &ctx.eph,
            cfg,
        )
        .await?;
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

/// How the role words of a test read in a sentence: as written
/// (`calls`, `calls or writes`), and the form after "they" (`call`).
struct RoleWords {
    written: String,
    base: String,
}

fn role_words(test: RefTest, parent: &Rc<Statement>) -> RoleWords {
    let roles = test.roles();
    let mut words: Vec<(&str, &str)> = Vec::new();
    if roles.contains(Roles::CALL) {
        words.push(("calls", "call"));
    }
    if roles.contains(Roles::WRITE) {
        words.push(("writes", "write"));
    }
    if roles.contains(Roles::TAKE) {
        // One bit, two spellings: say the one the query wrote.
        words.push(if wrote_reads(parent) {
            ("reads", "read")
        } else {
            ("takes", "take")
        });
    }
    RoleWords {
        written: words
            .iter()
            .map(|(word, _)| *word)
            .collect::<Vec<_>>()
            .join(" or "),
        base: words
            .iter()
            .map(|(_, base)| *base)
            .collect::<Vec<_>>()
            .join(" or "),
    }
}

/// The query spelled the TAKE role `reads`: the nearest statement at or above
/// `parent` that wrote either spelling wrote that one last.  (The word is
/// consumed when it is parsed; the edge keeps the bit, not the spelling.)
fn wrote_reads(parent: &Rc<Statement>) -> bool {
    let mut at = Some(parent.clone());
    while let Some(statement) = at {
        let text = statement
            .command()
            .query_statement_span()
            .as_pest_span()
            .as_str();
        let mut last: Option<bool> = None;
        let mut word = String::new();
        let mut quoted = false;
        let mut escaped = false;
        for c in text.chars().chain(std::iter::once(' ')) {
            if quoted {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => quoted = false,
                    _ => {}
                }
                continue;
            }
            if c.is_alphanumeric() || c == '_' {
                word.push(c);
                continue;
            }
            match word.as_str() {
                "reads" => last = Some(true),
                "takes" => last = Some(false),
                _ => {}
            }
            word.clear();
            quoted = c == '"';
        }
        if let Some(reads) = last {
            return reads;
        }
        at = statement.parent().and_then(|p| p.upgrade());
    }
    false
}

/// The rows a statement has whatever the edge to `across` asks: what its
/// probe resolved without being bound by that neighbour
/// (`ExecutionContext::probe_through`).
///
/// Where to read from when asking what the word on that edge passed over.
/// What the statement ended as will not do, and neither will rows a probe
/// read through that edge: both already lack the rows whose only references
/// the word passed over, and those are the rows being asked about.
fn rows_not_read_through(
    statement: &Rc<Statement>,
    across: &Rc<Statement>,
    ctx: &ExecutionContext,
) -> Option<Vec<i64>> {
    let key = statement_key(statement);
    let bound_by = ctx.probe_through.get(&key)?;
    if bound_by.contains(&statement_key(across)) {
        return None;
    }
    ctx.probe_resolved
        .get(&key)
        .filter(|ids| !ids.is_empty())
        .cloned()
}

/// The rows of a neighbour that a candidate can be tested against: what its
/// probe resolved without being bound by the statement asked about, or what
/// it ended as when that is all of what it means.
fn settled_rows(
    statement: &Rc<Statement>,
    asked_statement: &Rc<Statement>,
    ctx: &ExecutionContext,
) -> Option<Vec<i64>> {
    rows_not_read_through(statement, asked_statement, ctx).or_else(|| {
        statement
            .get_selection(ctx)
            .filter(|selection| !selection.is_empty() && !selection.cut())
            .map(|selection| selection.get_instance_ids())
    })
}

/// A reference to one of these can state a role.  A reference to a type or
/// to a macro cannot: nothing is omitted when it states none.
const CAN_STATE_A_ROLE: [i32; 3] = [SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_DATA];

/// Notice 3: what a role word could not decide, per project the edge touched.
async fn unstated_roles(
    child: &Rc<Statement>,
    parent: &Rc<Statement>,
    test: RefTest,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Vec<String>> {
    // Read from an end whose rows do not depend on the word, and ask about
    // the other: what the parent's rows name (the targets passed over), or
    // what names the child's rows (the sources).  One sentence per edge.
    // An end that wrote no words of its own is the one the word fills in, so
    // it is asked about first (`calls { "hook" }` lists callers); between two
    // that wrote some, the targets first.  The other end is asked about only
    // when nothing was passed over at the first (`func calls { "hook" }`:
    // `hook` was found, and a function that only names it was not).
    let targets = (rows_not_read_through(parent, child, ctx), Asked::Child);
    let sources = (rows_not_read_through(child, parent, ctx), Asked::Parent);
    let directions = if parent.command().own_words().is_none() {
        [sources, targets]
    } else {
        [targets, sources]
    };
    for (from, asked) in directions {
        let Some(from) = from else {
            continue;
        };
        let said = undecided_from(child, parent, test, from, asked, ctx, cfg).await?;
        if !said.is_empty() {
            return Ok(said);
        }
    }
    Ok(vec![])
}

/// [`unstated_roles`] read from one end: `from` are that end's rows, `asked`
/// the other end.
async fn undecided_from(
    child: &Rc<Statement>,
    parent: &Rc<Statement>,
    test: RefTest,
    from: Vec<i64>,
    asked: Asked,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Vec<String>> {
    let asked_statement = match asked {
        Asked::Child => child,
        Asked::Parent => parent,
    };
    let Some(own) = predicate(asked_statement, ctx) else {
        return Ok(vec![]);
    };
    // See `passed_over_relations`: a statement whose own words match nothing
    // anywhere would not have answered under any word.
    if !cfg.index.has_symbol_matching(&own, &ctx.eph).await? {
        return Ok(vec![]);
    }

    // The rows to read from, by project: what an index states is a fact
    // about one upload.
    let states = cfg.index.what_projects_state().await?;
    let mut by_project: BTreeMap<String, (ProjectStates, Vec<(i64, i32)>)> = BTreeMap::new();
    for row in cfg
        .index
        .instances_matching(&ids_leaf(&from), &ctx.eph)
        .await?
    {
        let Some(state) = states.iter().find(|s| s.project == row.project) else {
            continue;
        };
        by_project
            .entry(state.name.clone())
            .or_insert_with(|| (state.clone(), Vec::new()))
            .1
            .push((row.instance, row.symbol_type));
    }

    let words = role_words(test, parent);
    let unstated = Edge::references(RefTest::UNSTATED);
    let mut said = Vec::new();
    for (project, (state, rows)) in by_project {
        if !state.states_roles {
            // Nothing in this index states a role, so the word cannot match
            // there whatever the code does.  One row is enough to know the
            // word ran into it.
            let from: Vec<i64> = rows.iter().map(|(instance, _)| *instance).collect();
            let mut one = ctx.eph.clone();
            one.set_read_cap(1);
            let (instances, ran_out) = related(
                asked,
                asked_statement,
                &from,
                unstated,
                vec![own.clone()],
                &one,
                cfg,
            )
            .await?;
            // A read that ran past its budget of references returns none of
            // them and says so: there were more than enough.
            if !instances.is_empty() || ran_out {
                said.push(format!(
                    "the `{project}` index states no roles; `{}` cannot match there. \
                     `refs {{ … }}` lists its references",
                    words.written
                ));
            }
            continue;
        }

        // The target of a reference is the child's row: only a target that
        // can be called, written or taken is counted.
        let mut parts = vec![own.clone()];
        let from: Vec<i64> = match asked {
            Asked::Child => {
                parts.push(CompositeFilter::leaf(DefaultSymbolTypeMixin::new(
                    CAN_STATE_A_ROLE.to_vec(),
                )));
                rows.iter().map(|(instance, _)| *instance).collect()
            }
            Asked::Parent => rows
                .iter()
                .filter(|(_, symbol_type)| CAN_STATE_A_ROLE.contains(symbol_type))
                .map(|(instance, _)| *instance)
                .collect(),
        };
        if from.is_empty() {
            continue;
        }
        let (instances, cut) = related(
            asked,
            asked_statement,
            &from,
            unstated,
            parts.clone(),
            &ctx.eph,
            cfg,
        )
        .await?;
        if instances.is_empty() {
            if cut {
                // More unstated references than the read's budget: none came
                // back, so there is nothing to count or to name -- and
                // saying nothing would pass them over in silence.
                said.push(uncounted(
                    &words,
                    asked,
                    parent.command().own_words().as_deref(),
                    child.command().own_words().as_deref(),
                ));
            }
            continue;
        }
        // Less the rows the scope's own edge finds from the same end: a
        // symbol that another reference states the role to (or that the
        // edge reaches some other way it asks for) was found, not passed
        // over.  (Not "less what the answer shows": a statement somebody
        // named shows its rows whether or not the word found them.)
        let (found, found_cut) = related(
            asked,
            asked_statement,
            &from,
            child.get_relationship_type(),
            parts.clone(),
            &ctx.eph,
            cfg,
        )
        .await?;
        if found_cut {
            // Which rows the word found is not all known, so which it passed
            // over is not either: no count would be true.  (The statement
            // asked about is then cut as well, and says it is incomplete.)
            continue;
        }
        let as_ids = |instances: &[SymbolInstanceId]| -> Vec<i64> {
            instances.iter().map(|id| Into::<i64>::into(*id)).collect()
        };
        let found: HashSet<i64> = cfg
            .index
            .instances_matching(&ids_leaf(&as_ids(&found)), &ctx.eph)
            .await?
            .into_iter()
            .map(|row| row.symbol)
            .collect();
        // The read applies what of the predicate its channel carries; the
        // rows are those the whole predicate matches.
        let matching = CompositeFilter::and(vec![own.clone(), ids_leaf(&as_ids(&instances))]);
        let candidates: Vec<_> = cfg
            .index
            .instances_matching(&matching, &ctx.eph)
            .await?
            .into_iter()
            .filter(|row| !found.contains(&row.symbol))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        // "Passed over" means `refs` in this one place would have listed
        // them: the statement's other edges must keep them too.
        let candidate_rows: Vec<i64> = candidates.iter().map(|row| row.instance).collect();
        let Some(kept) =
            kept_by_other_edges(asked_statement, child, asked, candidate_rows, ctx, cfg).await?
        else {
            continue;
        };
        let kept: HashSet<i64> = kept.into_iter().collect();
        let names: std::collections::BTreeSet<String> = candidates
            .into_iter()
            .filter(|row| kept.contains(&row.instance))
            .map(|row| row.name)
            .collect();
        if names.is_empty() {
            continue;
        }
        said.push(undecided(
            &words,
            asked,
            &names.into_iter().collect::<Vec<_>>(),
            cut,
            parent.command().own_words().as_deref(),
            child.command().own_words().as_deref(),
        ));
    }
    Ok(said)
}

/// Which of `candidates` -- rows the asked statement would gain with `refs`
/// on the edge to `child` -- its OTHER edges would keep.  `None` when a
/// neighbour has no settled rows to test against, or the test could not be
/// read to its end: then nothing can be said.
async fn kept_by_other_edges(
    asked_statement: &Rc<Statement>,
    child: &Rc<Statement>,
    asked: Asked,
    mut candidates: Vec<i64>,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Option<Vec<i64>>> {
    // (neighbour, the edge between them, the side the CANDIDATES are on)
    let mut others: Vec<(Rc<Statement>, Edge, AskSide)> = Vec::new();
    if asked == Asked::Parent {
        if let Some(above) = asked_statement.parent().and_then(|p| p.upgrade()) {
            others.push((
                above,
                asked_statement.get_relationship_type(),
                AskSide::Children,
            ));
        }
    }
    for below in asked_statement.children() {
        if asked == Asked::Parent && statement_key(&below) == statement_key(child) {
            continue; // the edge being asked about
        }
        let edge = below.get_relationship_type();
        others.push((below, edge, AskSide::Parents));
    }
    for (neighbour, edge, side) in others {
        if candidates.is_empty() {
            break;
        }
        let Some(theirs) = settled_rows(&neighbour, asked_statement, ctx) else {
            return Ok(None);
        };
        let related = match side {
            AskSide::Parents => {
                cfg.index
                    .which_relate(&candidates, &theirs, AskSide::Parents, edge, &ctx.eph)
                    .await?
            }
            AskSide::Children => {
                cfg.index
                    .which_relate(&theirs, &candidates, AskSide::Children, edge, &ctx.eph)
                    .await?
            }
        };
        if !related.whole {
            return Ok(None);
        }
        candidates = related.ids;
    }
    Ok(Some(candidates))
}

/// The same, when the references were too many to read: "`calls` passed over
/// more references than could be read here, which `mod("x")` makes without
/// stating a role …".
fn uncounted(words: &RoleWords, asked: Asked, parent: Option<&str>, child: Option<&str>) -> String {
    let quoted = |text: &str| format!("`{}`", Diagnostic::quote(text));
    let written = &words.written;
    let why = "without stating a role (`#define` bodies, macro uses)";
    let whose = match asked {
        Asked::Child => format!(
            "which {} makes",
            parent.map(quoted).unwrap_or_else(|| "it".to_string())
        ),
        Asked::Parent => format!(
            "made to {}",
            child.map(quoted).unwrap_or_else(|| "it".to_string())
        ),
    };
    format!(
        "`{written}` passed over more references than could be read here, {whose} {why}. \
         Whether they are what `{written}` asks for is not known; `refs {{ … }}` lists them"
    )
}

/// "`calls` passed over 2 symbols that `"caller"` names without stating a
/// role (`#define` bodies, macro uses): `maybe`, `plain`. Whether it calls
/// them is not known; `refs { … }` lists them".
fn undecided(
    words: &RoleWords,
    asked: Asked,
    names: &[String],
    at_least: bool,
    parent: Option<&str>,
    child: Option<&str>,
) -> String {
    const SHOWN: usize = 3;
    let count = names.len();
    let one = count == 1 && !at_least;
    let mut listed = names
        .iter()
        .take(SHOWN)
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if count > SHOWN {
        if at_least {
            listed.push_str(" and more");
        } else {
            listed.push_str(&format!(" and {} more", count - SHOWN));
        }
    }
    let how_many = format!(
        "{}{} symbol{}",
        if at_least { "at least " } else { "" },
        count,
        if one { "" } else { "s" },
    );
    let quoted = |text: &str| format!("`{}`", Diagnostic::quote(text));
    let matching = |text: Option<&str>| match text {
        Some(text) => format!(" matching {}", quoted(text)),
        None => String::new(),
    };
    let why = "without stating a role (`#define` bodies, macro uses)";
    let (written, base) = (&words.written, &words.base);
    let (them, lists) = if one {
        ("it", "lists it")
    } else {
        ("them", "lists them")
    };
    match asked {
        Asked::Child => {
            let source = parent.map(quoted).unwrap_or_else(|| "it".to_string());
            format!(
                "`{written}` passed over {how_many}{} that {source} names {why}: {listed}. \
                 Whether it {written} {them} is not known; `refs {{ … }}` {lists}",
                matching(child),
            )
        }
        Asked::Parent => {
            let target = child.map(quoted).unwrap_or_else(|| "it".to_string());
            format!(
                "`{written}` passed over {how_many}{} that {} {target} {why}: {listed}. \
                 Whether {} {target} is not known; `refs {{ … }}` {lists}",
                matching(parent),
                if one { "names" } else { "name" },
                if one {
                    format!("it {written}")
                } else {
                    format!("they {base}")
                },
            )
        }
    }
}

/// Notice 4: a role word found nothing, every reference to the target states
/// what it does, and a slot holds the target.
///
/// `calls` is literal: a call through `ops->read(...)` is a call of the slot.
/// So "who calls `my_read`" is empty for every function that is only ever
/// stored in a table, which is most of a kernel.  The answer is right and the
/// question was one hop short; it says where the hop is.
async fn held_by_a_slot(
    child: &Rc<Statement>,
    parent: &Rc<Statement>,
    test: RefTest,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Option<String>> {
    // Upward only: the parent answered nothing and the child was named.
    if !answered_nothing(parent, ctx) {
        return Ok(None);
    }
    let (Some(targets), Some(target_words)) = (rows(child, ctx), child.command().own_words())
    else {
        return Ok(None);
    };
    if let Some(own) = predicate(parent, ctx) {
        if !cfg.index.has_symbol_matching(&own, &ctx.eph).await? {
            return Ok(None);
        }
    }
    // What holds the target: relation rows belong to their source only, so
    // this reads the slots and nothing around them.
    let (slots, cut) = cfg
        .index
        .find_parent_instance_ids(
            &targets,
            Edge::references(RefTest::relation(Relation::Holds)),
            &CompositeFilter::and(vec![]),
            None,
            &ctx.eph,
        )
        .await?;
    if slots.is_empty() {
        return Ok(None);
    }
    let slots: Vec<i64> = slots.iter().map(|id| Into::<i64>::into(*id)).collect();
    let names = cfg
        .index
        .symbol_names_matching(&ids_leaf(&slots), &ctx.eph)
        .await?;
    if names.is_empty() {
        return Ok(None);
    }

    const SHOWN: usize = 3;
    let words = role_words(test, parent);
    let one = names.len() == 1 && !cut;
    let mut listed = names
        .iter()
        .take(SHOWN)
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        if cut {
            listed.push_str(" and more");
        } else {
            listed.push_str(&format!(" and {} more", names.len() - SHOWN));
        }
    }
    let target = Diagnostic::quote(&target_words);
    let source = parent.command().own_words();
    let source = source.as_deref().map(Diagnostic::quote);
    Ok(Some(format!(
        "nothing{} {} `{target}` directly; {listed} {} it (`⇢`): \
         `{}{} {{ field holds {{ {target} }} }}` goes through the slot",
        source
            .as_deref()
            .map(|s| format!(" matching `{s}`"))
            .unwrap_or_default(),
        words.written,
        if one { "holds" } else { "hold" },
        source
            .as_deref()
            .map(|s| format!("{s} "))
            .unwrap_or_default(),
        words.written,
    )))
}

/// What only a source of `relation` can be, for "records what a … …".
fn recorded_by(relation: Relation) -> &'static str {
    match relation {
        Relation::Declares => "what a macro declares",
        Relation::Holds => "what a slot holds",
    }
}

/// Notice 5: a relation word found nothing in an index that has no rows of
/// that relation.
///
/// Said of the projects the edge's rows are in, and only when the answer is
/// empty.  Whether such rows "would have been in the answer" cannot be asked
/// of rows that do not exist; what can be said is that this index was never
/// going to answer.
async fn no_such_rows(
    child: &Rc<Statement>,
    parent: &Rc<Statement>,
    test: RefTest,
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) -> anyhow::Result<Option<String>> {
    // An end that answered nothing because its own words match nothing
    // anywhere is told that, and the index is not why.
    for statement in [child, parent] {
        if !answered_nothing(statement, ctx) || is_listing(statement) {
            continue;
        }
        if let Some(own) = predicate(statement, ctx) {
            if !cfg.index.has_symbol_matching(&own, &ctx.eph).await? {
                return Ok(None);
            }
        }
    }
    let Some(from) = rows(parent, ctx).or_else(|| rows(child, ctx)) else {
        return Ok(None);
    };
    let states = cfg.index.what_projects_state().await?;
    let projects: std::collections::BTreeSet<i32> = cfg
        .index
        .instances_matching(&ids_leaf(&from), &ctx.eph)
        .await?
        .into_iter()
        .map(|row| row.project)
        .collect();
    let mut lacking: BTreeMap<String, Vec<Relation>> = BTreeMap::new();
    for state in states.iter().filter(|s| projects.contains(&s.project)) {
        for relation in test.relations().iter() {
            if !state.has(relation) {
                lacking
                    .entry(state.name.clone())
                    .or_default()
                    .push(relation);
            }
        }
    }
    if lacking.is_empty() {
        return Ok(None);
    }
    let sentences: Vec<String> = lacking
        .into_iter()
        .map(|(project, relations)| {
            format!(
                "the `{project}` index has no {} rows: nothing in it records {}",
                relations
                    .iter()
                    .map(|relation| format!("`{}`", words(*relation).0))
                    .collect::<Vec<_>>()
                    .join(" or "),
                relations
                    .iter()
                    .map(|relation| recorded_by(*relation))
                    .collect::<Vec<_>>()
                    .join(" or "),
            )
        })
        .collect();
    Ok(Some(sentences.join("; ")))
}
