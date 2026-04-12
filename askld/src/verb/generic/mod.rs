use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule};
use crate::parser_context::{
    SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use index::db_diesel::{CompoundNameMixin, CompositeFilter, LeafNameMixin};
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::vec;

use super::labels::{LabelVerb, UserVerb};
use super::preamble::PreambleVerb;
use super::Verb;

mod filters;
mod modifiers;
mod selectors;

pub use self::filters::{DefaultTypeFilter, DirectOnlyFilter, GenericFilter};
pub use self::selectors::{GenericSelector, NameSelector, UnitVerb};

pub(super) use self::filters::{IgnoreVerb, ProjectFilter};
pub(super) use self::modifiers::{
    AnyModifier, DeriveModifier, HasModifier, IsolatedScope, RefsModifier, UnnestModifier,
};
pub(super) use self::selectors::{ForcedVerb, TypeSelector};

pub(crate) fn build_generic_verb(
    ctx: Rc<crate::parser_context::ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
    let verb_span = Span::from_pest(pair.as_span(), ctx.source());
    let mut pair = pair.into_inner();
    let ident = pair.next().unwrap();
    let mut positional = vec![];
    let mut named = HashMap::new();
    pair.map(|pair| match pair.as_rule() {
        Rule::positional_argument => {
            let arg = PositionalArgument::build(pair)?;
            positional.push(arg.value.0);
            Ok(())
        }
        Rule::named_argument => {
            let arg = NamedArgument::build(pair)?;
            named.insert(arg.name.0, arg.value.0);
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
    let res = match Identifier::build(ident)?.0.as_str() {
        GenericSelector::NAME => GenericSelector::new(verb_span, &positional, &named),
        GenericFilter::NAME => GenericFilter::new(verb_span, &positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(verb_span, &positional, &named),
        ProjectFilter::NAME => ProjectFilter::new(verb_span, &positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(verb_span, &positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(verb_span, &positional, &named),
        LabelVerb::NAME => LabelVerb::new(verb_span, &positional, &named),
        UserVerb::NAME => UserVerb::new(verb_span, &positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(verb_span, &positional, &named),
        HasModifier::NAME => HasModifier::new(verb_span, &positional, &named),
        RefsModifier::NAME => RefsModifier::new(verb_span, &positional, &named),
        DeriveModifier::NAME => DeriveModifier::new(verb_span, &positional, &named),
        UnnestModifier::NAME => UnnestModifier::new(verb_span, &positional, &named),
        AnyModifier::NAME => AnyModifier::new(verb_span, &positional, &named),
        TypeSelector::NAME_FUNCTION => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_FUNCTION)
        }
        TypeSelector::NAME_FILE => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_FILE)
        }
        TypeSelector::NAME_MODULE => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_MODULE)
        }
        TypeSelector::NAME_DIRECTORY => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_DIRECTORY)
        }
        TypeSelector::NAME_TYPE => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_TYPE)
        }
        TypeSelector::NAME_DATA => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_DATA)
        }
        TypeSelector::NAME_MACRO => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_MACRO)
        }
        TypeSelector::NAME_FIELD | TypeSelector::NAME_METHOD => {
            TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_FIELD)
        }
        "_" => Ok(UnitVerb::new(verb_span)),
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
fn name_filter(name: &str) -> CompositeFilter {
    let is_compound = name.contains('.') || name.contains('/') || name.contains(':');
    if is_compound {
        CompositeFilter::leaf(CompoundNameMixin::new(name))
    } else {
        CompositeFilter::leaf(LeafNameMixin::new(name, true))
    }
}
