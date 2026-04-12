use crate::command::LabeledStatements;
use crate::execution_state::{DependencyRole, RelationshipType, StatementDependency, StatementDependent};
use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::span::Span;
use crate::verb::{build_verb, DefaultTypeFilter, VerbTag};
use pest::error::Error;
use std::rc::Rc;

use super::Statement;

pub fn build_statement<'a>(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Rc<Statement>, Error<Rule>> {
    let statement_span = Span::from_pest(pair.as_span(), ctx.source());
    let mut iter = pair.into_inner();
    let sub_ctx = ParserContext::derive(ctx, statement_span.clone());
    let mut scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    // Track relationship type BEFORE any verbs run
    let inherited_rel_type = sub_ctx.get_relationship_type();
    // Track inherited default symbol types
    let inherited_default_types = sub_ctx.get_default_symbol_types();

    let mut last_verb_end: Option<usize> = None;
    let mut scope_has_real_children = false;

    for pair in iter.by_ref() {
        match pair.as_rule() {
            Rule::verb => {
                last_verb_end = Some(pair.as_span().end());
                build_verb(sub_ctx.clone(), pair)?;
            }
            Rule::scope => {
                scope_has_real_children = pair.clone().into_inner().next().is_some();

                if !sub_ctx.has_relationship_modifier() {
                    // No explicit has/refs — default to both so parent-child
                    // works regardless of whether the edge is containment or reference.
                    sub_ctx.set_relationship_type_default(RelationshipType::REFS | RelationshipType::HAS);
                }

                // Allow all symbol types for children — empty vec means no type filtering.
                sub_ctx.set_default_symbol_types(vec![]);

                scope = build_scope(sub_ctx.clone(), pair)?;
                break;
            }
            _ => Err(Error::new_from_span(
                pest::error::ErrorVariant::ParsingError {
                    positives: vec![Rule::verb, Rule::scope],
                    negatives: vec![pair.as_rule()],
                },
                pair.as_span(),
            ))?,
        }
    }

    // Restore this statement's own relationship_type (how it relates to its parent).
    // This is the INHERITED value, not the value after verbs (like has/func) modified it.
    // The verb modifications only affect children (via the scope built above).
    sub_ctx.set_relationship_type_default(inherited_rel_type);

    if let Some(pair) = iter.next() {
        return Err(Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Unexpected token after scope: {}", pair),
            },
            pair.as_span(),
        ));
    }

    // If no explicit type selector was used, add a DefaultTypeFilter.
    // None → no inherited default, all types (no filtering needed, skip verb).
    // Some(vec![]) → explicitly set to "all types" (no filtering needed, skip verb).
    // Some(types) → filter by those types.
    if !sub_ctx.has_type_selector() {
        let default_types = inherited_default_types.unwrap_or_default();
        if !default_types.is_empty() {
            sub_ctx.extend_verb(DefaultTypeFilter::new(statement_span.clone(), default_types));
        }
    }

    let mut command = sub_ctx.command(statement_span.clone());
    if scope_has_real_children {
        if let Some(end) = last_verb_end {
            command.set_verb_span(statement_span.sub_span(statement_span.start(), end));
        }
    }
    let relationship_type = sub_ctx.get_relationship_type();
    let unnest = command.has_verb_tag(&VerbTag::Unnest);
    let statement = Statement::new_full(command, scope.clone(), relationship_type, unnest);
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}

pub fn build_empty_statement(ctx: Rc<ParserContext>, span: Span) -> Rc<Statement> {
    let scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    let sub_ctx = ParserContext::derive(ctx.clone(), span.clone());
    // Keep the inherited relationship type (Has or Refs).
    // For has {}, we want to use Has relationship (containment).
    // For {} without has, the parent context already reset to Refs.
    // The relationship type is correctly set by build_statement before calling build_scope.

    // Empty statements have no explicit type selector — add DefaultTypeFilter if needed.
    // None → all types (no filtering needed, skip verb). Some(vec![]) → same, skip.
    let default_types = sub_ctx.get_default_symbol_types().unwrap_or_default();
    if !default_types.is_empty() {
        sub_ctx.extend_verb(DefaultTypeFilter::new(span.clone(), default_types));
    }

    let command = sub_ctx.command(span);
    let relationship_type = sub_ctx.get_relationship_type();
    let statement = Statement::new_with_relationship(command, scope.clone(), relationship_type);
    scope.set_parent(Rc::downgrade(&statement));
    return statement;
}

pub fn init_dependencies(
    statement: Rc<Statement>,
    labeled_statements_map: &LabeledStatements,
) -> Result<(), pest::error::Error<Rule>> {
    let mut state = statement.get_state_mut();
    if let Some(parent) = statement.parent().and_then(|p| p.upgrade()) {
        // Add a parent as a dependent
        state.dependents.push(StatementDependent::new(
            parent.clone(),
            DependencyRole::Parent,
        ));

        // Add ourself as a dependency to the parent
        parent
            .get_state_mut()
            .dependencies
            .push(StatementDependency::new(
                statement.clone(),
                DependencyRole::Parent,
            ));
    }

    for child in statement.children() {
        state.dependents.push(StatementDependent::new(
            child.clone(),
            DependencyRole::Child,
        ));

        // Add ourself as a dependency to the child
        child
            .get_state_mut()
            .dependencies
            .push(StatementDependency::new(
                statement.clone(),
                DependencyRole::Child,
            ));
    }

    // For every user verb, add current statement as dependent to the labeled statements
    for user in statement.command().selectors() {
        let Some(label) = user.get_label() else {
            continue;
        };
        let labeled_statements =
            if let Some(labeled_statements) = labeled_statements_map.get_statements(&label) {
                labeled_statements
            } else {
                return Err(Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Label '{}' not found for user selector", label),
                    },
                    user.span(),
                ));
            };

        for labeled_statement in labeled_statements {
            labeled_statement
                .get_state_mut()
                .dependents
                .push(StatementDependent::new_user(
                    statement.clone(),
                    label.as_str(),
                ));

            // Add ourself as a dependency to the labeled statement
            state.dependencies.push(StatementDependency::new(
                labeled_statement.clone(),
                DependencyRole::User,
            ));
        }
    }

    Ok(())
}
