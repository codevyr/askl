//! Boolean expressions over predicate verbs — the compound filter of the
//! filter-expressions design (docs: design/filter-expressions).
//!
//! A `compound_filter` with operators builds a [`PredicateExpr`] tree via a
//! Pratt parser (precedence `not` > `and` > `or`; parens group) and wraps it
//! in ONE [`CompoundVerb`].  Classification of what may appear inside is the
//! builder's job, not the grammar's:
//!
//! - only predicate-classed verbs are admitted; a relationship, binding, or
//!   env verb in expression position is a type error with a span;
//! - each atom must pass [`Verb::compound_admission`] — it denotes one
//!   concrete predicate, so the compiled tree never meets the IR's "no
//!   constraint" case under `Not`/`Or` (the IR has no `false`);
//! - per `or` node, operands are either all anchor-capable (the group is one
//!   branch with one no-match probe) or all pure filters — mixed is an
//!   error;
//! - an anchor-free compound must constrain at most one dimension: filters
//!   of different dimensions conjoin by juxtaposition, not by operators.
//!
//! The suppression rule: atoms contribute ONLY their predicate.  Their
//! parse-context side effects (`update_context` — relationship switching,
//! default-type propagation) never run, so `file or dir { … }` leaves the
//! scope on its default relationship.  Layer blocks are the one construct
//! whose verbs mutate shared state during CONSTRUCTION rather than in
//! `update_context`, so expressions are refused inside them outright.

use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::span::Span;
use index::db_diesel::{CompositeFilter, EphContext};
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use pest::iterators::{Pair, Pairs};
use pest::pratt_parser::{Assoc, Op, PrattParser};
use std::fmt::Display;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use super::{
    construct_verb, AnchorKind, DeriveMethod, Dimension, Filter, OwnPredicate, Selector, Verb,
    VerbClass,
};
use async_trait::async_trait;

#[derive(Debug)]
pub(crate) enum PredicateExpr {
    Atom(Arc<dyn Verb>),
    And(Vec<PredicateExpr>),
    Or(Vec<PredicateExpr>),
    Not(Box<PredicateExpr>),
}

impl PredicateExpr {
    /// Compile onto the existing IR: operators map one-to-one onto the
    /// `CompositeFilter` constructors; an atom compiles through its filter
    /// role, or — for pure anchors like a name selector — through its own
    /// predicate.  Totality is the admission gate's guarantee
    /// ([`Verb::compound_admission`]).
    fn compile(&self, eph: &EphContext) -> CompositeFilter {
        match self {
            PredicateExpr::Atom(v) => {
                if let Some(f) = v.as_filter() {
                    f.get_composite_filter(eph).unwrap_or_else(|| {
                        unreachable!(
                            "atom `{}` passed the concreteness gate yet compiled to no constraint",
                            v.name()
                        )
                    })
                } else if let Some(s) = v.as_selector() {
                    match s.own_predicate(eph) {
                        OwnPredicate::Filter(Some(f)) => f,
                        _ => unreachable!(
                            "atom `{}` passed the concreteness gate yet has no own predicate",
                            v.name()
                        ),
                    }
                } else {
                    unreachable!("predicate atom `{}` has neither role", v.name())
                }
            }
            PredicateExpr::And(parts) => {
                CompositeFilter::and(parts.iter().map(|p| p.compile(eph)).collect())
            }
            PredicateExpr::Or(parts) => {
                CompositeFilter::or(parts.iter().map(|p| p.compile(eph)).collect())
            }
            PredicateExpr::Not(inner) => CompositeFilter::not(inner.compile(eph)),
        }
    }

    /// The first positive anchor's kind, if any: an expression anchors a
    /// statement iff this is `Some` (a negated anchor is an exclusion,
    /// never an entry point).
    fn first_positive_anchor_kind(&self) -> Option<AnchorKind> {
        match self {
            PredicateExpr::Atom(v) => v.anchor_kind(),
            PredicateExpr::And(parts) | PredicateExpr::Or(parts) => {
                parts.iter().find_map(|p| p.first_positive_anchor_kind())
            }
            PredicateExpr::Not(_) => None,
        }
    }

    /// Visit every atom with its positivity (false once under any `not`).
    fn for_each_atom<'a>(&'a self, positive: bool, f: &mut impl FnMut(&'a Arc<dyn Verb>, bool)) {
        match self {
            PredicateExpr::Atom(v) => f(v, positive),
            PredicateExpr::And(parts) | PredicateExpr::Or(parts) => {
                for p in parts {
                    p.for_each_atom(positive, f);
                }
            }
            PredicateExpr::Not(inner) => inner.for_each_atom(false, f),
        }
    }

