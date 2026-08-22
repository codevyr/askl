use crate::name_pattern::NamePattern;
use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule, Value};
use crate::parser_context::{
    SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use index::db_diesel::{CompositeFilter, CompoundNameMixin, LeafNameMixin};
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::vec;

use super::labels::{LabelVerb, UserVerb};
use super::preamble::PreambleVerb;
use super::Verb;

mod ephemeral;
mod filters;
mod loc;
mod modifiers;
mod search;
mod selectors;

pub use self::filters::{DefaultTypeFilter, GenericFilter};
pub use self::selectors::{GenericSelector, NameSelector, UnitVerb};

pub(super) use self::ephemeral::LayerVerb;
use self::ephemeral::{EphemeralInstanceVerb, EphemeralRefVerb, EphemeralSymbolVerb};
pub(crate) use self::ephemeral::{EphemeralOps, LabelResolutions};
pub(super) use self::filters::{IgnoreVerb, PackageFilter, ProjectFilter};
pub(super) use self::loc::LocSelector;
pub(super) use self::modifiers::{
    DeriveModifier, HasModifier, IsolatedScope, RefsModifier, UnnestModifier,
};
pub(super) use self::search::SearchSelector;
pub(super) use self::selectors::{ForcedVerb, TypeSelector};

pub(crate) fn build_generic_verb(
    ctx: Rc<crate::parser_context::ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
    let verb_span = Span::from_pest(pair.as_span(), ctx.source());
    let mut pair = pair.into_inner();
    let ident = pair.next().unwrap();
    let mut positional: Vec<Value> = vec![];
    let mut named: HashMap<String, Value> = HashMap::new();
    pair.map(|pair| match pair.as_rule() {
        Rule::positional_argument => {
            let arg = PositionalArgument::build(pair)?;
            positional.push(arg.value);
            Ok(())
        }
        Rule::named_argument => {
            let arg = NamedArgument::build(pair)?;
            named.insert(arg.name.0, arg.value);
            Ok(())
        }
        rule => Err(Error::new_from_span(
            pest::error::ErrorVariant::ParsingError {
                positives: vec![Rule::positional_argument, Rule::named_argument],
                negatives: vec![rule],
            },
            pair.as_span(),
        )),
    })
    .collect::<Result<Vec<_>, _>>()?;

    let span = ident.as_span();
    let ident_name = Identifier::build(ident)?.0;

    // Reject non-ephemeral verbs inside layer blocks.
    if ctx.get_eph_ops().is_some() {
        match ident_name.as_str() {
            EphemeralSymbolVerb::NAME
            | EphemeralInstanceVerb::NAME
            | EphemeralRefVerb::NAME
            | "_" => {}
            other => {
                return Err(Error::new_from_span(
                    CustomError {
                        message: format!(
                            "only ephemeral verbs are allowed inside layer blocks, found '{}'",
                            other
                        ),
                    },
                    span,
                ));
            }
        }
    }

    let res = match ident_name.as_str() {
        GenericSelector::NAME => GenericSelector::new(verb_span, &positional, &named),
        GenericFilter::NAME => GenericFilter::new(verb_span, &positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(verb_span, &positional, &named),
        ProjectFilter::NAME => ProjectFilter::new(verb_span, &positional, &named),
        PackageFilter::NAME => PackageFilter::new(verb_span, &positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(verb_span, &positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(verb_span, &positional, &named),
        LabelVerb::NAME => LabelVerb::new(verb_span, &positional, &named),
        UserVerb::NAME => UserVerb::new(verb_span, &positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(verb_span, &positional, &named),
        HasModifier::NAME => HasModifier::new(verb_span, &positional, &named),
        RefsModifier::NAME => RefsModifier::new(verb_span, &positional, &named),
        DeriveModifier::NAME => DeriveModifier::new(verb_span, &positional, &named),
        UnnestModifier::NAME => UnnestModifier::new(verb_span, &positional, &named),
        TypeSelector::NAME_ANY => TypeSelector::new(verb_span, &positional, &named, None),
        TypeSelector::NAME_FUNCTION => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_FUNCTION))
        }
        TypeSelector::NAME_FILE => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_FILE))
        }
        TypeSelector::NAME_MODULE => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_MODULE))
        }
        TypeSelector::NAME_DIRECTORY => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_DIRECTORY))
        }
        TypeSelector::NAME_TYPE => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_TYPE))
        }
        TypeSelector::NAME_DATA => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_DATA))
        }
        TypeSelector::NAME_MACRO => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_MACRO))
        }
        TypeSelector::NAME_FIELD | TypeSelector::NAME_METHOD => {
            TypeSelector::new(verb_span, &positional, &named, Some(SYMBOL_TYPE_FIELD))
        }
        LocSelector::NAME => LocSelector::new(verb_span, &positional, &named),
        SearchSelector::NAME => SearchSelector::new(verb_span, &positional, &named),
        LayerVerb::NAME => LayerVerb::new(verb_span, &positional, &named),
        EphemeralSymbolVerb::NAME | EphemeralInstanceVerb::NAME | EphemeralRefVerb::NAME => ctx
            .get_eph_ops()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "'{}' can only be used inside a layer {{ }} block",
                    ident_name
                )
            })
            .and_then(|eph_ops| {
                let op = match ident_name.as_str() {
                    EphemeralSymbolVerb::NAME => {
                        EphemeralSymbolVerb::new_op(verb_span.clone(), &positional, &named)?
                    }
                    EphemeralInstanceVerb::NAME => {
                        EphemeralInstanceVerb::new_op(verb_span.clone(), &positional, &named)?
                    }
                    EphemeralRefVerb::NAME => {
                        EphemeralRefVerb::new_op(verb_span.clone(), &positional, &named)?
                    }
                    _ => unreachable!(),
                };
                eph_ops.lock().unwrap().push(op);
                Ok(UnitVerb::new(verb_span))
            }),
        "_" => Ok(UnitVerb::new(verb_span)),
        // An operator in verb position means the line began with it: the
        // grammar continues an expression across a newline only after a
        // TRAILING operator, so say that instead of "unknown verb".  The
        // span error is returned DIRECTLY rather than through the
        // construction wrapper below: nothing failed to build here, and
        // "Failed to create a generic verb" would misdescribe it.
        op @ ("or" | "and") => {
            return Err(Error::new_from_span(
                CustomError {
                    message: format!(
                        "`{op}` starts this line, but a boolean expression continues \
                         across lines only from a trailing operator — move `{op}` to \
                         the end of the previous line"
                    ),
                },
                span,
            ))
        }
        unknown => Err(anyhow::anyhow!("unknown verb : {}", unknown)),
    };

    match res {
        Ok(res) => Ok(res),
        Err(err) => Err(Error::new_from_span(
            CustomError {
                message: format!("Failed to create a generic verb: {}", err),
            },
            span,
        )),
    }
}

/// Returns a name filter for the type-agnostic case (bare selectors).
/// Treats '.' as a separator (code symbol convention).
pub(crate) fn name_filter(pattern: &NamePattern) -> CompositeFilter {
    match pattern {
        NamePattern::Exact(name) => {
            let is_compound = name.contains('.') || name.contains('/') || name.contains(':');
            if is_compound {
                CompositeFilter::leaf(CompoundNameMixin::new(name))
            } else {
                CompositeFilter::leaf(LeafNameMixin::new(name, true))
            }
        }
        NamePattern::Glob(glob) => glob.filter(true, true),
    }
}