    /// The dimensions positively constrained — the compound's slot-eviction
    /// key.  The single source of truth: both validation and
    /// [`CompoundVerb`]'s `dimensions()` use this.
    fn positive_dimensions(&self) -> Vec<Dimension> {
        let mut dims: Vec<Dimension> = Vec::new();
        self.for_each_atom(true, &mut |v, positive| {
            if positive {
                for d in v.dimensions() {
                    if !dims.contains(&d) {
                        dims.push(d);
                    }
                }
            }
        });
        dims
    }

    /// Whether a positive atom constrains by name (weakness accounting).
    fn has_positive_name(&self) -> bool {
        let mut has = false;
        self.for_each_atom(true, &mut |v, positive| {
            if positive && (v.has_name_constraint() || v.anchor_kind() == Some(AnchorKind::Name)) {
                has = true;
            }
        });
        has
    }

    /// Whether the compound, used as a filter, inherits into child scopes.
    /// A pure exclusion (no positive atom) accumulates AND inherits
    /// unconditionally, like `ignore`; a slot-holding compound inherits iff
    /// all its atoms would.
    fn inherits_as_filter(&self) -> bool {
        let mut any_positive = false;
        let mut all_clone = true;
        self.for_each_atom(true, &mut |v, positive| {
            // Only POSITIVE atoms gate inheritance.  A negated atom is an
            // exclusion, and exclusions inherit unconditionally (like
            // `ignore`) regardless of what the atom would do on its own —
            // a name selector's own `Skip` says "anchors don't inherit",
            // which has nothing to say about excluding that name.
            if positive {
                any_positive = true;
                if !matches!(v.derive_method(), DeriveMethod::Clone) {
                    all_clone = false;
                }
            }
        });
        !any_positive || all_clone
    }

    /// True when some `or` node mixes anchor-capable operands with pure
    /// filters — a branch and a constraint have different jobs, so the mix
    /// is rejected.
    fn or_mixes_anchors(&self) -> bool {
        match self {
            PredicateExpr::Or(parts) => {
                let anchors = parts
                    .iter()
                    .filter(|p| p.first_positive_anchor_kind().is_some())
                    .count();
                (anchors != 0 && anchors != parts.len())
                    || parts.iter().any(|p| p.or_mixes_anchors())
            }
            PredicateExpr::And(parts) => parts.iter().any(|p| p.or_mixes_anchors()),
            PredicateExpr::Not(inner) => inner.or_mixes_anchors(),
            PredicateExpr::Atom(_) => false,
        }
    }

    /// True when a `not` directly wraps another `not` (either spelling:
    /// `not not x` or `not (not x)`).  The positivity classification treats
    /// everything under a `not` as negative, so double negation would be
    /// silently misclassified as an exclusion; it is rejected instead.
    fn contains_double_negation(&self) -> bool {
        match self {
            PredicateExpr::Not(inner) => {
                matches!(**inner, PredicateExpr::Not(_)) || inner.contains_double_negation()
            }
            PredicateExpr::And(parts) | PredicateExpr::Or(parts) => {
                parts.iter().any(|p| p.contains_double_negation())
            }
            PredicateExpr::Atom(_) => false,
        }
    }
}

impl Display for PredicateExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PredicateExpr::Atom(v) => write!(f, "{}", v.name()),
            PredicateExpr::And(parts) => {
                write!(f, "(")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, " and ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ")")
            }
            PredicateExpr::Or(parts) => {
                write!(f, "(")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, " or ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ")")
            }
            PredicateExpr::Not(inner) => write!(f, "not {}", inner),
        }
    }
}

/// One boolean expression over predicate verbs, acting as a single verb in
/// the command: a pure-filter compound holds its dimension's slot (or
/// accumulates as an exclusion when it has no positive atom); an
/// anchor-capable compound is ONE branch — one fused emission, one no-match
/// probe for the whole disjunction.  All properties are computed from the
/// immutable expression on demand — parse-time trees are tiny.
#[derive(Debug)]
pub(crate) struct CompoundVerb {
    span: Span,
    expr: PredicateExpr,
}

impl CompoundVerb {
    fn new(span: Span, expr: PredicateExpr) -> Self {
        Self { span, expr }
    }
}

impl Verb for CompoundVerb {
    fn name(&self) -> &str {
        "compound"
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    /// Slots are held by CONSTRAINTS.  An anchor-capable compound is a
    /// branch — it produces rows rather than narrowing them — so it occupies
    /// no dimension and a later filter cannot evict it: `func("a") or
    /// method("a") data` keeps its anchor and simply conjoins `data`.
    fn dimensions(&self) -> Vec<Dimension> {
        if self.anchor_kind().is_some() {
            Vec::new()
        } else {
            self.expr.positive_dimensions()
        }
    }

    fn anchor_kind(&self) -> Option<AnchorKind> {
        self.expr.first_positive_anchor_kind()
    }

    fn has_name_constraint(&self) -> bool {
        self.expr.has_positive_name()
    }

    /// Forward the first positive atom's token so an all-anchor `or`-group
    /// keeps the "did you mean?" suggestions that the equivalent
    /// juxtaposition gets — the group's single no-match diagnostic would
    /// otherwise be strictly worse than the spelling it replaces.
    fn matched_token(&self) -> Option<String> {
        let mut token = None;
        self.expr.for_each_atom(true, &mut |v, positive| {
            if positive && token.is_none() {
                token = v.matched_token();
            }
        });
        token
    }

    /// A branch never inherits; a filter compound follows
    /// [`PredicateExpr::inherits_as_filter`].
    fn derive_method(&self) -> DeriveMethod {
        if self.anchor_kind().is_none() && self.expr.inherits_as_filter() {
            DeriveMethod::Clone
        } else {
            DeriveMethod::Skip
        }
    }

    /// A compound occupying the type dimension provides the statement's type
    /// answer, so the parse-time default type filter must not be injected on
    /// top of it.
    /// Independent of `dimensions()`: this answers the parse-time
    /// default-injection question, which a branch mentioning the type
    /// dimension must answer just as a filter does.
    fn suppresses_default_type_filter(&self) -> bool {
        self.expr.positive_dimensions().contains(&Dimension::Type)
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        if self.anchor_kind().is_none() {
            Some(self)
        } else {
            None
        }
    }

    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        if self.anchor_kind().is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl Filter for CompoundVerb {
    fn get_composite_filter(&self, eph: &EphContext) -> Option<CompositeFilter> {
        Some(self.expr.compile(eph))
    }
}

#[async_trait(?Send)]
impl Selector for CompoundVerb {
    fn build_composite_filter(
        &self,
        command: &crate::command::Command,
        eph: &EphContext,
    ) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> = command
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        parts.push(self.expr.compile(eph));
        Some(CompositeFilter::and(parts))
    }

    fn own_predicate(&self, eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Filter(Some(self.expr.compile(eph)))
    }

    /// Guaranteed by construction: every anchor atom admitted into a
    /// compound is fusable, so the branch's whole emission is one
    /// `find_symbol` over the fused predicate.
    fn fusable(&self) -> bool {
        true
    }

    // No `select_from_all_impl`: `fusable()` is unconditionally true and
    // there is no layer spec, so `compute_selection` always routes this
    // verb through the fused statement query.  The trait default would be
    // unreachable duplication of that path.
}

impl Display for CompoundVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compound({})", self.expr)
    }
}

fn pratt() -> &'static PrattParser<Rule> {
    static PRATT: OnceLock<PrattParser<Rule>> = OnceLock::new();
    // Later `.op` calls bind tighter: or < and < not.
    PRATT.get_or_init(|| {
        PrattParser::new()
            .op(Op::infix(Rule::or_kw, Assoc::Left))
            .op(Op::infix(Rule::and_kw, Assoc::Left))
            .op(Op::prefix(Rule::not_kw))
    })
}

fn span_err(message: String, span: pest::Span<'_>) -> Error<Rule> {
    Error::new_from_span(CustomError { message }, span)
}

fn construct_atom(ctx: &Rc<ParserContext>, pair: Pair<Rule>) -> Result<PredicateExpr, Error<Rule>> {
    let pest_span = pair.as_span();
    let verb = construct_verb(ctx, pair)?;
    // One gate, one source of truth: the verb itself says whether it may be
    // an atom and, if not, why (see [`Verb::compound_admission`]).
    verb.compound_admission()
        .map_err(|reason| span_err(reason, pest_span))?;
    Ok(PredicateExpr::Atom(verb))
}

fn build_expr<'i>(
    ctx: &Rc<ParserContext>,
    pairs: Pairs<'i, Rule>,
) -> Result<PredicateExpr, Error<Rule>> {
    pratt()
        .map_primary(|primary| match primary.as_rule() {
            Rule::verb => construct_atom(ctx, primary),
            Rule::group => build_expr(ctx, primary.into_inner()),
            rule => Err(span_err(
                format!("unexpected rule {:?} in filter expression", rule),
                primary.as_span(),
            )),
        })
        .map_prefix(|op, rhs| match op.as_rule() {
            Rule::not_kw => Ok(PredicateExpr::Not(Box::new(rhs?))),
            rule => Err(span_err(
                format!("unexpected prefix {:?}", rule),
                op.as_span(),
            )),
        })
        .map_infix(|lhs, op, rhs| {
            let (lhs, rhs) = (lhs?, rhs?);
            match op.as_rule() {
                Rule::or_kw => Ok(match lhs {
                    PredicateExpr::Or(mut parts) => {
                        parts.push(rhs);
                        PredicateExpr::Or(parts)
                    }
                    other => PredicateExpr::Or(vec![other, rhs]),
                }),
                Rule::and_kw => Ok(match lhs {
                    PredicateExpr::And(mut parts) => {
                        parts.push(rhs);
                        PredicateExpr::And(parts)
                    }
                    other => PredicateExpr::And(vec![other, rhs]),
                }),
                rule => Err(span_err(
                    format!("unexpected operator {:?}", rule),
                    op.as_span(),
                )),
            }
        })
        .parse(pairs)
}

/// The design's structural checks, applied once over the built tree.  All
/// classification lives on [`PredicateExpr`]; this attaches the expression's
/// span to whichever rule failed.
fn validate(expr: &PredicateExpr, span: pest::Span<'_>) -> Result<(), Error<Rule>> {
    if expr.contains_double_negation() {
        return Err(span_err(
            "double negation: `not not` cancels itself — remove both".to_string(),
            span,
        ));
    }

    // Per `or` node: operands are all branches or all filters.
    if expr.or_mixes_anchors() {
        return Err(span_err(
            "`or` mixes anchors (name patterns, named type selectors) with \
             pure filters; make both sides anchors, or both sides filters"
                .to_string(),
            span,
        ));
    }

    // An anchor-free compound is an (inheritable) filter, and the slot
    // cascade needs it to constrain ONE dimension.  Cross-dimension
    // conjunction is juxtaposition; cross-dimension disjunction is sibling
    // statements.
    if expr.first_positive_anchor_kind().is_none() {
        let dims = expr.positive_dimensions();
        if dims.len() > 1 {
            return Err(span_err(
                format!(
                    "cross-dimension filter expression ({:?}): filters of different \
                     dimensions conjoin by juxtaposition (write them side by side); \
                     for a disjunction, split into sibling statements",
                    dims
                ),
                span,
            ));
        }
    }
    Ok(())
}

/// Build one `compound_filter` pair into the current command.  A single
/// bare verb (no operators) takes the classic [`super::build_verb`] path —
/// context side effects included; anything with operators builds a
/// [`CompoundVerb`].
pub(crate) fn build_compound_filter(
    ctx: Rc<ParserContext>,
    pair: Pair<Rule>,
) -> Result<(), Error<Rule>> {
    let pest_span = pair.as_span();
    let span = Span::from_pest(pest_span, ctx.source());

    // Degenerate case: exactly one bare verb.
    {
        let mut probe = pair.clone().into_inner();
        let first = probe.next();
        if let (Some(first), None) = (first, probe.next()) {
            if first.as_rule() == Rule::verb {
                return super::build_verb(ctx, first);
            }
        }
    }

    // Ephemeral verbs push their op into the block's shared ops vec while
    // being CONSTRUCTED, so an expression inside a layer block would leave
    // ops behind even though its atoms are then rejected.  Refuse before
    // constructing anything, which also keeps the module's suppression rule
    // ("atoms contribute only their predicate") literally true.
    if ctx.get_eph_ops().is_some() {
        return Err(span_err(
            "boolean expressions are not allowed inside layer blocks".to_string(),
            pest_span,
        ));
    }

    let expr = build_expr(&ctx, pair.into_inner())?;
    validate(&expr, pest_span)?;

    let verb = Arc::new(CompoundVerb::new(span, expr));

    // The same rule build_verb applies: a preamble configures the query and
    // cannot select, so an anchor-capable expression is rejected there.
    if ctx.redirects_to_global() && verb.anchor_kind().is_some() {
        return Err(span_err(
            "this expression selects, and a preamble cannot select: its verbs \
             apply to every statement.  Move it into a statement after the \
             preamble, or keep only constraints here"
                .to_string(),
            pest_span,
        ));
    }

    ctx.extend_verb(verb);
    Ok(())
}
